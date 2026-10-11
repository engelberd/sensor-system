use core::fmt;

use sensor_core::{BootEpoch, HardwareId, SequenceCursor};

use crate::capture_repository::{CaptureRepository, CaptureRepositoryError, CaptureStreamSpec};
use crate::persistence_executor::RecoveredCursorEvidence;
use crate::persistence_executor::{
    GroupCommitPolicy, GroupedPersistenceExecutor, PersistenceExecutorError,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveredCaptureStream {
    pub hardware_id: HardwareId,
    pub boot_epoch: BootEpoch,
    pub durable: SequenceCursor,
}

impl From<RecoveredCaptureStream> for RecoveredCursorEvidence {
    fn from(value: RecoveredCaptureStream) -> Self {
        Self {
            hardware_id: value.hardware_id,
            boot_epoch: value.boot_epoch,
            durable: value.durable,
        }
    }
}

/// Validated, parser-independent startup configuration for one storage owner.
pub struct CaptureRuntimeConfig {
    streams: Vec<CaptureStreamSpec>,
    queue_capacity_records: usize,
    group_commit_policy: GroupCommitPolicy,
    worker_name: String,
}

impl CaptureRuntimeConfig {
    pub fn new(
        streams: Vec<CaptureStreamSpec>,
        queue_capacity_records: usize,
        group_commit_policy: GroupCommitPolicy,
        worker_name: impl Into<String>,
    ) -> Result<Self, CaptureRuntimeConfigError> {
        if streams.is_empty() {
            return Err(CaptureRuntimeConfigError::NoStreams);
        }
        if queue_capacity_records == 0 {
            return Err(CaptureRuntimeConfigError::ZeroQueueCapacity);
        }
        let worker_name = worker_name.into();
        if worker_name.trim().is_empty() {
            return Err(CaptureRuntimeConfigError::EmptyWorkerName);
        }
        Ok(Self {
            streams,
            queue_capacity_records,
            group_commit_policy,
            worker_name,
        })
    }

    pub fn start(self) -> Result<CaptureRuntime, CaptureRuntimeError> {
        let repository =
            CaptureRepository::open(self.streams).map_err(CaptureRuntimeError::Repository)?;
        let recovered_streams = repository
            .durable_cursors()
            .map(
                |(hardware_id, boot_epoch, durable)| RecoveredCaptureStream {
                    hardware_id,
                    boot_epoch,
                    durable,
                },
            )
            .collect();
        let executor = GroupedPersistenceExecutor::start(
            repository,
            self.queue_capacity_records,
            self.group_commit_policy,
            self.worker_name,
        )
        .map_err(CaptureRuntimeError::Executor)?;
        Ok(CaptureRuntime {
            recovered_streams,
            executor,
        })
    }

    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    pub const fn queue_capacity_records(&self) -> usize {
        self.queue_capacity_records
    }

    pub const fn group_commit_policy(&self) -> GroupCommitPolicy {
        self.group_commit_policy
    }

    pub fn worker_name(&self) -> &str {
        &self.worker_name
    }
}

/// Startup result whose recovery snapshot is captured before the repository is
/// moved to its exclusive storage thread.
pub struct CaptureRuntime {
    recovered_streams: Vec<RecoveredCaptureStream>,
    executor: GroupedPersistenceExecutor<CaptureRepository>,
}

impl CaptureRuntime {
    pub fn recovered_streams(&self) -> &[RecoveredCaptureStream] {
        &self.recovered_streams
    }

    pub fn into_parts(
        self,
    ) -> (
        Vec<RecoveredCaptureStream>,
        GroupedPersistenceExecutor<CaptureRepository>,
    ) {
        (self.recovered_streams, self.executor)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureRuntimeConfigError {
    NoStreams,
    ZeroQueueCapacity,
    EmptyWorkerName,
}

impl fmt::Display for CaptureRuntimeConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "capture runtime configuration error: {self:?}")
    }
}

impl std::error::Error for CaptureRuntimeConfigError {}

#[derive(Debug)]
pub enum CaptureRuntimeError {
    Repository(CaptureRepositoryError),
    Executor(PersistenceExecutorError),
}

impl fmt::Display for CaptureRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repository(error) => write!(formatter, "capture recovery failed: {error}"),
            Self::Executor(error) => write!(formatter, "capture executor failed: {error}"),
        }
    }
}

