use core::fmt;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use sensor_protocol::{
    ObservabilityProfile, RuntimeOperation, RuntimeResult, RuntimeResultCode, RuntimeState,
};

use crate::channel_agent::{ChannelAgentError, ChannelEvent};
use crate::channel_endpoint::{
    ChannelEndpoint, ChannelEndpointError, ChannelEndpointEvent, MonotonicClock,
};
use crate::channel_recovery::RecoveredNode;
use crate::discovery::DiscoveryTargetId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordingActivationPolicy {
    pub node_deadline: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordingShutdownPolicy {
    pub node_deadline: Duration,
}

impl RecordingShutdownPolicy {
    pub fn validate(self) -> Result<Self, RecordingShutdownError> {
        if self.node_deadline.is_zero() {
            Err(RecordingShutdownError::ZeroNodeDeadline)
        } else {
            Ok(self)
        }
    }
}

impl RecordingActivationPolicy {
    pub fn validate(self) -> Result<Self, RecordingActivationError> {
        if self.node_deadline.is_zero() {
            Err(RecordingActivationError::ZeroNodeDeadline)
        } else {
            Ok(self)
        }
    }
}

/// Makes recovered sessions ready for the recording loop without ever
/// touching their cursors. The `RecoveredNode` input makes it impossible to
/// call this phase before Capture replay and stream reconciliation.
pub fn activate_recording_nodes<P, C>(
    endpoint: &mut ChannelEndpoint<P, C>,
    nodes: &mut [RecoveredNode],
    policy: RecordingActivationPolicy,
) -> Result<(), RecordingActivationError>
where
    P: Read + Write,
    C: MonotonicClock,
{
    let policy = policy.validate()?;
    for node in nodes {
        match (node.runtime().state, node.runtime().observability) {
            (RuntimeState::Recording, ObservabilityProfile::Production) => continue,
            (RuntimeState::Idle, _) => {}
            (state, profile) => {
                return Err(RecordingActivationError::ExplicitOperationRequired {
                    id: node.id(),
                    state,
                    profile,
                });
            }
        }

        let result = start_recording_node(endpoint, node.id(), policy)?;
        node.confirm_runtime(result);
    }
    Ok(())
}

/// Starts a known set of quiesced nodes after their retained Measurement tail
/// has been drained. This is used during process takeover: the scheduler and
/// durable sessions already own the cursors, so only Lifecycle state changes.
pub fn start_recording_nodes<P, C>(
    endpoint: &mut ChannelEndpoint<P, C>,
    nodes: &[DiscoveryTargetId],
    policy: RecordingActivationPolicy,
) -> Result<Vec<RuntimeResult>, RecordingActivationError>
where
    P: Read + Write,
    C: MonotonicClock,
{
    let policy = policy.validate()?;
    let mut started = Vec::with_capacity(nodes.len());
    for id in nodes {
        started.push(start_recording_node(endpoint, *id, policy)?);
    }
    Ok(started)
}

fn start_recording_node<P, C>(
    endpoint: &mut ChannelEndpoint<P, C>,
    id: DiscoveryTargetId,
    policy: RecordingActivationPolicy,
) -> Result<RuntimeResult, RecordingActivationError>
where
    P: Read + Write,
    C: MonotonicClock,
{
    endpoint
        .agent_mut()
        .submit_lifecycle(
            id.node_address,
            RuntimeOperation::Start,
            ObservabilityProfile::Production,
        )
        .map_err(RecordingActivationError::Agent)?;
    let result = wait_for_start(endpoint, id, Instant::now() + policy.node_deadline)?;
    if result.code != RuntimeResultCode::Ok
        || result.state != RuntimeState::Recording
        || result.observability != ObservabilityProfile::Production
    {
        return Err(RecordingActivationError::StartRejected { id, result });
    }
    Ok(result)
}

/// Stops nodes only after the caller has quiesced and durably settled its
/// channel work. `Stop` is correlated and retry-safe; an `InProgress` response
/// remains inside the same logical lifecycle operation until the node reaches
/// `Idle` or the bounded deadline expires.
pub fn stop_recording_nodes<P, C>(
    endpoint: &mut ChannelEndpoint<P, C>,
    nodes: &[DiscoveryTargetId],
    policy: RecordingShutdownPolicy,
) -> Result<Vec<RuntimeResult>, RecordingShutdownError>
where
    P: Read + Write,
    C: MonotonicClock,
{
    let policy = policy.validate()?;
    let mut stopped = Vec::with_capacity(nodes.len());
    for id in nodes {
        endpoint
            .agent_mut()
            .submit_lifecycle(
                id.node_address,
                RuntimeOperation::Stop,
                ObservabilityProfile::Production,
            )
            .map_err(RecordingShutdownError::Agent)?;
        let result = wait_for_stop(endpoint, *id, Instant::now() + policy.node_deadline)?;
        if result.code != RuntimeResultCode::Ok || result.state != RuntimeState::Idle {
            return Err(RecordingShutdownError::StopRejected { id: *id, result });
        }
        stopped.push(result);
    }
    Ok(stopped)
}

fn wait_for_stop<P, C>(
    endpoint: &mut ChannelEndpoint<P, C>,
    id: DiscoveryTargetId,
    deadline: Instant,
) -> Result<RuntimeResult, RecordingShutdownError>
where
    P: Read + Write,
    C: MonotonicClock,
{
    let mut decode_errors = 0_u32;
    let mut transaction_timeouts = 0_u32;
    loop {
        if Instant::now() >= deadline {
            return Err(RecordingShutdownError::NodeDeadline {
                id,
                decode_errors,
                transaction_timeouts,
            });
        }
        match endpoint.step().map_err(RecordingShutdownError::Endpoint)? {
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete {
                node_address,
                result,
            }) if node_address == id.node_address => return Ok(result),
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete { .. }) => {
                return Err(RecordingShutdownError::UnexpectedNode(id));
            }
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleTimedOut { .. }) => {
                transaction_timeouts = transaction_timeouts.saturating_add(1);
            }
            ChannelEndpointEvent::DecodeError(_) => {
                decode_errors = decode_errors.saturating_add(1);
            }
            ChannelEndpointEvent::Disconnected => {
                return Err(RecordingShutdownError::Disconnected(id));
            }
            _ => {}
        }
    }
}

