use core::fmt;
use std::io::{Read, Write};
use std::time::Duration;

use crate::channel_agent::{ChannelAgent, ChannelAgentError, ChannelPolicy};
use crate::channel_bootstrap::{
    bootstrap_channel, BootstrappedChannel, ChannelBootstrapError, ChannelBootstrapPolicy,
};
use crate::channel_endpoint::{
    ChannelEndpoint, MonotonicClock, SystemMonotonicClock, SystemMonotonicClockError,
};
use crate::channel_port::{ChannelPortError, SerialChannelPort, SerialPortConfig};
use crate::discovery::{
    ChannelDiscoveryPlan, DiscoveryError, DiscoveryInventory, InstallationDiscoveryPlan,
};
use crate::installation_topology::TransportBinding;
use crate::transaction::DurationMicros;

/// One channel retained after installation-wide bootstrap.
///
/// The live endpoint is deliberately kept beside the immutable channel
/// identity. No later phase has to reopen or rediscover the physical port.
pub struct BootstrappedInstallationChannel<P, C> {
    channel_id: u8,
    channel_name: String,
    channel: BootstrappedChannel<P, C>,
}

impl<P, C> BootstrappedInstallationChannel<P, C> {
    pub const fn channel_id(&self) -> u8 {
        self.channel_id
    }

    pub fn channel_name(&self) -> &str {
        &self.channel_name
    }

    pub fn channel(&self) -> &BootstrappedChannel<P, C> {
        &self.channel
    }

    pub fn into_channel(self) -> BootstrappedChannel<P, C> {
        self.channel
    }
}

/// Complete, fail-closed startup evidence with continued ownership of every
/// physical channel. This is the input boundary for admission and recovery.
pub struct BootstrappedInstallation<P, C> {
    inventory: DiscoveryInventory,
    channels: Vec<BootstrappedInstallationChannel<P, C>>,
}

impl<P, C> BootstrappedInstallation<P, C> {
    pub fn inventory(&self) -> &DiscoveryInventory {
        &self.inventory
    }

    pub fn channels(&self) -> &[BootstrappedInstallationChannel<P, C>] {
        &self.channels
    }

    pub fn into_parts(
        self,
    ) -> (
        DiscoveryInventory,
        Vec<BootstrappedInstallationChannel<P, C>>,
    ) {
        (self.inventory, self.channels)
    }
}