impl std::error::Error for CaptureRuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Repository(error) => Some(error),
            Self::Executor(error) => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::bounded_writer::{PersistenceEnvelope, PersistenceSubmitter};
    use crate::capture_wal::CaptureWalConfig;
    use crate::capture_window::CaptureWindowPolicy;
    use sensor_core::{SampleAxes, SampleAxis};
    use sensor_protocol::{SampleChunk, SampleChunkHeader, TimingEvidence, TIMING_VALID};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sensor-host-capture-runtime-{}-{id}",
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

    fn spec(root: &Path) -> CaptureStreamSpec {
        CaptureStreamSpec {
            directory: root.join("node-1"),
            config: CaptureWalConfig {
                hardware_id: HardwareId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0]),
                boot_epoch: BootEpoch::new(7),
                recovered_cursor: SequenceCursor::ORIGIN,
                first_window_ordinal: 0,
                window_policy: CaptureWindowPolicy::new(600_000_000).unwrap(),
            },
        }
    }

    fn envelope() -> PersistenceEnvelope {
        envelope_at(0)
    }

    fn envelope_at(first: u64) -> PersistenceEnvelope {
        let axis = SampleAxis::new(19).unwrap();
        PersistenceEnvelope::chunk(
            HardwareId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0]),
            SampleChunk {
                header: SampleChunkHeader {
                    boot_epoch: BootEpoch::new(7),
                    retained_from: SequenceCursor::new(first),
                    produced: SequenceCursor::new(first + 1),
                    first: SequenceCursor::new(first),
                    sample_count: 1,
                },
                config_revision: 1,
                timing: TimingEvidence {
                    segment_id: 1,
                    first_device_time_us: 100,
                    last_device_time_us: 100,
                    max_fit_residual_us: 0,
                    quality_flags: TIMING_VALID,
                },
                samples: vec![SampleAxes::new(axis, axis, axis)],
            },
        )
        .unwrap()
    }

    fn group_policy() -> GroupCommitPolicy {
        GroupCommitPolicy::new(4, std::time::Duration::from_millis(1)).unwrap()
    }

    #[test]
    fn recovery_snapshot_precedes_exclusive_executor_ownership() {
        let temp = TempDirectory::new();
        let runtime =
            CaptureRuntimeConfig::new(vec![spec(&temp.0)], 2, group_policy(), "capture-line-a")
                .unwrap()
                .start()
                .unwrap();
        assert_eq!(
            runtime.recovered_streams(),
            &[RecoveredCaptureStream {
                hardware_id: HardwareId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0]),
                boot_epoch: BootEpoch::new(7),
                durable: SequenceCursor::ORIGIN,
            }]
        );
        let (_, mut executor) = runtime.into_parts();
        executor.submit_record(envelope()).unwrap();
        executor.submit_record(envelope_at(1)).unwrap();
        let mut completions = Vec::new();
        for _ in 0..1_000 {
            if let Some(received) = executor.try_completion().unwrap() {
                completions.push(received);
                if completions.len() == 2 {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(completions.len(), 2, "capture completions did not arrive");
        assert_eq!(
            completions.pop().unwrap().result.unwrap().end,
            SequenceCursor::new(2)
        );
        let shutdown = executor.shutdown().unwrap();
        assert_eq!(
            shutdown.sink.durable_cursors().next().unwrap().2,
            SequenceCursor::new(2)
        );
        drop(shutdown);

        let restarted =
            CaptureRuntimeConfig::new(vec![spec(&temp.0)], 2, group_policy(), "capture-line-a")
                .unwrap()
                .start()
                .unwrap();
        assert_eq!(
            restarted.recovered_streams()[0].durable,
            SequenceCursor::new(2)
        );
        let (_, executor) = restarted.into_parts();
        executor.shutdown().unwrap();
    }

    #[test]
    fn invalid_runtime_policy_is_rejected_without_touching_storage() {
        let temp = TempDirectory::new();
        assert!(matches!(
            CaptureRuntimeConfig::new(Vec::new(), 1, group_policy(), "capture"),
            Err(CaptureRuntimeConfigError::NoStreams)
        ));
        assert!(matches!(
            CaptureRuntimeConfig::new(vec![spec(&temp.0)], 0, group_policy(), "capture"),
            Err(CaptureRuntimeConfigError::ZeroQueueCapacity)
        ));
        assert!(matches!(
            CaptureRuntimeConfig::new(vec![spec(&temp.0)], 1, group_policy(), "  "),
            Err(CaptureRuntimeConfigError::EmptyWorkerName)
        ));
    }
}
