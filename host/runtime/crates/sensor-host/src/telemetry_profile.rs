use core::fmt;
use std::path::{Path, PathBuf};

use sensor_core::{BootEpoch, HardwareId};
use sensor_protocol::{BROADCAST_ADDRESS, HOST_ADDRESS};

use crate::telemetry_repository::TelemetryStreamSpec;
use crate::telemetry_runtime::{TelemetryRuntimeConfig, TelemetryRuntimeConfigError};
use crate::telemetry_wal::TelemetryWalConfig;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredTelemetryStream {
    pub channel_name: String,
    pub hardware_id: HardwareId,
    pub node_address: u8,
    pub boot_epoch: BootEpoch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelemetryPersistenceInstallationProfile {
    root: PathBuf,
    queue_capacity_records: usize,
    worker_name: String,
}

impl TelemetryPersistenceInstallationProfile {
    pub fn new(
        root: impl Into<PathBuf>,
        queue_capacity_records: usize,
        worker_name: impl Into<String>,
    ) -> Result<Self, TelemetryProfileError> {
        let root = root.into();
        if root.as_os_str().is_empty() {
            return Err(TelemetryProfileError::EmptyRoot);
        }
        if queue_capacity_records == 0 {
            return Err(TelemetryProfileError::ZeroQueueCapacity);
        }
        let worker_name = worker_name.into();
        if !valid_component(&worker_name) {
            return Err(TelemetryProfileError::InvalidWorkerName);
        }
        Ok(Self {
            root,
            queue_capacity_records,
            worker_name,
        })
    }

    pub fn runtime_config(
        &self,
        discovered: Vec<DiscoveredTelemetryStream>,
    ) -> Result<Option<TelemetryRuntimeConfig>, TelemetryProfileError> {
        if discovered.is_empty() {
            return Ok(None);
        }
        for (index, stream) in discovered.iter().enumerate() {
            validate_stream(stream)?;
            if discovered[..index].iter().any(|existing| {
                existing.hardware_id == stream.hardware_id
                    && existing.node_address == stream.node_address
                    && existing.boot_epoch == stream.boot_epoch
            }) {
                return Err(TelemetryProfileError::DuplicateStream);
            }
        }
        let streams = discovered
            .into_iter()
            .map(|stream| self.stream_spec(stream))
            .collect::<Result<Vec<_>, _>>()?;
        TelemetryRuntimeConfig::new(
            streams,
            self.queue_capacity_records,
            self.worker_name.clone(),
        )
        .map(Some)
        .map_err(TelemetryProfileError::Runtime)
    }

    pub fn stream_spec(
        &self,
        stream: DiscoveredTelemetryStream,
    ) -> Result<TelemetryStreamSpec, TelemetryProfileError> {
        validate_stream(&stream)?;
        Ok(TelemetryStreamSpec {
            path: self
                .root
                .join(&stream.channel_name)
                .join(bytes_hex(&stream.hardware_id.bytes()))
                .join(format!(
                    "boot-{:016x}-node-{:02x}.wal",
                    stream.boot_epoch.get(),
                    stream.node_address
                )),
            config: TelemetryWalConfig {
                hardware_id: stream.hardware_id,
                node_address: stream.node_address,
                boot_epoch: stream.boot_epoch,
            },
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn validate_stream(stream: &DiscoveredTelemetryStream) -> Result<(), TelemetryProfileError> {
    if !valid_component(&stream.channel_name) {
        return Err(TelemetryProfileError::InvalidChannelName);
    }
    if stream.node_address == HOST_ADDRESS || stream.node_address == BROADCAST_ADDRESS {
        return Err(TelemetryProfileError::InvalidNodeAddress(
            stream.node_address,
        ));
    }
    if stream.boot_epoch.get() == 0 {
        return Err(TelemetryProfileError::ZeroBootEpoch);
    }
    Ok(())
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
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
pub enum TelemetryProfileError {
    EmptyRoot,
    ZeroQueueCapacity,
    InvalidWorkerName,
    InvalidChannelName,
    InvalidNodeAddress(u8),
    ZeroBootEpoch,
    DuplicateStream,
    Runtime(TelemetryRuntimeConfigError),
}

impl fmt::Display for TelemetryProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "telemetry installation profile error: {self:?}")
    }
}

impl std::error::Error for TelemetryProfileError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(epoch: u64) -> DiscoveredTelemetryStream {
        DiscoveredTelemetryStream {
            channel_name: "line-a".to_owned(),
            hardware_id: HardwareId::from_bytes([1; 8]),
            node_address: 1,
            boot_epoch: BootEpoch::new(epoch),
        }
    }

    #[test]
    fn profile_is_optional_for_legacy_nodes_and_builds_identity_paths() {
        let profile = TelemetryPersistenceInstallationProfile::new(
            "var/recordings/.telemetry",
            64,
            "telemetry-storage",
        )
        .unwrap();
        assert!(profile.runtime_config(Vec::new()).unwrap().is_none());
        let spec = profile.stream_spec(stream(7)).unwrap();
        assert!(spec
            .path
            .ends_with("line-a/0101010101010101/boot-0000000000000007-node-01.wal"));
    }

    #[test]
    fn duplicate_identity_is_rejected_before_filesystem_access() {
        let profile =
            TelemetryPersistenceInstallationProfile::new("telemetry", 4, "worker").unwrap();
        assert!(matches!(
            profile.runtime_config(vec![stream(7), stream(7)]),
            Err(TelemetryProfileError::DuplicateStream)
        ));
    }
}