/// Bootstraps independent channels concurrently, then validates one complete
/// inventory. A channel result is never published on its own as a successful
/// installation; any error drops all retained endpoints.
pub fn bootstrap_installation_with<P, C, E, F>(
    plan: &InstallationDiscoveryPlan,
    runner: F,
) -> Result<BootstrappedInstallation<P, C>, InstallationBootstrapError<E>>
where
    P: Read + Write + Send,
    C: MonotonicClock + Send,
    E: Send,
    F: Fn(&ChannelDiscoveryPlan) -> Result<BootstrappedChannel<P, C>, E> + Sync,
{
    let results = std::thread::scope(|scope| {
        let handles = plan
            .channels()
            .iter()
            .map(|channel_plan| {
                let runner = &runner;
                scope.spawn(move || {
                    runner(channel_plan).map(|channel| BootstrappedInstallationChannel {
                        channel_id: channel_plan.channel_id(),
                        channel_name: channel_plan.channel_name().to_owned(),
                        channel,
                    })
                })
            })
            .collect::<Vec<_>>();

        handles
            .into_iter()
            .zip(plan.channels())
            .map(|(handle, channel_plan)| {
                handle
                    .join()
                    .map_err(|_| InstallationBootstrapError::WorkerPanicked {
                        channel_id: channel_plan.channel_id(),
                    })?
                    .map_err(|source| InstallationBootstrapError::Channel {
                        channel_id: channel_plan.channel_id(),
                        source,
                    })
            })
            .collect::<Result<Vec<_>, InstallationBootstrapError<E>>>()
    })?;

    let mut collector = plan.collector();
    for entry in &results {
        for node in entry.channel.nodes() {
            collector
                .accept(node.id, node.identity, node.capabilities)
                .map_err(InstallationBootstrapError::Inventory)?;
        }
    }
    let inventory = collector
        .finish()
        .map_err(InstallationBootstrapError::Inventory)?;

    Ok(BootstrappedInstallation {
        inventory,
        channels: results,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SerialInstallationBootstrapPolicy {
    pub agent: ChannelPolicy,
    pub quarantine_duration: DurationMicros,
    pub node_deadline: Duration,
    pub serial_io_timeout: Duration,
}

impl SerialInstallationBootstrapPolicy {
    pub fn validate(self) -> Result<Self, SerialChannelBootstrapError> {
        self.agent
            .validate()
            .map_err(SerialChannelBootstrapError::Agent)?;
        if self.node_deadline.is_zero() {
            return Err(SerialChannelBootstrapError::InvalidPolicy(
                "node deadline must be non-zero",
            ));
        }
        if self.serial_io_timeout.is_zero() {
            return Err(SerialChannelBootstrapError::InvalidPolicy(
                "serial I/O timeout must be non-zero",
            ));
        }
        if self.node_deadline < Duration::from_micros(self.agent.transaction_timeout.get()) {
            return Err(SerialChannelBootstrapError::InvalidPolicy(
                "node deadline must cover at least one transaction timeout",
            ));
        }
        Ok(self)
    }
}

pub type SerialBootstrappedInstallation =
    BootstrappedInstallation<Box<dyn serialport::SerialPort>, SystemMonotonicClock>;

/// Production serial bootstrap. The transaction seed is split per physical
/// channel, while the resulting agent and port continue into recording.
pub fn bootstrap_serial_installation(
    plan: &InstallationDiscoveryPlan,
    policy: SerialInstallationBootstrapPolicy,
    session_seed: u32,
) -> Result<SerialBootstrappedInstallation, InstallationBootstrapError<SerialChannelBootstrapError>>
{
    let policy = policy
        .validate()
        .map_err(InstallationBootstrapError::Preparation)?;
    if session_seed == 0 {
        return Err(InstallationBootstrapError::Preparation(
            SerialChannelBootstrapError::ZeroSessionSeed,
        ));
    }

    bootstrap_installation_with(plan, |channel_plan| {
        let TransportBinding::SerialRs485(binding) = channel_plan.transport();
        let mut config = SerialPortConfig::rs485(binding.endpoint(), binding.baud_rate());
        config.io_timeout = policy.serial_io_timeout;
        let port = SerialChannelPort::open(&config).map_err(SerialChannelBootstrapError::Port)?;
        let channel_seed = derive_channel_seed(session_seed, channel_plan.channel_id());
        let agent = ChannelAgent::new(
            channel_seed,
            channel_seed.rotate_left(13),
            policy.quarantine_duration,
            policy.agent,
        )
        .map_err(SerialChannelBootstrapError::Agent)?;
        let clock = SystemMonotonicClock::try_new().map_err(SerialChannelBootstrapError::Clock)?;
        let endpoint = ChannelEndpoint::new(agent, port, clock);
        bootstrap_channel(
            endpoint,
            channel_plan,
            ChannelBootstrapPolicy {
                node_deadline: policy.node_deadline,
            },
        )
        .map_err(SerialChannelBootstrapError::Bootstrap)
    })
}

fn derive_channel_seed(session_seed: u32, channel_id: u8) -> u32 {
    session_seed
        .rotate_left(u32::from(channel_id) % 31)
        .wrapping_add(u32::from(channel_id))
        .max(1)
}

#[derive(Debug)]
pub enum InstallationBootstrapError<E> {
    Preparation(E),
    Channel { channel_id: u8, source: E },
    WorkerPanicked { channel_id: u8 },
    Inventory(DiscoveryError),
}

impl<E: fmt::Display> fmt::Display for InstallationBootstrapError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preparation(source) => {
                write!(formatter, "installation bootstrap rejected: {source}")
            }
            Self::Channel { channel_id, source } => {
                write!(formatter, "channel {channel_id} bootstrap failed: {source}")
            }
            Self::WorkerPanicked { channel_id } => {
                write!(formatter, "channel {channel_id} bootstrap worker panicked")
            }
            Self::Inventory(source) => write!(formatter, "bootstrap inventory rejected: {source}"),
        }
    }
}