fn wait_for_start<P, C>(
    endpoint: &mut ChannelEndpoint<P, C>,
    id: DiscoveryTargetId,
    deadline: Instant,
) -> Result<RuntimeResult, RecordingActivationError>
where
    P: Read + Write,
    C: MonotonicClock,
{
    let mut decode_errors = 0_u32;
    let mut transaction_timeouts = 0_u32;
    loop {
        if Instant::now() >= deadline {
            return Err(RecordingActivationError::NodeDeadline {
                id,
                decode_errors,
                transaction_timeouts,
            });
        }
        match endpoint
            .step()
            .map_err(RecordingActivationError::Endpoint)?
        {
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete {
                node_address,
                result,
            }) if node_address == id.node_address => return Ok(result),
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete { .. }) => {
                return Err(RecordingActivationError::UnexpectedNode(id));
            }
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleTimedOut { .. }) => {
                transaction_timeouts = transaction_timeouts.saturating_add(1);
            }
            ChannelEndpointEvent::DecodeError(_) => {
                decode_errors = decode_errors.saturating_add(1);
            }
            ChannelEndpointEvent::Disconnected => {
                return Err(RecordingActivationError::Disconnected(id));
            }
            _ => {}
        }
    }
}

#[derive(Debug)]
pub enum RecordingActivationError {
    ZeroNodeDeadline,
    Agent(ChannelAgentError),
    Endpoint(ChannelEndpointError),
    NodeDeadline {
        id: DiscoveryTargetId,
        decode_errors: u32,
        transaction_timeouts: u32,
    },
    Disconnected(DiscoveryTargetId),
    UnexpectedNode(DiscoveryTargetId),
    ExplicitOperationRequired {
        id: DiscoveryTargetId,
        state: RuntimeState,
        profile: ObservabilityProfile,
    },
    StartRejected {
        id: DiscoveryTargetId,
        result: RuntimeResult,
    },
}

impl fmt::Display for RecordingActivationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "recording activation error: {self:?}")
    }
}

impl std::error::Error for RecordingActivationError {}

#[derive(Debug)]
pub enum RecordingShutdownError {
    ZeroNodeDeadline,
    Agent(ChannelAgentError),
    Endpoint(ChannelEndpointError),
    NodeDeadline {
        id: DiscoveryTargetId,
        decode_errors: u32,
        transaction_timeouts: u32,
    },
    Disconnected(DiscoveryTargetId),
    UnexpectedNode(DiscoveryTargetId),
    StopRejected {
        id: DiscoveryTargetId,
        result: RuntimeResult,
    },
}

impl fmt::Display for RecordingShutdownError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "recording shutdown error: {self:?}")
    }
}

