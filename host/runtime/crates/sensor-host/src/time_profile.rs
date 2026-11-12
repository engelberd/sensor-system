use core::fmt;
use std::path::PathBuf;

use sensor_core::{BootEpoch, HardwareId};
use sensor_protocol::{BROADCAST_ADDRESS, HOST_ADDRESS};

use crate::channel_runtime::{ChannelRuntimeError, TimeSyncScheduleConfig};
use crate::time_repository::TimeStreamSpec;
use crate::time_runtime::{TimeRuntimeConfig, TimeRuntimeConfigError};
use crate::time_wal::{HostClockId, HostClockIdError, TimeWalConfig};
use crate::transaction::DurationMicros;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredTimeStream {
    pub channel_name: String,
    pub hardware_id: HardwareId,
    pub node_address: u8,
    pub boot_epoch: BootEpoch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimePersistenceInstallationProfile {
    root: PathBuf,
    queue_capacity_records: usize,
    worker_name: String,
}

impl TimePersistenceInstallationProfile {
    pub fn new(
        root: impl Into<PathBuf>,
        queue_capacity_records: usize,
        worker_name: impl Into<String>,
    ) -> Result<Self, TimeProfileError> {
        let root = root.into();
        if root.as_os_str().is_empty() {
            return Err(TimeProfileError::EmptyRoot);
        }
        if queue_capacity_records == 0 {
            return Err(TimeProfileError::ZeroQueueCapacity);
        }
        let worker_name = worker_name.into();
        if !valid_component(&worker_name) {
            return Err(TimeProfileError::InvalidWorkerName);
        }
        Ok(Self {
            root,
            queue_capacity_records,
            worker_name,
        })
    }

    pub fn runtime_config(
        &self,
        host_clock_id: HostClockId,
        discovered: Vec<DiscoveredTimeStream>,
    ) -> Result<TimeRuntimeConfig, TimeProfileError> {
        if host_clock_id.is_zero() {
            return Err(TimeProfileError::ZeroHostClockId);
        }
        if discovered.is_empty() {
            return Err(TimeProfileError::NoStreams);
        }
        for (index, stream) in discovered.iter().enumerate() {
            if !valid_component(&stream.channel_name) {
                return Err(TimeProfileError::InvalidChannelName);
            }
            if stream.node_address == HOST_ADDRESS || stream.node_address == BROADCAST_ADDRESS {
                return Err(TimeProfileError::InvalidNodeAddress(stream.node_address));
            }
            if stream.boot_epoch.get() == 0 {
                return Err(TimeProfileError::ZeroBootEpoch);
            }
            if discovered[..index].iter().any(|existing| {
                existing.hardware_id == stream.hardware_id
                    && existing.node_address == stream.node_address
                    && existing.boot_epoch == stream.boot_epoch
            }) {
                return Err(TimeProfileError::DuplicateStream);
            }
        }

        let streams = discovered
            .into_iter()
            .map(|stream| self.stream_spec(host_clock_id, stream))
            .collect::<Result<Vec<_>, _>>()?;
        TimeRuntimeConfig::new(
            streams,
            self.queue_capacity_records,
            self.worker_name.clone(),
        )
        .map_err(TimeProfileError::Runtime)
    }

    pub fn stream_spec(
        &self,
        host_clock_id: HostClockId,
        stream: DiscoveredTimeStream,
    ) -> Result<TimeStreamSpec, TimeProfileError> {
        if host_clock_id.is_zero() {
            return Err(TimeProfileError::ZeroHostClockId);
        }
        if !valid_component(&stream.channel_name) {
            return Err(TimeProfileError::InvalidChannelName);
        }
        if stream.node_address == HOST_ADDRESS || stream.node_address == BROADCAST_ADDRESS {
            return Err(TimeProfileError::InvalidNodeAddress(stream.node_address));
        }
        if stream.boot_epoch.get() == 0 {
            return Err(TimeProfileError::ZeroBootEpoch);
        }
        let host_clock_hex = bytes_hex(&host_clock_id.bytes());
        Ok(TimeStreamSpec {
            path: self
                .root
                .join(&stream.channel_name)
                .join(bytes_hex(&stream.hardware_id.bytes()))
                .join(format!("host-{host_clock_hex}"))
                .join(format!(
                    "boot-{:016x}-node-{:02x}.wal",
                    stream.boot_epoch.get(),
                    stream.node_address
                )),
            config: TimeWalConfig {
                hardware_id: stream.hardware_id,
                node_address: stream.node_address,
                boot_epoch: stream.boot_epoch,
                host_clock_id,
            },
        })
    }

    pub fn linux_runtime_config(
        &self,
        discovered: Vec<DiscoveredTimeStream>,
    ) -> Result<TimeRuntimeConfig, TimeProfileError> {
        let host_clock_id =
            HostClockId::read_current_linux_boot_id().map_err(TimeProfileError::HostClockId)?;
        self.runtime_config(host_clock_id, discovered)
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub const fn queue_capacity_records(&self) -> usize {
        self.queue_capacity_records
    }

    pub fn worker_name(&self) -> &str {
        &self.worker_name
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeSyncInstallationPolicy {
    schedule: TimeSyncScheduleConfig,
}

impl TimeSyncInstallationPolicy {
    pub fn new(
        interval: DurationMicros,
        estimated_slot: DurationMicros,
        retry_delay: DurationMicros,
    ) -> Result<Self, TimeProfileError> {
        let schedule = TimeSyncScheduleConfig::new(interval, estimated_slot, retry_delay)
            .map_err(TimeProfileError::Schedule)?;
        Ok(Self { schedule })
    }

    pub const fn schedule(self) -> TimeSyncScheduleConfig {
        self.schedule
    }
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn bytes_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use core::fmt::Write;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

#[derive(Debug)]
pub enum TimeProfileError {
    EmptyRoot,
    ZeroQueueCapacity,
    InvalidWorkerName,
    NoStreams,
    InvalidChannelName,
    InvalidNodeAddress(u8),
    ZeroBootEpoch,
    ZeroHostClockId,
    DuplicateStream,
    HostClockId(HostClockIdError),
    Schedule(ChannelRuntimeError),
    Runtime(TimeRuntimeConfigError),
}

impl fmt::Display for TimeProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "time installation profile error: {self:?}")
    }
}

impl std::error::Error for TimeProfileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HostClockId(error) => Some(error),
            Self::Schedule(error) => Some(error),
            Self::Runtime(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(channel: &str, id: u8, address: u8, epoch: u64) -> DiscoveredTimeStream {
        DiscoveredTimeStream {
            channel_name: channel.into(),
            hardware_id: HardwareId::from_bytes([id; 8]),
            node_address: address,
            boot_epoch: BootEpoch::new(epoch),
        }
    }

    #[test]
    fn profile_builds_one_global_bounded_runtime_from_discovery() {
        let profile =
            TimePersistenceInstallationProfile::new("var/recordings/time", 64, "time-storage")
                .unwrap();
        let config = profile
            .runtime_config(
                HostClockId::from_bytes([9; 16]),
                vec![stream("line-a", 1, 1, 7), stream("line-b", 2, 1, 8)],
            )
            .unwrap();
        assert_eq!(config.stream_count(), 2);
        assert_eq!(config.queue_capacity_records(), 64);
        assert_eq!(config.worker_name(), "time-storage");
    }

    #[test]
    fn discovery_identity_is_validated_before_runtime_start() {
        let profile = TimePersistenceInstallationProfile::new("time", 1, "worker").unwrap();
        assert!(matches!(
            profile.runtime_config(
                HostClockId::from_bytes([0; 16]),
                vec![stream("line-a", 1, 1, 7)]
            ),
            Err(TimeProfileError::ZeroHostClockId)
        ));
        assert!(matches!(
            profile.runtime_config(
                HostClockId::from_bytes([1; 16]),
                vec![stream("../line", 1, 1, 7)]
            ),
            Err(TimeProfileError::InvalidChannelName)
        ));
        assert!(matches!(
            profile.runtime_config(
                HostClockId::from_bytes([1; 16]),
                vec![stream("line-a", 1, 1, 7), stream("line-b", 1, 1, 7)]
            ),
            Err(TimeProfileError::DuplicateStream)
        ));
    }

    #[test]
    fn synchronization_policy_rejects_a_slot_larger_than_interval() {
        assert!(matches!(
            TimeSyncInstallationPolicy::new(
                DurationMicros::new(10).unwrap(),
                DurationMicros::new(11).unwrap(),
                DurationMicros::new(1).unwrap()
            ),
            Err(TimeProfileError::Schedule(
                ChannelRuntimeError::TimeSyncSlotExceedsInterval
            ))
        ));
    }
}