impl<E> std::error::Error for InstallationBootstrapError<E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Preparation(source) | Self::Channel { source, .. } => Some(source),
            Self::Inventory(source) => Some(source),
            Self::WorkerPanicked { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum SerialChannelBootstrapError {
    InvalidPolicy(&'static str),
    ZeroSessionSeed,
    Port(ChannelPortError),
    Clock(SystemMonotonicClockError),
    Agent(ChannelAgentError),
    Bootstrap(ChannelBootstrapError),
}

impl fmt::Display for SerialChannelBootstrapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPolicy(message) => {
                write!(formatter, "invalid bootstrap policy: {message}")
            }
            Self::ZeroSessionSeed => formatter.write_str("bootstrap session seed must be non-zero"),
            Self::Port(source) => write!(formatter, "cannot open channel port: {source}"),
            Self::Clock(source) => write!(formatter, "cannot read monotonic clock: {source}"),
            Self::Agent(source) => write!(formatter, "cannot create channel agent: {source}"),
            Self::Bootstrap(source) => write!(formatter, "channel exchange failed: {source}"),
        }
    }
}

impl std::error::Error for SerialChannelBootstrapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Port(source) => Some(source),
            Self::Clock(source) => Some(source),
            Self::Agent(source) => Some(source),
            Self::Bootstrap(source) => Some(source),
            Self::InvalidPolicy(_) | Self::ZeroSessionSeed => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Cursor};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;

    use sensor_core::BootEpoch;
    use sensor_protocol::{
        FirmwareVersion, NodeCapabilities, NodeIdentity, ObservabilityProfile, RuntimeResult,
        RuntimeResultCode, RuntimeState, SensorStatusCode, ServiceId, StreamState,
        CAPABILITIES_SCHEMA_VERSION, LOCAL_PROTOCOL_VERSION, MAX_PAYLOAD_BYTES,
    };
    use serde_json::json;

    use crate::channel_admission::AdmissionPolicy;
    use crate::channel_bootstrap::NodeBootstrapEvidence;
    use crate::channel_runtime::TimeSyncScheduleConfig;
    use crate::channel_startup::ChannelStartupPolicy;
    use crate::discovery::REQUIRED_RECORDING_FEATURES;
    use crate::host_coordinator::plan_host_startup;
    use crate::installation_topology::InstallationTopologyProfile;
    use crate::time_profile::TimePersistenceInstallationProfile;
    use crate::time_wal::HostClockId;

    fn plan(channel_count: u8) -> InstallationDiscoveryPlan {
        let channels = (1..=channel_count)
            .map(|channel_id| {
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
                    "nodes": [{
                        "id": 1,
                        "hardware_id": format!("{:02x}01010101010101", channel_id),
                        "board_revision": 2,
                        "enabled": true,
                        "output_rate_hz": 125
                    }]
                })
            })
            .collect::<Vec<_>>();
        let topology = InstallationTopologyProfile::from_document(&json!({
            "system": {"name": "bootstrap-test"},
            "channels": channels
        }))
        .unwrap();
        InstallationDiscoveryPlan::from_topology(&topology)
    }

    fn channel(
        channel_plan: &ChannelDiscoveryPlan,
    ) -> BootstrappedChannel<Cursor<Vec<u8>>, SystemMonotonicClock> {
        let target = channel_plan.targets()[0];
        let identity = NodeIdentity {
            hardware_id: target.hardware_id(),
            boot_epoch: BootEpoch::new(100 + u64::from(channel_plan.channel_id())),
            node_address: target.id().node_address,
            board_revision: target.board_revision(),
            firmware: FirmwareVersion {
                major: 0,
                minor: 5,
                patch: 6,
            },
            protocol: LOCAL_PROTOCOL_VERSION,
        };
        let capabilities = NodeCapabilities {
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
        };
        let runtime = RuntimeResult {
            operation_id: 7,
            code: RuntimeResultCode::Ok,
            state: RuntimeState::Idle,
            observability: ObservabilityProfile::Production,
            sensor_status: SensorStatusCode::Ok,
        };
        let stream = StreamState {
            boot_epoch: identity.boot_epoch,
            retained_from: sensor_core::SequenceCursor::new(0),
            produced: sensor_core::SequenceCursor::new(10),
            released: sensor_core::SequenceCursor::new(0),
            capacity_samples: 2048,
            overwritten_samples: 0,
        };
        let agent = ChannelAgent::new(
            1,
            2,
            DurationMicros::new(10).unwrap(),
            ChannelPolicy {
                transaction_timeout: DurationMicros::new(100).unwrap(),
                retry_delay: DurationMicros::new(10).unwrap(),
                measurement_attempts_per_slot: 1,
                max_pending_work_items: 1,
            },
        )
        .unwrap();
        let endpoint = ChannelEndpoint::new(
            agent,
            crate::channel_port::ChannelPort::from_io(Cursor::new(Vec::new())),
            SystemMonotonicClock::new(),
        );
        BootstrappedChannel::from_parts(
            endpoint,
            vec![NodeBootstrapEvidence {
                id: target.id(),
                identity,
                capabilities,
                runtime,
                stream,
            }],
        )
    }

    fn startup_policy() -> ChannelStartupPolicy {
        ChannelStartupPolicy {
            admission: AdmissionPolicy {
                maximum_utilization_ppm: 900_000,
                recovery_reserve_ppm: 100_000,
            },
            target_chunk_samples: 32,
            measurement_transaction_time: DurationMicros::new(36_900).unwrap(),
            measurement_slot: DurationMicros::new(40_000).unwrap(),
            maximum_service_interval: DurationMicros::new(500_000).unwrap(),
            retry_base_delay: DurationMicros::new(10_000).unwrap(),
            retry_max_delay: DurationMicros::new(1_000_000).unwrap(),
            retry_attempt_limit: 3,
            offline_probe_interval: DurationMicros::new(5_000_000).unwrap(),
            time_sync_schedule: TimeSyncScheduleConfig::new(
                DurationMicros::new(30_000_000).unwrap(),
                DurationMicros::new(4_000).unwrap(),
                DurationMicros::new(250_000).unwrap(),
            )
            .unwrap(),
            time_sync_transaction_time: DurationMicros::new(4_000).unwrap(),
        }
    }

    #[test]
    fn bootstraps_channels_concurrently_and_retains_complete_inventory() {
        let plan = plan(4);
        let barrier = Barrier::new(4);
        let active = AtomicUsize::new(0);
        let maximum = AtomicUsize::new(0);
        let installation = bootstrap_installation_with(&plan, |channel_plan| {
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            maximum.fetch_max(current, Ordering::SeqCst);
            barrier.wait();
            active.fetch_sub(1, Ordering::SeqCst);
            Ok::<_, io::Error>(channel(channel_plan))
        })
        .unwrap();

        assert_eq!(installation.inventory().nodes().len(), 4);
        assert_eq!(installation.channels().len(), 4);
        assert_eq!(
            installation
                .channels()
                .iter()
                .map(BootstrappedInstallationChannel::channel_id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert!(maximum.load(Ordering::SeqCst) > 1);
    }

    #[test]
    fn one_failed_channel_prevents_partial_installation_success() {
        let plan = plan(4);
        let result = bootstrap_installation_with(&plan, |channel_plan| {
            if channel_plan.channel_id() == 3 {
                Err(io::Error::other("line unavailable"))
            } else {
                Ok(channel(channel_plan))
            }
        });
        assert!(matches!(
            result,
            Err(InstallationBootstrapError::Channel { channel_id: 3, .. })
        ));
    }

    #[test]
    fn complete_inventory_is_revalidated_after_channel_bootstrap() {
        let plan = plan(1);
        let result = bootstrap_installation_with(&plan, |channel_plan| {
            let (endpoint, mut nodes) = channel(channel_plan).into_parts();
            nodes[0].identity.node_address = 2;
            Ok::<_, io::Error>(BootstrappedChannel::from_parts(endpoint, nodes))
        });
        assert!(matches!(
            result,
            Err(InstallationBootstrapError::Inventory(
                DiscoveryError::IdentityMismatch(_)
            ))
        ));
    }

    #[test]
    fn host_blueprint_pairs_live_channels_and_one_global_time_topology() {
        let plan = plan(4);
        let installation = bootstrap_installation_with(&plan, |channel_plan| {
            Ok::<_, io::Error>(channel(channel_plan))
        })
        .unwrap();
        let time = TimePersistenceInstallationProfile::new("time", 16, "time-storage").unwrap();
        let blueprint = plan_host_startup(
            installation,
            startup_policy(),
            &time,
            HostClockId::from_bytes([9; 16]),
        )
        .unwrap();

        assert_eq!(blueprint.channels().len(), 4);
        assert_eq!(blueprint.time().stream_count(), 4);
        for entry in blueprint.channels() {
            assert_eq!(entry.channel_id(), entry.startup().channel_id());
            assert_eq!(entry.channel_name(), entry.startup().channel_name());
            assert_eq!(entry.bootstrapped().nodes().len(), 1);
        }
    }
}
