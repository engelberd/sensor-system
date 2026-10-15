use core::fmt;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use sensor_protocol::{
    NodeCapabilities, NodeIdentity, ObservabilityProfile, ObservedConfiguration, RuntimeOperation,
    RuntimeResult, RuntimeResultCode, StreamState,
};

use crate::channel_agent::{ChannelAgentError, ChannelEvent};
use crate::channel_endpoint::{
    ChannelEndpoint, ChannelEndpointError, ChannelEndpointEvent, MonotonicClock,
};
use crate::discovery::{ChannelDiscoveryPlan, DiscoveryTargetId, NodeDiscoveryTarget};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelBootstrapPolicy {
    /// One bounded deadline covers handshake, lifecycle state, and stream state
    /// for a node. Retrying wire requests never extends it.
    pub node_deadline: Duration,
}

impl ChannelBootstrapPolicy {
    pub fn validate(self) -> Result<Self, ChannelBootstrapError> {
        if self.node_deadline.is_zero() {
            Err(ChannelBootstrapError::ZeroNodeDeadline)
        } else {
            Ok(self)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeBootstrapEvidence {
    pub id: DiscoveryTargetId,
    pub identity: NodeIdentity,
    pub capabilities: NodeCapabilities,
    pub configuration: ObservedConfiguration,
    pub runtime: RuntimeResult,
    pub stream: StreamState,
}

/// A bootstrapped physical channel whose endpoint has never released ownership
/// of the port. The endpoint can be moved directly into startup reconciliation
/// and then into `ThreadedChannelRecordingLoop`.
pub struct BootstrappedChannel<P, C> {
    endpoint: ChannelEndpoint<P, C>,
    nodes: Vec<NodeBootstrapEvidence>,
}

impl<P, C> BootstrappedChannel<P, C> {
    pub(crate) fn from_parts(
        endpoint: ChannelEndpoint<P, C>,
        nodes: Vec<NodeBootstrapEvidence>,
    ) -> Self {
        Self { endpoint, nodes }
    }

    pub fn nodes(&self) -> &[NodeBootstrapEvidence] {
        &self.nodes
    }

    pub fn endpoint(&self) -> &ChannelEndpoint<P, C> {
        &self.endpoint
    }

    pub fn endpoint_mut(&mut self) -> &mut ChannelEndpoint<P, C> {
        &mut self.endpoint
    }

    pub fn into_parts(self) -> (ChannelEndpoint<P, C>, Vec<NodeBootstrapEvidence>) {
        (self.endpoint, self.nodes)
    }
}

pub fn bootstrap_channel<P, C>(
    mut endpoint: ChannelEndpoint<P, C>,
    plan: &ChannelDiscoveryPlan,
    policy: ChannelBootstrapPolicy,
) -> Result<BootstrappedChannel<P, C>, ChannelBootstrapError>
where
    P: Read + Write,
    C: MonotonicClock,
{
    let policy = policy.validate()?;
    let mut nodes = Vec::with_capacity(plan.targets().len());
    for target in plan.targets() {
        let deadline = Instant::now() + policy.node_deadline;
        let mut diagnostics = BootstrapDiagnostics::default();

        endpoint
            .agent_mut()
            .submit_handshake(target.requirements())
            .map_err(ChannelBootstrapError::Agent)?;
        let (identity, capabilities) =
            wait_for_handshake(&mut endpoint, *target, deadline, &mut diagnostics)?;
        if capabilities.service_flags & sensor_protocol::ServiceId::Configuration.capability_bit()
            == 0
        {
            return Err(ChannelBootstrapError::MissingConfigurationService { id: target.id() });
        }

        endpoint
            .agent_mut()
            .submit_get_configuration(target.id().node_address)
            .map_err(ChannelBootstrapError::Agent)?;
        let configuration =
            wait_for_configuration(&mut endpoint, target.id(), deadline, &mut diagnostics)?;

        endpoint
            .agent_mut()
            .submit_lifecycle(
                target.id().node_address,
                RuntimeOperation::GetState,
                ObservabilityProfile::Production,
            )
            .map_err(ChannelBootstrapError::Agent)?;
        let runtime =
            wait_for_runtime_state(&mut endpoint, target.id(), deadline, &mut diagnostics)?;

        endpoint
            .agent_mut()
            .submit_stream_state(target.id().node_address)
            .map_err(ChannelBootstrapError::Agent)?;
        let stream = wait_for_stream_state(&mut endpoint, target.id(), deadline, &mut diagnostics)?;

        if stream.boot_epoch != identity.boot_epoch {
            return Err(ChannelBootstrapError::StreamEpochMismatch { id: target.id() });
        }
        if stream.capacity_samples != capabilities.journal_capacity_samples {
            return Err(ChannelBootstrapError::StreamCapacityMismatch {
                id: target.id(),
                advertised: capabilities.journal_capacity_samples,
                observed: stream.capacity_samples,
            });
        }
        nodes.push(NodeBootstrapEvidence {
            id: target.id(),
            identity,
            capabilities,
            configuration,
            runtime,
            stream,
        });
    }
    Ok(BootstrappedChannel::from_parts(endpoint, nodes))
}

fn wait_for_configuration<P, C>(
    endpoint: &mut ChannelEndpoint<P, C>,
    id: DiscoveryTargetId,
    deadline: Instant,
    diagnostics: &mut BootstrapDiagnostics,
) -> Result<ObservedConfiguration, ChannelBootstrapError>
where
    P: Read + Write,
    C: MonotonicClock,
{
    loop {
        ensure_deadline(id, deadline, *diagnostics)?;
        match step(endpoint, id, diagnostics)? {
            ChannelEndpointEvent::Agent(ChannelEvent::ConfigurationComplete {
                node_address,
                outcome: crate::configuration::ConfigurationOutcome::Observed(configuration),
            }) if node_address == id.node_address => return Ok(configuration),
            ChannelEndpointEvent::Agent(ChannelEvent::ConfigurationComplete { .. }) => {
                return Err(ChannelBootstrapError::UnexpectedNode(id));
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct BootstrapDiagnostics {
    decode_errors: u32,
    transaction_timeouts: u32,
}

fn wait_for_handshake<P, C>(
    endpoint: &mut ChannelEndpoint<P, C>,
    target: NodeDiscoveryTarget,
    deadline: Instant,
    diagnostics: &mut BootstrapDiagnostics,
) -> Result<(NodeIdentity, NodeCapabilities), ChannelBootstrapError>
where
    P: Read + Write,
    C: MonotonicClock,
{
    loop {
        ensure_deadline(target.id(), deadline, *diagnostics)?;
        match step(endpoint, target.id(), diagnostics)? {
            ChannelEndpointEvent::Agent(ChannelEvent::HandshakeComplete {
                node_address,
                identity,
                capabilities,
            }) if node_address == target.id().node_address => return Ok((identity, capabilities)),
            ChannelEndpointEvent::Agent(ChannelEvent::HandshakeComplete { .. }) => {
                return Err(ChannelBootstrapError::UnexpectedNode(target.id()));
            }
            _ => {}
        }
    }
}

fn wait_for_runtime_state<P, C>(
    endpoint: &mut ChannelEndpoint<P, C>,
    id: DiscoveryTargetId,
    deadline: Instant,
    diagnostics: &mut BootstrapDiagnostics,
) -> Result<RuntimeResult, ChannelBootstrapError>
where
    P: Read + Write,
    C: MonotonicClock,
{
    loop {
        ensure_deadline(id, deadline, *diagnostics)?;
        match step(endpoint, id, diagnostics)? {
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete {
                node_address,
                result,
            }) if node_address == id.node_address => {
                if result.code != RuntimeResultCode::Ok {
                    return Err(ChannelBootstrapError::RuntimeStateRejected {
                        id,
                        code: result.code,
                    });
                }
                return Ok(result);
            }
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete { .. }) => {
                return Err(ChannelBootstrapError::UnexpectedNode(id));
            }
            _ => {}
        }
    }
}

fn wait_for_stream_state<P, C>(
    endpoint: &mut ChannelEndpoint<P, C>,
    id: DiscoveryTargetId,
    deadline: Instant,
    diagnostics: &mut BootstrapDiagnostics,
) -> Result<StreamState, ChannelBootstrapError>
where
    P: Read + Write,
    C: MonotonicClock,
{
    loop {
        ensure_deadline(id, deadline, *diagnostics)?;
        match step(endpoint, id, diagnostics)? {
            ChannelEndpointEvent::Agent(ChannelEvent::StreamState {
                node_address,
                state,
            }) if node_address == id.node_address => return Ok(state),
            ChannelEndpointEvent::Agent(ChannelEvent::StreamState { .. }) => {
                return Err(ChannelBootstrapError::UnexpectedNode(id));
            }
            _ => {}
        }
    }
}

fn step<P, C>(
    endpoint: &mut ChannelEndpoint<P, C>,
    id: DiscoveryTargetId,
    diagnostics: &mut BootstrapDiagnostics,
) -> Result<ChannelEndpointEvent, ChannelBootstrapError>
where
    P: Read + Write,
    C: MonotonicClock,
{
    let event = endpoint.step().map_err(ChannelBootstrapError::Endpoint)?;
    match event {
        ChannelEndpointEvent::DecodeError(_) => {
            diagnostics.decode_errors = diagnostics.decode_errors.saturating_add(1);
        }
        ChannelEndpointEvent::Agent(
            ChannelEvent::HandshakeTimedOut { .. }
            | ChannelEvent::ConfigurationTimedOut { .. }
            | ChannelEvent::LifecycleTimedOut { .. }
            | ChannelEvent::StreamStateTimedOut { .. },
        ) => {
            diagnostics.transaction_timeouts = diagnostics.transaction_timeouts.saturating_add(1);
        }
        ChannelEndpointEvent::Disconnected => {
            return Err(ChannelBootstrapError::Disconnected(id));
        }
        _ => {}
    }
    Ok(event)
}

fn ensure_deadline(
    id: DiscoveryTargetId,
    deadline: Instant,
    diagnostics: BootstrapDiagnostics,
) -> Result<(), ChannelBootstrapError> {
    if Instant::now() >= deadline {
        Err(ChannelBootstrapError::NodeDeadline {
            id,
            decode_errors: diagnostics.decode_errors,
            transaction_timeouts: diagnostics.transaction_timeouts,
        })
    } else {
        Ok(())
    }
}

#[derive(Debug)]
pub enum ChannelBootstrapError {
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
    RuntimeStateRejected {
        id: DiscoveryTargetId,
        code: RuntimeResultCode,
    },
    StreamEpochMismatch {
        id: DiscoveryTargetId,
    },
    StreamCapacityMismatch {
        id: DiscoveryTargetId,
        advertised: u32,
        observed: u32,
    },
    MissingConfigurationService {
        id: DiscoveryTargetId,
    },
}

impl fmt::Display for ChannelBootstrapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "channel bootstrap error: {self:?}")
    }
}

impl std::error::Error for ChannelBootstrapError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::io;

    use sensor_core::{BootEpoch, SequenceCursor};
    use sensor_protocol::{
        decode_lifecycle_request, encode_get_config_response, encode_lifecycle_response,
        encode_stream_state_success, Adxl355Odr, DecimationFilterProfile, FirmwareVersion, Frame,
        FrameRole, MeasurementConfiguration, NodeCapabilities, NodeIdentity, OutputRateCode,
        RuntimeState, SensorStatusCode, ServiceId, StreamDecoder, CAPABILITIES_SCHEMA_VERSION,
        CONFIGURATION_SERVICE_ID, GET_CAPABILITIES_MESSAGE_ID, GET_CONFIG_MESSAGE_ID,
        GET_IDENTITY_MESSAGE_ID, GET_STREAM_STATE_MESSAGE_ID, HOST_ADDRESS, IDENTITY_SERVICE_ID,
        LOCAL_PROTOCOL_VERSION, MAX_PAYLOAD_BYTES, MAX_WIRE_FRAME_BYTES,
    };
    use serde_json::json;

    use crate::channel_agent::{ChannelAgent, ChannelPolicy};
    use crate::channel_endpoint::SystemMonotonicClock;
    use crate::channel_port::ChannelPort;
    use crate::discovery::{InstallationDiscoveryPlan, REQUIRED_RECORDING_FEATURES};
    use crate::installation_topology::InstallationTopologyProfile;
    use crate::transaction::DurationMicros;

    struct NodeFixture {
        identity: NodeIdentity,
        capabilities: NodeCapabilities,
        configuration: ObservedConfiguration,
        runtime: RuntimeResult,
        stream: StreamState,
    }

    struct BootstrapBus {
        nodes: HashMap<u8, NodeFixture>,
        reads: VecDeque<Vec<u8>>,
        decoder: StreamDecoder,
    }

    impl Read for BootstrapBus {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let Some(bytes) = self.reads.pop_front() else {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no response"));
            };
            let count = bytes.len().min(output.len());
            output[..count].copy_from_slice(&bytes[..count]);
            if count < bytes.len() {
                self.reads.push_front(bytes[count..].to_vec());
            }
            Ok(count)
        }
    }

