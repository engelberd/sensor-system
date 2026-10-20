use core::fmt;
use std::path::PathBuf;

use sensor_core::{BootEpoch, HardwareId};
use sensor_protocol::{TelemetrySnapshot, BROADCAST_ADDRESS, HOST_ADDRESS};

use crate::telemetry_wal::{
    TelemetryAppend, TelemetryWalConfig, TelemetryWalError, TelemetryWalSink,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TelemetryStreamKey {
    pub hardware_id: HardwareId,
    pub node_address: u8,
    pub boot_epoch: BootEpoch,
}

impl TelemetryStreamKey {
    pub const fn from_config(config: TelemetryWalConfig) -> Self {
        Self {
            hardware_id: config.hardware_id,
            node_address: config.node_address,
            boot_epoch: config.boot_epoch,
        }
    }

    pub const fn from_record(record: TelemetryObservationRecord) -> Self {
        Self {
            hardware_id: record.hardware_id,
            node_address: record.node_address,
            boot_epoch: record.boot_epoch,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TelemetryObservationRecord {
    pub hardware_id: HardwareId,
    pub node_address: u8,
    pub boot_epoch: BootEpoch,
    pub snapshot: TelemetrySnapshot,
}

impl TelemetryObservationRecord {
    pub const fn new(
        hardware_id: HardwareId,
        node_address: u8,
        boot_epoch: BootEpoch,
        snapshot: TelemetrySnapshot,
    ) -> Result<Self, TelemetryObservationRecordError> {
        if node_address == HOST_ADDRESS || node_address == BROADCAST_ADDRESS {
            return Err(TelemetryObservationRecordError::InvalidNodeAddress(
                node_address,
            ));
        }
        if boot_epoch.get() == 0 {
            return Err(TelemetryObservationRecordError::ZeroBootEpoch);
        }
        Ok(Self {
            hardware_id,
            node_address,
            boot_epoch,
            snapshot,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryObservationRecordError {
    InvalidNodeAddress(u8),
    ZeroBootEpoch,
}

impl fmt::Display for TelemetryObservationRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid telemetry observation record: {self:?}")
    }
}

impl std::error::Error for TelemetryObservationRecordError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelemetryStreamSpec {
    pub path: PathBuf,
    pub config: TelemetryWalConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TelemetryDurableReceipt {
    pub stream: TelemetryStreamKey,
    pub record_count: u64,
    pub disposition: TelemetryAppend,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TelemetryStreamRollover {
    pub stream: TelemetryStreamKey,
    pub durable_record_count: u64,
}

pub struct TelemetryRepository {
    streams: Vec<TelemetryWalSink>,
}

impl TelemetryRepository {
    pub fn open(
        specs: impl IntoIterator<Item = TelemetryStreamSpec>,
    ) -> Result<Self, TelemetryRepositoryError> {
        let mut streams = Vec::new();
        for spec in specs {
            let key = TelemetryStreamKey::from_config(spec.config);
            if streams.iter().any(|stream: &TelemetryWalSink| {
                TelemetryStreamKey::from_config(stream.config()) == key
            }) {
                return Err(TelemetryRepositoryError::DuplicateStream(key));
            }
            streams.push(
                TelemetryWalSink::open(spec.path, spec.config)
                    .map_err(TelemetryRepositoryError::Wal)?,
            );
        }
        if streams.is_empty() {
            return Err(TelemetryRepositoryError::NoStreams);
        }
        Ok(Self { streams })
    }

    pub fn persist(
        &mut self,
        record: TelemetryObservationRecord,
    ) -> Result<TelemetryDurableReceipt, TelemetryRepositoryError> {
        let key = TelemetryStreamKey::from_record(record);
        let stream = self.stream_mut(key)?;
        let disposition = stream
            .append(record.snapshot)
            .map_err(TelemetryRepositoryError::Wal)?;
        Ok(TelemetryDurableReceipt {
            stream: key,
            record_count: stream.recovery().record_count,
            disposition,
        })
    }

    pub fn durable_record_count(
        &self,
        key: TelemetryStreamKey,
    ) -> Result<u64, TelemetryRepositoryError> {
        Ok(self.stream(key)?.recovery().record_count)
    }

    pub fn recoveries(&self) -> impl Iterator<Item = (TelemetryStreamKey, u64)> + '_ {
        self.streams.iter().map(|stream| {
            (
                TelemetryStreamKey::from_config(stream.config()),
                stream.recovery().record_count,
            )
        })
    }

    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    pub fn rollover_stream(
        &mut self,
        previous: TelemetryStreamKey,
        next: TelemetryStreamSpec,
    ) -> Result<TelemetryStreamRollover, TelemetryRepositoryError> {
        self.stream(previous)?;
        let next_key = TelemetryStreamKey::from_config(next.config);
        if previous.hardware_id != next_key.hardware_id
            || previous.node_address != next_key.node_address
        {
            return Err(TelemetryRepositoryError::RolloverIdentityMismatch {
                previous,
                next: next_key,
            });
        }
        if previous.boot_epoch == next_key.boot_epoch {
            return Err(TelemetryRepositoryError::UnchangedBootEpoch(previous));
        }
        if let Some(existing) = self
            .streams
            .iter()
            .find(|stream| TelemetryStreamKey::from_config(stream.config()) == next_key)
        {
            if existing.path() != next.path {
                return Err(TelemetryRepositoryError::ConflictingStreamPath(next_key));
            }
            return Ok(TelemetryStreamRollover {
                stream: next_key,
                durable_record_count: existing.recovery().record_count,
            });
        }
        let stream = TelemetryWalSink::open(next.path, next.config)
            .map_err(TelemetryRepositoryError::Wal)?;
        let durable_record_count = stream.recovery().record_count;
        self.streams.push(stream);
        Ok(TelemetryStreamRollover {
            stream: next_key,
            durable_record_count,
        })
    }

    fn stream(
        &self,
        key: TelemetryStreamKey,
    ) -> Result<&TelemetryWalSink, TelemetryRepositoryError> {
        self.streams
            .iter()
            .find(|stream| TelemetryStreamKey::from_config(stream.config()) == key)
            .ok_or(TelemetryRepositoryError::UnknownStream(key))
    }

    fn stream_mut(
        &mut self,
        key: TelemetryStreamKey,
    ) -> Result<&mut TelemetryWalSink, TelemetryRepositoryError> {
        self.streams
            .iter_mut()
            .find(|stream| TelemetryStreamKey::from_config(stream.config()) == key)
            .ok_or(TelemetryRepositoryError::UnknownStream(key))
    }
}

#[derive(Debug)]
pub enum TelemetryRepositoryError {
    Wal(TelemetryWalError),
    NoStreams,
    DuplicateStream(TelemetryStreamKey),
    UnknownStream(TelemetryStreamKey),
    RolloverIdentityMismatch {
        previous: TelemetryStreamKey,
        next: TelemetryStreamKey,
    },
    UnchangedBootEpoch(TelemetryStreamKey),
    ConflictingStreamPath(TelemetryStreamKey),
    PersistenceFaulted,
}

impl fmt::Display for TelemetryRepositoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "telemetry repository error: {self:?}")
    }
}

impl std::error::Error for TelemetryRepositoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Wal(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_protocol::Adxl355TemperatureCode;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    struct TempDirectory(PathBuf);
    impl TempDirectory {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sensor-telemetry-repository-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn config(hardware: u8, address: u8, epoch: u64) -> TelemetryWalConfig {
        TelemetryWalConfig {
            hardware_id: HardwareId::from_bytes([hardware; 8]),
            node_address: address,
            boot_epoch: BootEpoch::new(epoch),
        }
    }
    fn spec(root: &Path, name: &str, config: TelemetryWalConfig) -> TelemetryStreamSpec {
        TelemetryStreamSpec {
            path: root.join(name),
            config,
        }
    }
    fn record(config: TelemetryWalConfig, time: u64) -> TelemetryObservationRecord {
        TelemetryObservationRecord::new(
            config.hardware_id,
            config.node_address,
            config.boot_epoch,
            TelemetrySnapshot {
                captured_device_time_us: time,
                adxl355_temperature: Adxl355TemperatureCode::new(1885).unwrap(),
            },
        )
        .unwrap()
    }

    #[test]
    fn routes_and_deduplicates_without_mixing_streams() {
        let temp = TempDirectory::new();
        let first = config(1, 1, 7);
        let second = config(2, 2, 8);
        let mut repository = TelemetryRepository::open([
            spec(&temp.0, "first.wal", first),
            spec(&temp.0, "second.wal", second),
        ])
        .unwrap();
        let observation = record(second, 100);
        assert_eq!(
            repository.persist(observation).unwrap().disposition,
            TelemetryAppend::Appended
        );
        assert_eq!(
            repository.persist(observation).unwrap().disposition,
            TelemetryAppend::Duplicate
        );
        assert_eq!(
            repository
                .durable_record_count(TelemetryStreamKey::from_config(first))
                .unwrap(),
            0
        );
        assert_eq!(
            repository
                .durable_record_count(TelemetryStreamKey::from_config(second))
                .unwrap(),
            1
        );
    }

    #[test]
    fn rollover_is_identity_checked_and_idempotent() {
        let temp = TempDirectory::new();
        let previous = config(1, 1, 7);
        let previous_key = TelemetryStreamKey::from_config(previous);
        let mut repository =
            TelemetryRepository::open([spec(&temp.0, "old.wal", previous)]).unwrap();
        let next = config(1, 1, 8);
        let next_spec = spec(&temp.0, "new.wal", next);
        let rollover = repository
            .rollover_stream(previous_key, next_spec.clone())
            .unwrap();
        assert_eq!(rollover.stream, TelemetryStreamKey::from_config(next));
        assert_eq!(
            repository.rollover_stream(previous_key, next_spec).unwrap(),
            rollover
        );
        let wrong = config(2, 1, 9);
        assert!(matches!(
            repository.rollover_stream(previous_key, spec(&temp.0, "wrong.wal", wrong)),
            Err(TelemetryRepositoryError::RolloverIdentityMismatch { .. })
        ));
    }
}
