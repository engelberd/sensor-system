use core::fmt;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use sensor_protocol::{NodeCapabilities, NodeIdentity};

use crate::channel_agent::{ChannelAgent, ChannelAgentError, ChannelEvent, ChannelPolicy};
use crate::channel_endpoint::{
    ChannelEndpoint, ChannelEndpointError, ChannelEndpointEvent, SystemMonotonicClock,
};
use crate::channel_port::{ChannelPort, ChannelPortError, SerialChannelPort, SerialPortConfig};
use crate::discovery::{
    ChannelDiscoveryPlan, DiscoveryError, DiscoveryInventory, DiscoveryTargetId,
    InstallationDiscoveryPlan,
};
use crate::installation_topology::TransportBinding;
use crate::transaction::DurationMicros;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveryExecutionPolicy {
    pub transaction_timeout: DurationMicros,
    pub retry_delay: DurationMicros,
    pub quarantine_duration: DurationMicros,
    pub node_deadline: Duration,
    pub serial_io_timeout: Duration,
}

impl DiscoveryExecutionPolicy {
    pub fn production() -> Self {
        Self {
            transaction_timeout: DurationMicros::new(250_000)
                .expect("production transaction timeout is non-zero"),
            retry_delay: DurationMicros::new(25_000).expect("production retry delay is non-zero"),
            quarantine_duration: DurationMicros::new(50_000)
                .expect("production quarantine is non-zero"),
            node_deadline: Duration::from_secs(5),
            serial_io_timeout: Duration::from_millis(10),
        }
    }