    impl Write for BootstrapBus {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            for byte in bytes {
                let Some(decoded) = self.decoder.push(*byte) else {
                    continue;
                };
                let request = decoded.map_err(io::Error::other)?;
                let fixture = self
                    .nodes
                    .get(&request.destination)
                    .ok_or_else(|| io::Error::other("unknown node"))?;
                let payload = match (request.service, request.message) {
                    (IDENTITY_SERVICE_ID, GET_IDENTITY_MESSAGE_ID) => {
                        fixture.identity.encode_success().unwrap().to_vec()
                    }
                    (IDENTITY_SERVICE_ID, GET_CAPABILITIES_MESSAGE_ID) => {
                        fixture.capabilities.encode_success().unwrap().to_vec()
                    }
                    (CONFIGURATION_SERVICE_ID, GET_CONFIG_MESSAGE_ID) => {
                        encode_get_config_response(fixture.configuration)
                            .unwrap()
                            .to_vec()
                    }
                    (service, message) if service == ServiceId::Lifecycle as u8 => {
                        let command = decode_lifecycle_request(message, request.payload()).unwrap();
                        assert_eq!(command.operation, RuntimeOperation::GetState);
                        let mut result = fixture.runtime;
                        result.operation_id = command.operation_id;
                        encode_lifecycle_response(result).unwrap().to_vec()
                    }
                    (service, GET_STREAM_STATE_MESSAGE_ID)
                        if service == ServiceId::Measurement as u8 =>
                    {
                        encode_stream_state_success(fixture.stream)
                            .unwrap()
                            .to_vec()
                    }
                    _ => return Err(io::Error::other("unexpected request")),
                };
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

