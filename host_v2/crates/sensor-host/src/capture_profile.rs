use core::fmt;
use std::path::PathBuf;

use sensor_core::{BootEpoch, HardwareId, SequenceCursor};

use crate::capture_repository::CaptureStreamSpec;
use crate::capture_runtime::{CaptureRuntimeConfig, CaptureRuntimeConfigError};
use crate::capture_segments::{inspect_capture_stream_origin, CaptureSegmentError};
use crate::capture_wal::{CaptureWalConfig, CaptureWalHeader};
use crate::capture_window::CaptureWindowPolicy;
use crate::persistence_executor::GroupCommitPolicy;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveredCaptureStream {
    pub hardware_id: HardwareId,
    pub boot_epoch: BootEpoch,
    pub recovered_cursor: SequenceCursor,
    pub first_window_ordinal: u64,
}

/// Parser-independent storage policy supplied by an installation profile.
/// Discovery contributes immutable node identity and the current boot epoch;
/// neither value may be guessed from configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureInstallationProfile {
    root: PathBuf,
    window_policy: CaptureWindowPolicy,
    queue_capacity_records: usize,
    group_commit_policy: GroupCommitPolicy,
    worker_prefix: String,
}

impl CaptureInstallationProfile {
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub fn new(
        root: impl Into<PathBuf>,
        window_policy: CaptureWindowPolicy,
        queue_capacity_records: usize,
        group_commit_policy: GroupCommitPolicy,
        worker_prefix: impl Into<String>,
    ) -> Result<Self, CaptureProfileError> {
        let root = root.into();
        if root.as_os_str().is_empty() {
            return Err(CaptureProfileError::EmptyRoot);
        }
        if queue_capacity_records == 0 {
            return Err(CaptureProfileError::ZeroQueueCapacity);
        }
        let worker_prefix = worker_prefix.into();
        if !valid_component(&worker_prefix) {
            return Err(CaptureProfileError::InvalidWorkerPrefix);
        }
        Ok(Self {
            root,
            window_policy,
            queue_capacity_records,
            group_commit_policy,
            worker_prefix,
        })
    }

    pub fn runtime_config(
        &self,
        channel_name: &str,
        discovered: Vec<DiscoveredCaptureStream>,
    ) -> Result<CaptureRuntimeConfig, CaptureProfileError> {
        if !valid_component(channel_name) {
            return Err(CaptureProfileError::InvalidChannelName);
        }
        if discovered.is_empty() {
            return Err(CaptureProfileError::NoStreams);
        }
        for (index, stream) in discovered.iter().enumerate() {
            if stream.boot_epoch.get() == 0 {
                return Err(CaptureProfileError::ZeroBootEpoch);
            }
            if discovered[..index].iter().any(|existing| {
                existing.hardware_id == stream.hardware_id
                    && existing.boot_epoch == stream.boot_epoch
            }) {
                return Err(CaptureProfileError::DuplicateStream);
            }
        }

        let streams = discovered
            .into_iter()
            .map(|stream| self.stream_spec(channel_name, stream))
            .collect::<Result<Vec<_>, _>>()?;
        CaptureRuntimeConfig::new(
            streams,
            self.queue_capacity_records,
            self.group_commit_policy,
            format!("{}-{channel_name}", self.worker_prefix),
        )
        .map_err(CaptureProfileError::Runtime)
    }

    pub fn stream_spec(
        &self,
        channel_name: &str,
        stream: DiscoveredCaptureStream,
    ) -> Result<CaptureStreamSpec, CaptureProfileError> {
        let directory =
            self.stream_directory(channel_name, stream.hardware_id, stream.boot_epoch)?;
        Ok(CaptureStreamSpec {
            directory,
            config: CaptureWalConfig {
                hardware_id: stream.hardware_id,
                boot_epoch: stream.boot_epoch,
                recovered_cursor: stream.recovered_cursor,
                first_window_ordinal: stream.first_window_ordinal,
                window_policy: self.window_policy,
            },
        })
    }

    pub fn stream_directory(
        &self,
        channel_name: &str,
        hardware_id: HardwareId,
        boot_epoch: BootEpoch,
    ) -> Result<PathBuf, CaptureProfileError> {
        if !valid_component(channel_name) {
            return Err(CaptureProfileError::InvalidChannelName);
        }
        if boot_epoch.get() == 0 {
            return Err(CaptureProfileError::ZeroBootEpoch);
        }
        Ok(self
            .root
            .join(channel_name)
            .join(hardware_id_hex(hardware_id))
            .join(format!("boot-{:016x}", boot_epoch.get())))
    }

    pub fn inspect_stream_origin(
        &self,
        channel_name: &str,
        hardware_id: HardwareId,
        boot_epoch: BootEpoch,
    ) -> Result<Option<CaptureWalHeader>, CaptureProfileError> {
        let directory = self.stream_directory(channel_name, hardware_id, boot_epoch)?;
        inspect_capture_stream_origin(directory).map_err(CaptureProfileError::Segment)
    }
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn hardware_id_hex(hardware_id: HardwareId) -> String {
    let mut output = String::with_capacity(16);
    for byte in hardware_id.bytes() {
        use core::fmt::Write;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

#[derive(Debug)]
pub enum CaptureProfileError {
    EmptyRoot,
    ZeroQueueCapacity,
    InvalidWorkerPrefix,
    InvalidChannelName,
    NoStreams,
    ZeroBootEpoch,
    DuplicateStream,
    Segment(CaptureSegmentError),
    Runtime(CaptureRuntimeConfigError),
}

impl fmt::Display for CaptureProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "capture installation profile error: {self:?}")
    }
}

impl std::error::Error for CaptureProfileError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn stream(id: u8, epoch: u64) -> DiscoveredCaptureStream {
        DiscoveredCaptureStream {
            hardware_id: HardwareId::from_bytes([id, 0, 0, 0, 0, 0, 0, 0]),
            boot_epoch: BootEpoch::new(epoch),
            recovered_cursor: SequenceCursor::ORIGIN,
            first_window_ordinal: 0,
        }
    }

    fn profile() -> CaptureInstallationProfile {
        CaptureInstallationProfile::new(
            "var/recordings-v2",
            CaptureWindowPolicy::new(600_000_000).unwrap(),
            64,
            GroupCommitPolicy::new(8, Duration::from_millis(5)).unwrap(),
            "capture",
        )
        .unwrap()
    }

    #[test]
    fn profile_builds_a_bounded_runtime_only_from_discovered_identity() {
        let config = profile()
            .runtime_config("line-a", vec![stream(1, 17), stream(2, 18)])
            .unwrap();
        assert_eq!(config.stream_count(), 2);
        assert_eq!(config.queue_capacity_records(), 64);
        assert_eq!(config.group_commit_policy().max_records(), 8);
        assert_eq!(config.worker_name(), "capture-line-a");
    }

    #[test]
    fn profile_rejects_ambiguous_or_path_like_input_before_storage_is_opened() {
        assert!(matches!(
            profile().runtime_config("../line-a", vec![stream(1, 17)]),
            Err(CaptureProfileError::InvalidChannelName)
        ));
        assert!(matches!(
            profile().runtime_config("line-a", vec![stream(1, 17), stream(1, 17)]),
            Err(CaptureProfileError::DuplicateStream)
        ));
        assert!(matches!(
            profile().runtime_config("line-a", vec![stream(1, 0)]),
            Err(CaptureProfileError::ZeroBootEpoch)
        ));
    }
}