impl std::error::Error for RecordingShutdownError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io;

    use sensor_core::{BootEpoch, HardwareId, SequenceCursor};
    use sensor_protocol::{
        decode_lifecycle_request, encode_lifecycle_response, Frame, FrameRole, SensorStatusCode,
        StreamDecoder, HOST_ADDRESS, MAX_WIRE_FRAME_BYTES,
    };

    use crate::channel_agent::{ChannelAgent, ChannelPolicy};
    use crate::channel_endpoint::SystemMonotonicClock;
    use crate::channel_port::ChannelPort;
    use crate::node_session::NodeSession;
    use crate::stream_recovery::RecoveryContinuity;
    use crate::transaction::DurationMicros;

    struct LifecycleBus {
        reads: VecDeque<Vec<u8>>,
        decoder: StreamDecoder,
        operations: Vec<RuntimeOperation>,
    }

    impl Read for LifecycleBus {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let Some(bytes) = self.reads.pop_front() else {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no response"));
            };
            let count = bytes.len().min(output.len());
            output[..count].copy_from_slice(&bytes[..count]);
            Ok(count)
        }
    }

    impl Write for LifecycleBus {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            for byte in bytes {
                let Some(decoded) = self.decoder.push(*byte) else {
                    continue;
                };
                let request = decoded.map_err(io::Error::other)?;
                let command = decode_lifecycle_request(request.message, request.payload()).unwrap();
                self.operations.push(command.operation);
                let state = match command.operation {
                    RuntimeOperation::Start => RuntimeState::Recording,
                    RuntimeOperation::Stop => RuntimeState::Idle,
                    _ => return Err(io::Error::other("unexpected lifecycle operation")),
                };
                let payload = encode_lifecycle_response(RuntimeResult {
                    operation_id: command.operation_id,
                    code: RuntimeResultCode::Ok,
                    state,
                    observability: ObservabilityProfile::Production,
                    sensor_status: SensorStatusCode::Ok,
                })
                .unwrap();
                let response = Frame::new(
                    FrameRole::Response,
                    HOST_ADDRESS,
                    request.destination,
                    request.service,
                    request.message,
                    request.transaction_id,
                    &payload,
                )
                .unwrap();
                let mut wire = [0_u8; MAX_WIRE_FRAME_BYTES];
                let length = response.encode(&mut wire);
                self.reads.push_back(wire[..length].to_vec());
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn recovered(address: u8, state: RuntimeState) -> RecoveredNode {
        let hardware_id = HardwareId::from_bytes([address, 1, 2, 3, 4, 5, 6, 7]);
        let epoch = BootEpoch::new(100 + u64::from(address));
        RecoveredNode::from_parts(
            DiscoveryTargetId {
                channel_id: 1,
                node_address: address,
            },
            RuntimeResult {
                operation_id: 1,
                code: RuntimeResultCode::Ok,
                state,
                observability: ObservabilityProfile::Production,
                sensor_status: SensorStatusCode::Ok,
            },
            RecoveryContinuity::Contiguous,
            NodeSession::recovered(
                hardware_id,
                epoch,
                SequenceCursor::new(100),
                SequenceCursor::new(90),
            )
            .unwrap(),
        )
    }

    fn endpoint() -> ChannelEndpoint<LifecycleBus, SystemMonotonicClock> {
        let duration = |value| DurationMicros::new(value).unwrap();
        let agent = ChannelAgent::new(
            10,
            20,
            duration(50),
            ChannelPolicy {
                transaction_timeout: duration(100_000),
                retry_delay: duration(1_000),
                measurement_attempts_per_slot: 2,
                max_pending_work_items: 2,
            },
        )
        .unwrap();
        ChannelEndpoint::new(
            agent,
            ChannelPort::from_io(LifecycleBus {
                reads: VecDeque::new(),
                decoder: StreamDecoder::new(),
                operations: Vec::new(),
            }),
            SystemMonotonicClock::new(),
        )
    }

    #[test]
    fn starts_only_idle_nodes_after_recovery_and_preserves_every_cursor() {
        let mut endpoint = endpoint();
        let mut nodes = [
            recovered(1, RuntimeState::Recording),
            recovered(2, RuntimeState::Idle),
        ];
        let before = nodes
            .iter()
            .map(|node| node.session().cursors())
            .collect::<Vec<_>>();
        activate_recording_nodes(
            &mut endpoint,
            &mut nodes,
            RecordingActivationPolicy {
                node_deadline: Duration::from_secs(1),
            },
        )
        .unwrap();
        assert!(nodes
            .iter()
            .all(|node| node.runtime().state == RuntimeState::Recording));
        assert_eq!(
            nodes
                .iter()
                .map(|node| node.session().cursors())
                .collect::<Vec<_>>(),
            before
        );

        let (_agent, port, _clock) = endpoint.into_parts();
        assert_eq!(port.into_inner().operations, vec![RuntimeOperation::Start]);
    }

    #[test]
    fn maintenance_requires_an_explicit_service_operation() {
        let mut endpoint = endpoint();
        let mut nodes = [recovered(1, RuntimeState::Maintenance)];
        assert!(matches!(
            activate_recording_nodes(
                &mut endpoint,
                &mut nodes,
                RecordingActivationPolicy {
                    node_deadline: Duration::from_secs(1),
                },
            ),
            Err(RecordingActivationError::ExplicitOperationRequired {
                state: RuntimeState::Maintenance,
                ..
            })
        ));
    }

    #[test]
    fn orderly_shutdown_stops_each_node_on_the_owned_channel() {
        let mut endpoint = endpoint();
        let ids = [
            DiscoveryTargetId {
                channel_id: 1,
                node_address: 1,
            },
            DiscoveryTargetId {
                channel_id: 1,
                node_address: 2,
            },
        ];
        let results = stop_recording_nodes(
            &mut endpoint,
            &ids,
            RecordingShutdownPolicy {
                node_deadline: Duration::from_secs(1),
            },
        )
        .unwrap();
        assert!(results.iter().all(|result| {
            result.code == RuntimeResultCode::Ok && result.state == RuntimeState::Idle
        }));

        let (_agent, port, _clock) = endpoint.into_parts();
        assert_eq!(
            port.into_inner().operations,
            vec![RuntimeOperation::Stop, RuntimeOperation::Stop]
        );
    }
}