    fn fixture(address: u8) -> NodeFixture {
        let epoch = BootEpoch::new(100 + u64::from(address));
        NodeFixture {
            identity: NodeIdentity {
                hardware_id: sensor_core::HardwareId::from_bytes([address, 1, 2, 3, 4, 5, 6, 7]),
                boot_epoch: epoch,
                node_address: address,
                board_revision: 2,
                firmware: FirmwareVersion {
                    major: 0,
                    minor: 5,
                    patch: 6,
                },
                protocol: LOCAL_PROTOCOL_VERSION,
            },
            capabilities: NodeCapabilities {
                schema_version: CAPABILITIES_SCHEMA_VERSION,
                service_flags: ServiceId::Identity.capability_bit()
                    | ServiceId::Measurement.capability_bit()
                    | ServiceId::Configuration.capability_bit()
                    | ServiceId::Lifecycle.capability_bit()
                    | ServiceId::Time.capability_bit(),
                feature_flags: REQUIRED_RECORDING_FEATURES,
                max_payload_bytes: MAX_PAYLOAD_BYTES as u16,
                journal_capacity_samples: 2048,
                output_rate_mask: sensor_protocol::IMPLEMENTED_OUTPUT_RATE_MASK,
                max_sample_chunk_samples: 48,
            },
            configuration: ObservedConfiguration {
                revision: 1,
                desired: MeasurementConfiguration {
                    output_rate: OutputRateCode::Hz125,
                    range_g: 2,
                    high_pass_corner: 0,
                    filter_profile: DecimationFilterProfile::Balanced,
                    fifo_watermark: 30,
                    offset_x: 0,
                    offset_y: 0,
                    offset_z: 0,
                },
                sensor_odr: Adxl355Odr::Hz125,
                decimation_factor: 1,
            },
            runtime: RuntimeResult {
                operation_id: 0,
                code: RuntimeResultCode::Ok,
                state: RuntimeState::Idle,
                observability: ObservabilityProfile::Production,
                sensor_status: SensorStatusCode::Ok,
            },
            stream: StreamState {
                boot_epoch: epoch,
                retained_from: SequenceCursor::new(50),
                produced: SequenceCursor::new(100),
                released: SequenceCursor::new(40),
                capacity_samples: 2048,
                overwritten_samples: 50,
            },
        }
    }