    pub fn validate(self) -> Result<Self, DiscoveryExecutionError> {
        if self.node_deadline.is_zero() {
            return Err(DiscoveryExecutionError::InvalidPolicy(
                "node deadline must be non-zero",
            ));
        }
        if self.serial_io_timeout.is_zero() {
            return Err(DiscoveryExecutionError::InvalidPolicy(
                "serial I/O timeout must be non-zero",
            ));
        }
        if self.node_deadline < Duration::from_micros(self.transaction_timeout.get()) {
            return Err(DiscoveryExecutionError::InvalidPolicy(
                "node deadline must cover at least one transaction timeout",
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DiscoveredHandshake {
    id: DiscoveryTargetId,
    identity: NodeIdentity,
    capabilities: NodeCapabilities,
}

#[derive(Debug)]
struct ChannelDiscoveryResult {
    handshakes: Vec<DiscoveredHandshake>,
}

/// Executes discovery of all active channels concurrently.
///
/// Each worker opens and exclusively owns one channel endpoint. Targets on the
/// same half-duplex channel are handshaken in address order, while independent
/// channels may progress in parallel. No partial inventory is returned.
pub fn execute_installation_discovery(
    plan: &InstallationDiscoveryPlan,
    policy: DiscoveryExecutionPolicy,
    session_seed: u32,
) -> Result<DiscoveryInventory, DiscoveryExecutionError> {
    execute_installation_with(plan, policy, session_seed, |channel, policy, seed| {
        let TransportBinding::SerialRs485(binding) = channel.transport();
        let mut config = SerialPortConfig::rs485(binding.endpoint(), binding.baud_rate());
        config.io_timeout = policy.serial_io_timeout;
        let port =
            SerialChannelPort::open(&config).map_err(|source| DiscoveryExecutionError::Port {
                channel_id: channel.channel_id(),
                source,
            })?;
        execute_channel_discovery(channel, port, policy, seed)
    })
}

fn execute_installation_with<F>(
    plan: &InstallationDiscoveryPlan,
    policy: DiscoveryExecutionPolicy,
    session_seed: u32,
    runner: F,
) -> Result<DiscoveryInventory, DiscoveryExecutionError>
where
    F: Fn(
            &ChannelDiscoveryPlan,
            DiscoveryExecutionPolicy,
            u32,
        ) -> Result<ChannelDiscoveryResult, DiscoveryExecutionError>
        + Sync,
{
    let policy = policy.validate()?;
    if session_seed == 0 {
        return Err(DiscoveryExecutionError::ZeroSessionSeed);
    }

    let results = std::thread::scope(|scope| {
        let handles = plan
            .channels()
            .iter()
            .map(|channel| {
                let channel_seed = derive_channel_seed(session_seed, channel.channel_id());
                let runner = &runner;
                scope.spawn(move || runner(channel, policy, channel_seed))
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| DiscoveryExecutionError::WorkerPanicked)?
            })
            .collect::<Result<Vec<_>, DiscoveryExecutionError>>()
    })?;

    let mut collector = plan.collector();
    for channel in results {
        for handshake in channel.handshakes {
            collector
                .accept(handshake.id, handshake.identity, handshake.capabilities)
                .map_err(DiscoveryExecutionError::Inventory)?;
        }
    }
    collector
        .finish()
        .map_err(DiscoveryExecutionError::Inventory)
}

fn execute_channel_discovery<P>(
    plan: &ChannelDiscoveryPlan,
    port: ChannelPort<P>,
    policy: DiscoveryExecutionPolicy,
    session_seed: u32,
) -> Result<ChannelDiscoveryResult, DiscoveryExecutionError>
where
    P: Read + Write,
{
    let agent_policy = ChannelPolicy {
        transaction_timeout: policy.transaction_timeout,
        retry_delay: policy.retry_delay,
        measurement_attempts_per_slot: 1,
        max_pending_work_items: 1,
    };
    let agent = ChannelAgent::new(
        session_seed,
        session_seed.rotate_left(13),
        policy.quarantine_duration,
        agent_policy,
    )
    .map_err(|source| DiscoveryExecutionError::Agent {
        channel_id: plan.channel_id(),
        source,
    })?;
    let mut endpoint = ChannelEndpoint::new(agent, port, SystemMonotonicClock::new());
    let mut handshakes = Vec::with_capacity(plan.targets().len());

    for target in plan.targets() {
        endpoint
            .agent_mut()
            .submit_handshake(target.requirements())
            .map_err(|source| DiscoveryExecutionError::Agent {
                channel_id: plan.channel_id(),
                source,
            })?;
        let deadline = Instant::now() + policy.node_deadline;
        let mut decode_errors = 0_u32;
        let mut transaction_timeouts = 0_u32;
        loop {
            if Instant::now() >= deadline {
                return Err(DiscoveryExecutionError::NodeDeadline {
                    id: target.id(),
                    decode_errors,
                    transaction_timeouts,
                });
            }
            match endpoint
                .step()
                .map_err(|source| DiscoveryExecutionError::Endpoint {
                    channel_id: plan.channel_id(),
                    source,
                })? {
                ChannelEndpointEvent::Agent(ChannelEvent::HandshakeComplete {
                    node_address,
                    identity,
                    capabilities,
                }) if node_address == target.id().node_address => {
                    handshakes.push(DiscoveredHandshake {
                        id: target.id(),
                        identity,
                        capabilities,
                    });
                    break;
                }
                ChannelEndpointEvent::Agent(ChannelEvent::HandshakeComplete { .. }) => {
                    return Err(DiscoveryExecutionError::UnexpectedHandshake {
                        expected: target.id(),
                    });
                }
                ChannelEndpointEvent::Agent(ChannelEvent::HandshakeTimedOut { .. }) => {
                    transaction_timeouts = transaction_timeouts.saturating_add(1);
                }
                ChannelEndpointEvent::DecodeError(_) => {
                    decode_errors = decode_errors.saturating_add(1);
                }
                ChannelEndpointEvent::Disconnected => {
                    return Err(DiscoveryExecutionError::Disconnected {
                        channel_id: plan.channel_id(),
                    });
                }
                _ => {}
            }
        }
    }

    Ok(ChannelDiscoveryResult { handshakes })
}

fn derive_channel_seed(session_seed: u32, channel_id: u8) -> u32 {
    let value = session_seed
        .rotate_left(u32::from(channel_id) % 31)
        .wrapping_add(u32::from(channel_id));
    value.max(1)
}

#[derive(Debug)]
pub enum DiscoveryExecutionError {
    InvalidPolicy(&'static str),
    ZeroSessionSeed,
    Port {
        channel_id: u8,
        source: ChannelPortError,
    },
    Agent {
        channel_id: u8,
        source: ChannelAgentError,
    },
    Endpoint {
        channel_id: u8,
        source: ChannelEndpointError,
    },
    NodeDeadline {
        id: DiscoveryTargetId,
        decode_errors: u32,
        transaction_timeouts: u32,
    },
    UnexpectedHandshake {
        expected: DiscoveryTargetId,
    },
    Disconnected {
        channel_id: u8,
    },
    WorkerPanicked,
    Inventory(DiscoveryError),
}

impl fmt::Display for DiscoveryExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPolicy(message) => write!(formatter, "invalid discovery policy: {message}"),
            Self::ZeroSessionSeed => formatter.write_str("discovery session seed must be non-zero"),
            Self::Port { channel_id, source } => {
                write!(formatter, "channel {channel_id} port failed: {source}")
            }
            Self::Agent { channel_id, source } => {
                write!(formatter, "channel {channel_id} agent failed: {source}")
            }
            Self::Endpoint { channel_id, source } => {
                write!(formatter, "channel {channel_id} endpoint failed: {source}")
            }
            Self::NodeDeadline { id, decode_errors, transaction_timeouts } => write!(
                formatter,
                "discovery deadline expired for channel {} node {} (decode_errors={}, transaction_timeouts={})",
                id.channel_id, id.node_address, decode_errors, transaction_timeouts
            ),
            Self::UnexpectedHandshake { expected } => write!(
                formatter,
                "channel {} returned a handshake for a node other than {}",
                expected.channel_id, expected.node_address
            ),
            Self::Disconnected { channel_id } => {
                write!(formatter, "channel {channel_id} disconnected during discovery")
            }
            Self::WorkerPanicked => formatter.write_str("discovery worker panicked"),
            Self::Inventory(error) => write!(formatter, "discovery inventory rejected: {error}"),
        }
    }
}

impl std::error::Error for DiscoveryExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Port { source, .. } => Some(source),
            Self::Agent { source, .. } => Some(source),
            Self::Endpoint { source, .. } => Some(source),
            Self::Inventory(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;

    use sensor_core::BootEpoch;
    use sensor_protocol::{
        FirmwareVersion, Frame, FrameRole, NodeCapabilities, NodeIdentity, ServiceId,
        StreamDecoder, CAPABILITIES_SCHEMA_VERSION, GET_CAPABILITIES_MESSAGE_ID,
        GET_IDENTITY_MESSAGE_ID, HOST_ADDRESS, IDENTITY_SERVICE_ID, LOCAL_PROTOCOL_VERSION,
        MAX_PAYLOAD_BYTES, MAX_WIRE_FRAME_BYTES,
    };
    use serde_json::json;

    use crate::discovery::REQUIRED_RECORDING_FEATURES;
    use crate::installation_topology::InstallationTopologyProfile;

    struct ReactiveNodeBus {
        nodes: HashMap<u8, (NodeIdentity, NodeCapabilities)>,
        reads: VecDeque<Vec<u8>>,
        decoder: StreamDecoder,
    }

    impl ReactiveNodeBus {
        fn new(nodes: impl IntoIterator<Item = (NodeIdentity, NodeCapabilities)>) -> Self {
            Self {
                nodes: nodes
                    .into_iter()
                    .map(|pair| (pair.0.node_address, pair))
                    .collect(),
                reads: VecDeque::new(),
                decoder: StreamDecoder::new(),
            }
        }
    }

    impl Read for ReactiveNodeBus {
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

    impl Write for ReactiveNodeBus {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            for byte in bytes {
                let Some(decoded) = self.decoder.push(*byte) else {
                    continue;
                };
                let request = decoded.map_err(io::Error::other)?;
                let Some((identity, capabilities)) = self.nodes.get(&request.destination) else {
                    continue;
                };
                let payload = match (request.service, request.message) {
                    (IDENTITY_SERVICE_ID, GET_IDENTITY_MESSAGE_ID) => identity
                        .encode_success()
                        .map_err(io::Error::other)?
                        .to_vec(),
                    (IDENTITY_SERVICE_ID, GET_CAPABILITIES_MESSAGE_ID) => capabilities
                        .encode_success()
                        .map_err(io::Error::other)?
                        .to_vec(),
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
                .map_err(io::Error::other)?;
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

    fn topology(channel_count: u8, nodes_per_channel: u8) -> InstallationTopologyProfile {
        let channels = (1..=channel_count)
            .map(|channel_id| {
                let nodes = (1..=nodes_per_channel)
                    .map(|node_address| {
                        let hardware =
                            format!("{:02x}{:02x}010101010101", channel_id, node_address);
                        json!({
                            "id": node_address,
                            "hardware_id": hardware,
                            "board_revision": 2,
                            "enabled": true,
                            "output_rate_hz": 125
                        })
                    })
                    .collect::<Vec<_>>();
                json!({
                    "name": format!("line-{channel_id}"),
                    "channel_id": channel_id,
                    "enabled": true,
                    "transport": {
                        "kind": "serial-rs485",
                        "device_id": "adapter-1",
                        "endpoint": format!("/dev/line-{channel_id}"),
                        "baud_rate": 115200
                    },
                    "nodes": nodes
                })
            })
            .collect::<Vec<_>>();
        InstallationTopologyProfile::from_document(&json!({
            "system": {"name": "test-host"},
            "channels": channels
        }))
        .unwrap()
    }

    fn identity(target: crate::discovery::NodeDiscoveryTarget) -> NodeIdentity {
        NodeIdentity {
            hardware_id: target.hardware_id(),
            boot_epoch: BootEpoch::new(100 + u64::from(target.id().node_address)),
            node_address: target.id().node_address,
            board_revision: target.board_revision(),
            firmware: FirmwareVersion {
                major: 0,
                minor: 5,
                patch: 6,
            },
            protocol: LOCAL_PROTOCOL_VERSION,
        }
    }

    fn capabilities(target: crate::discovery::NodeDiscoveryTarget) -> NodeCapabilities {
        NodeCapabilities {
            schema_version: CAPABILITIES_SCHEMA_VERSION,
            service_flags: ServiceId::Identity.capability_bit()
                | ServiceId::Measurement.capability_bit()
                | ServiceId::Lifecycle.capability_bit()
                | ServiceId::Time.capability_bit(),
            feature_flags: REQUIRED_RECORDING_FEATURES,
            max_payload_bytes: MAX_PAYLOAD_BYTES as u16,
            journal_capacity_samples: 2048,
            output_rate_mask: target.output_rate().capability_bit(),
            max_sample_chunk_samples: 48,
        }
    }

    fn test_policy() -> DiscoveryExecutionPolicy {
        DiscoveryExecutionPolicy {
            transaction_timeout: DurationMicros::new(2_000).unwrap(),
            retry_delay: DurationMicros::new(500).unwrap(),
            quarantine_duration: DurationMicros::new(500).unwrap(),
            node_deadline: Duration::from_millis(20),
            serial_io_timeout: Duration::from_millis(1),
        }
    }

    #[test]
    fn one_channel_discovers_multiple_nodes_through_the_real_wire_endpoint() {
        let topology = topology(1, 2);
        let plan = InstallationDiscoveryPlan::from_topology(&topology);
        let channel = &plan.channels()[0];
        let nodes = channel
            .targets()
            .iter()
            .copied()
            .map(|target| (identity(target), capabilities(target)));
        let bus = ReactiveNodeBus::new(nodes);
        let result =
            execute_channel_discovery(channel, ChannelPort::from_io(bus), test_policy(), 17)
                .unwrap();
        assert_eq!(result.handshakes.len(), 2);
        assert_eq!(result.handshakes[0].id.node_address, 1);
        assert_eq!(result.handshakes[1].id.node_address, 2);
    }

    #[test]
    fn installation_runs_channels_in_parallel_and_returns_no_partial_inventory() {
        let topology = topology(4, 1);
        let plan = InstallationDiscoveryPlan::from_topology(&topology);
        let active = AtomicUsize::new(0);
        let maximum = AtomicUsize::new(0);
        let barrier = Barrier::new(4);
        let inventory = execute_installation_with(&plan, test_policy(), 9, |channel, _, _| {
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            maximum.fetch_max(current, Ordering::SeqCst);
            barrier.wait();
            let target = channel.targets()[0];
            active.fetch_sub(1, Ordering::SeqCst);
            Ok(ChannelDiscoveryResult {
                handshakes: vec![DiscoveredHandshake {
                    id: target.id(),
                    identity: identity(target),
                    capabilities: capabilities(target),
                }],
            })
        })
        .unwrap();
        assert_eq!(inventory.nodes().len(), 4);
        assert!(maximum.load(Ordering::SeqCst) > 1);

        let failed = execute_installation_with(&plan, test_policy(), 9, |channel, _, _| {
            if channel.channel_id() == 3 {
                return Err(DiscoveryExecutionError::Disconnected { channel_id: 3 });
            }
            let target = channel.targets()[0];
            Ok(ChannelDiscoveryResult {
                handshakes: vec![DiscoveredHandshake {
                    id: target.id(),
                    identity: identity(target),
                    capabilities: capabilities(target),
                }],
            })
        });
        assert!(matches!(
            failed,
            Err(DiscoveryExecutionError::Disconnected { channel_id: 3 })
        ));
    }

    #[test]
    fn silent_node_expires_at_the_bounded_node_deadline() {
        let topology = topology(1, 1);
        let plan = InstallationDiscoveryPlan::from_topology(&topology);
        let bus = ReactiveNodeBus::new(Vec::new());
        assert!(matches!(
            execute_channel_discovery(
                &plan.channels()[0],
                ChannelPort::from_io(bus),
                test_policy(),
                1
            ),
            Err(DiscoveryExecutionError::NodeDeadline { .. })
        ));
    }
}
