use core::fmt;
use std::path::PathBuf;

use sensor_core::{BootEpoch, HardwareId};

use crate::time_persistence::TimeObservationRecord;
use crate::time_wal::{HostClockId, TimeWalConfig, TimeWalError, TimeWalSink};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TimeStreamKey {
    pub hardware_id: HardwareId,
    pub node_address: u8,
    pub boot_epoch: BootEpoch,
}

impl TimeStreamKey {
    pub const fn from_record(record: TimeObservationRecord) -> Self {
        Self {
            hardware_id: record.hardware_id,
            node_address: record.node_address,
            boot_epoch: record.observation.boot_epoch,
        }
    }

    const fn from_config(config: TimeWalConfig) -> Self {
        Self {
            hardware_id: config.hardware_id,
            node_address: config.node_address,
            boot_epoch: config.boot_epoch,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeStreamSpec {
    pub path: PathBuf,
    pub config: TimeWalConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeDurableReceipt {
    pub stream: TimeStreamKey,
    /// Number of durable observations in this stream after the append.
    pub record_count: u64,
}

/// Fixed-topology owner of all Time WAL streams for one host clock epoch.
///
/// Runtime writes cannot create an unconfigured stream. A node or host reboot
/// requires explicit discovery, recovery and construction of a new repository.
pub struct TimeRepository {
    host_clock_id: HostClockId,
    streams: Vec<TimeWalSink>,
}

impl TimeRepository {
    pub fn open(
        specs: impl IntoIterator<Item = TimeStreamSpec>,
    ) -> Result<Self, TimeRepositoryError> {
        let mut streams = Vec::new();
        let mut host_clock_id = None;
        for spec in specs {
            if let Some(expected) = host_clock_id {
                if spec.config.host_clock_id != expected {
                    return Err(TimeRepositoryError::MixedHostClockIds);
                }
            } else {
                host_clock_id = Some(spec.config.host_clock_id);
            }
            let key = TimeStreamKey::from_config(spec.config);
            if streams
                .iter()
                .any(|stream: &TimeWalSink| TimeStreamKey::from_config(stream.config()) == key)
            {
                return Err(TimeRepositoryError::DuplicateStream(key));
            }
            streams
                .push(TimeWalSink::open(spec.path, spec.config).map_err(TimeRepositoryError::Wal)?);
        }
        let host_clock_id = host_clock_id.ok_or(TimeRepositoryError::NoStreams)?;
        Ok(Self {
            host_clock_id,
            streams,
        })
    }

    pub const fn host_clock_id(&self) -> HostClockId {
        self.host_clock_id
    }

    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    pub fn recoveries(&self) -> impl ExactSizeIterator<Item = (TimeStreamKey, u64)> + '_ {
        self.streams.iter().map(|stream| {
            (
                TimeStreamKey::from_config(stream.config()),
                stream.recovery().record_count,
            )
        })
    }

    pub fn durable_record_count(&self, stream: TimeStreamKey) -> Result<u64, TimeRepositoryError> {
        Ok(self.stream(stream)?.recovery().record_count)
    }

    pub fn persist(
        &mut self,
        record: TimeObservationRecord,
    ) -> Result<TimeDurableReceipt, TimeRepositoryError> {
        let key = TimeStreamKey::from_record(record);
        let stream = self.stream_mut(key)?;
        stream
            .append(record.observation)
            .map_err(TimeRepositoryError::Wal)?;
        Ok(TimeDurableReceipt {
            stream: key,
            record_count: stream.recovery().record_count,
        })
    }

    #[cfg(test)]
    pub(crate) fn inject_failure_before_next_sync(
        &mut self,
        stream: TimeStreamKey,
    ) -> Result<(), TimeRepositoryError> {
        self.stream_mut(stream)?.inject_failure_before_next_sync();
        Ok(())
    }

    fn stream(&self, key: TimeStreamKey) -> Result<&TimeWalSink, TimeRepositoryError> {
        self.streams
            .iter()
            .find(|stream| TimeStreamKey::from_config(stream.config()) == key)
            .ok_or(TimeRepositoryError::UnknownStream(key))
    }

    fn stream_mut(&mut self, key: TimeStreamKey) -> Result<&mut TimeWalSink, TimeRepositoryError> {
        self.streams
            .iter_mut()
            .find(|stream| TimeStreamKey::from_config(stream.config()) == key)
            .ok_or(TimeRepositoryError::UnknownStream(key))
    }
}

#[derive(Debug)]
pub enum TimeRepositoryError {
    Wal(TimeWalError),
    NoStreams,
    MixedHostClockIds,
    DuplicateStream(TimeStreamKey),
    UnknownStream(TimeStreamKey),
}

impl fmt::Display for TimeRepositoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "time repository error: {self:?}")
    }
}

impl std::error::Error for TimeRepositoryError {
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
    use sensor_core::ClockObservation;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sensor-time-repository-{}-{id}",
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

    fn config(hardware: u8, node_address: u8) -> TimeWalConfig {
        TimeWalConfig {
            hardware_id: HardwareId::from_bytes([hardware; 8]),
            node_address,
            boot_epoch: BootEpoch::new(7),
            host_clock_id: HostClockId::from_bytes([9; 16]),
        }
    }

    fn record(config: TimeWalConfig, base: u64) -> TimeObservationRecord {
        TimeObservationRecord::new(
            config.hardware_id,
            config.node_address,
            ClockObservation::new(config.boot_epoch, base, base + 10, base + 20, base + 40)
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn routes_records_without_mixing_streams() {
        let temp = TempDirectory::new();
        let first = config(1, 1);
        let second = config(2, 2);
        let mut repository = TimeRepository::open([
            TimeStreamSpec {
                path: temp.0.join("first.wal"),
                config: first,
            },
            TimeStreamSpec {
                path: temp.0.join("second.wal"),
                config: second,
            },
        ])
        .unwrap();

        let receipt = repository.persist(record(second, 1_000)).unwrap();
        assert_eq!(receipt.stream, TimeStreamKey::from_config(second));
        assert_eq!(receipt.record_count, 1);
        assert_eq!(
            repository
                .durable_record_count(TimeStreamKey::from_config(first))
                .unwrap(),
            0
        );
        assert_eq!(
            repository
                .durable_record_count(TimeStreamKey::from_config(second))
                .unwrap(),
            1
        );
    }

    #[test]
    fn rejects_unknown_duplicate_and_mixed_clock_streams() {
        let temp = TempDirectory::new();
        let first = config(1, 1);
        let mut repository = TimeRepository::open([TimeStreamSpec {
            path: temp.0.join("first.wal"),
            config: first,
        }])
        .unwrap();
        assert!(matches!(
            repository.persist(record(config(2, 2), 1_000)),
            Err(TimeRepositoryError::UnknownStream(_))
        ));

        assert!(matches!(
            TimeRepository::open([
                TimeStreamSpec {
                    path: temp.0.join("duplicate-a.wal"),
                    config: first,
                },
                TimeStreamSpec {
                    path: temp.0.join("duplicate-b.wal"),
                    config: first,
                },
            ]),
            Err(TimeRepositoryError::DuplicateStream(_))
        ));

        let mut other_clock = config(2, 2);
        other_clock.host_clock_id = HostClockId::from_bytes([8; 16]);
        assert!(matches!(
            TimeRepository::open([
                TimeStreamSpec {
                    path: temp.0.join("clock-a.wal"),
                    config: first,
                },
                TimeStreamSpec {
                    path: temp.0.join("clock-b.wal"),
                    config: other_clock,
                },
            ]),
            Err(TimeRepositoryError::MixedHostClockIds)
        ));
    }
}