    fn plan() -> InstallationDiscoveryPlan {
        let topology = InstallationTopologyProfile::from_document(&json!({
            "system":{"name":"test"},
            "channels":[{
                "name":"line-a", "channel_id":1, "enabled":true,
                "transport":{"kind":"serial-rs485", "device_id":"adapter", "endpoint":"/dev/test", "baud_rate":115200},
                "nodes":[
                    {"id":1,"hardware_id":"0101020304050607","board_revision":2,"enabled":true,"output_rate_hz":125},
                    {"id":2,"hardware_id":"0201020304050607","board_revision":2,"enabled":true,"output_rate_hz":125}
                ]
            }]
        }))
        .unwrap();
        InstallationDiscoveryPlan::from_topology(&topology)
    }

    #[test]
    fn keeps_one_endpoint_alive_across_all_bootstrap_phases_and_nodes() {
        let plan = plan();
        let bus = BootstrapBus {
            nodes: [(1, fixture(1)), (2, fixture(2))].into_iter().collect(),
            reads: VecDeque::new(),
            decoder: StreamDecoder::new(),
        };
        let port = ChannelPort::from_io(bus);
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
        let endpoint = ChannelEndpoint::new(agent, port, SystemMonotonicClock::new());

        let prepared = bootstrap_channel(
            endpoint,
            &plan.channels()[0],
            ChannelBootstrapPolicy {
                node_deadline: Duration::from_secs(1),
            },
        )
        .unwrap();

        assert_eq!(prepared.nodes().len(), 2);
        assert_eq!(prepared.nodes()[0].runtime.state, RuntimeState::Idle);
        assert_eq!(
            prepared.nodes()[1].stream.produced,
            SequenceCursor::new(100)
        );
        assert_eq!(prepared.endpoint().agent().queued_work_items(), 0);
        let (_endpoint, evidence) = prepared.into_parts();
        assert_eq!(evidence[0].identity.node_address, 1);
        assert_eq!(evidence[1].identity.node_address, 2);
    }
}
