use core::fmt;

use crate::time_health::{
    TimeHealthIntegrityError, TimePersistenceHealth, TimePersistenceHealthState,
};
use crate::time_persistence::{
    TimeObservationQueueSender, TimePersistenceExecutor, TimePersistenceExecutorError,
    TimePersistenceExecutorManagementError, TimePersistenceExecutorRecoveryError,
    TimePersistenceRecoveryDisposition, TimePersistenceShutdown,
};
use crate::time_repository::{
    TimeRepository, TimeRepositoryError, TimeStreamKey, TimeStreamRollover, TimeStreamSpec,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveredTimeStream {
    pub stream: TimeStreamKey,
    pub durable_record_count: u64,
}

/// Parser-independent startup configuration for the single Time WAL owner.
pub struct TimeRuntimeConfig {
    streams: Vec<TimeStreamSpec>,
    queue_capacity_records: usize,
    worker_name: String,
}

impl TimeRuntimeConfig {
    pub fn new(
        streams: Vec<TimeStreamSpec>,
        queue_capacity_records: usize,
        worker_name: impl Into<String>,
    ) -> Result<Self, TimeRuntimeConfigError> {
        if streams.is_empty() {
            return Err(TimeRuntimeConfigError::NoStreams);
        }
        if queue_capacity_records == 0 {
            return Err(TimeRuntimeConfigError::ZeroQueueCapacity);
        }
        let worker_name = worker_name.into();
        if worker_name.trim().is_empty() {
            return Err(TimeRuntimeConfigError::EmptyWorkerName);
        }
        Ok(Self {
            streams,
            queue_capacity_records,
            worker_name,
        })
    }

    pub fn start(self) -> Result<TimeRuntime, TimeRuntimeError> {
        let repository =
            TimeRepository::open(self.streams.clone()).map_err(TimeRuntimeError::Repository)?;
        let recovered_streams = repository
            .recoveries()
            .map(|(stream, durable_record_count)| RecoveredTimeStream {
                stream,
                durable_record_count,
            })
            .collect();
        let health = TimePersistenceHealth::from_recovery(repository.recoveries())
            .map_err(TimeRuntimeError::Health)?;
        let executor = TimePersistenceExecutor::start(
            repository,
            self.queue_capacity_records,
            self.worker_name,
        )
        .map_err(TimeRuntimeError::Executor)?;
        Ok(TimeRuntime {
            streams: self.streams,
            recovered_streams,
            health,
            executor,
        })
    }

    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    pub const fn queue_capacity_records(&self) -> usize {
        self.queue_capacity_records
    }

    pub fn worker_name(&self) -> &str {
        &self.worker_name
    }
}

/// Running Time persistence service. All filesystem access remains owned by
/// its executor thread after startup recovery.
pub struct TimeRuntime {
    streams: Vec<TimeStreamSpec>,
    recovered_streams: Vec<RecoveredTimeStream>,
    health: TimePersistenceHealth,
    executor: TimePersistenceExecutor,
}

impl TimeRuntime {
    pub fn recovered_streams(&self) -> &[RecoveredTimeStream] {
        &self.recovered_streams
    }

    pub fn submitter(&self) -> Result<TimeObservationQueueSender, TimePersistenceExecutorError> {
        self.executor.submitter()
    }

    pub const fn health(&self) -> &TimePersistenceHealth {
        &self.health
    }

    pub fn rollover_stream(
        &mut self,
        previous: TimeStreamKey,
        next: TimeStreamSpec,
    ) -> Result<TimeStreamRollover, TimeRuntimeManagementError> {
        if self.health.state() != TimePersistenceHealthState::Healthy {
            return Err(TimeRuntimeManagementError::HealthNotReady);
        }
        let next_key = TimeStreamKey {
            hardware_id: next.config.hardware_id,
            node_address: next.config.node_address,
            boot_epoch: next.config.boot_epoch,
        };
        let already_registered = self.streams.iter().any(|spec| {
            spec.config.hardware_id == next_key.hardware_id
                && spec.config.node_address == next_key.node_address
                && spec.config.boot_epoch == next_key.boot_epoch
        });
        let result = self
            .executor
            .rollover_stream(previous, next.clone())
            .map_err(TimeRuntimeManagementError::Executor)?;
        if !already_registered {
            self.health
                .register_stream(result.stream, result.durable_record_count)
                .map_err(TimeRuntimeManagementError::Health)?;
            self.streams.push(next);
        }
        Ok(result)
    }

    /// Projects at most one storage completion into health without blocking.
    pub fn poll_health(&mut self) -> Result<bool, TimeRuntimeHealthError> {
        let Some(completion) = self
            .executor
            .try_completion()
            .map_err(TimeRuntimeHealthError::Executor)?
        else {
            return Ok(false);
        };
        self.health
            .observe_completion(completion)
            .map_err(TimeRuntimeHealthError::Integrity)?;
        Ok(true)
    }

    /// Reopens every configured WAL after a persistence fault and lets the
    /// worker reconcile its one uncertain record against durable record count.
    pub fn recover(
        &mut self,
    ) -> Result<TimePersistenceRecoveryDisposition, TimeRuntimeRecoveryError> {
        let repository = TimeRepository::open(self.streams.clone())
            .map_err(TimeRuntimeRecoveryError::Repository)?;
        let disposition = self
            .executor
            .replace_recovered_repository(repository)
            .map_err(TimeRuntimeRecoveryError::Executor)?;
        self.health
            .observe_recovery(disposition)
            .map_err(TimeRuntimeRecoveryError::Health)?;
        Ok(disposition)
    }

    pub fn shutdown(self) -> Result<TimePersistenceShutdown, TimePersistenceExecutorError> {
        self.executor.shutdown()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeRuntimeConfigError {
    NoStreams,
    ZeroQueueCapacity,
    EmptyWorkerName,
}

impl fmt::Display for TimeRuntimeConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "time runtime configuration error: {self:?}")
    }
}

impl std::error::Error for TimeRuntimeConfigError {}

#[derive(Debug)]
pub enum TimeRuntimeError {
    Repository(TimeRepositoryError),
    Health(TimeHealthIntegrityError),
    Executor(TimePersistenceExecutorError),
}

impl fmt::Display for TimeRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "time runtime startup error: {self:?}")
    }
}

impl std::error::Error for TimeRuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Repository(error) => Some(error),
            Self::Health(error) => Some(error),
            Self::Executor(error) => Some(error),
        }
    }
}

#[derive(Debug)]
pub enum TimeRuntimeRecoveryError {
    Repository(TimeRepositoryError),
    Executor(TimePersistenceExecutorRecoveryError),
    Health(TimeHealthIntegrityError),
}

#[derive(Debug)]
pub enum TimeRuntimeManagementError {
    HealthNotReady,
    Executor(TimePersistenceExecutorManagementError),
    Health(TimeHealthIntegrityError),
}

impl fmt::Display for TimeRuntimeManagementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "time runtime management error: {self:?}")
    }
}

impl std::error::Error for TimeRuntimeManagementError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Executor(error) => Some(error),
            Self::Health(error) => Some(error),
            Self::HealthNotReady => None,
        }
    }
}

impl fmt::Display for TimeRuntimeRecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "time runtime recovery error: {self:?}")
    }
}

impl std::error::Error for TimeRuntimeRecoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Repository(error) => Some(error),
            Self::Executor(error) => Some(error),
            Self::Health(error) => Some(error),
        }
    }
}

#[derive(Debug)]
pub enum TimeRuntimeHealthError {
    Executor(TimePersistenceExecutorError),
    Integrity(TimeHealthIntegrityError),
}

impl fmt::Display for TimeRuntimeHealthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "time runtime health error: {self:?}")
    }
}

impl std::error::Error for TimeRuntimeHealthError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Executor(error) => Some(error),
            Self::Integrity(error) => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use sensor_core::{BootEpoch, ClockObservation, HardwareId};

    use crate::time_persistence::{TimeObservationRecord, TimeObservationSubmitter};
    use crate::time_wal::{HostClockId, TimeWalConfig};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sensor-host-time-runtime-{}-{id}",
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

    fn spec(root: &Path) -> TimeStreamSpec {
        TimeStreamSpec {
            path: root.join("time.wal"),
            config: TimeWalConfig {
                hardware_id: HardwareId::from_bytes([1; 8]),
                node_address: 1,
                boot_epoch: BootEpoch::new(7),
                host_clock_id: HostClockId::from_bytes([9; 16]),
            },
        }
    }

    fn record() -> TimeObservationRecord {
        TimeObservationRecord::new(
            HardwareId::from_bytes([1; 8]),
            1,
            ClockObservation::new(BootEpoch::new(7), 10, 20, 30, 40).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn startup_recovers_before_exposing_the_submitter() {
        let temp = TempDirectory::new();
        let config = TimeRuntimeConfig::new(vec![spec(&temp.0)], 2, "time-storage").unwrap();
        let mut runtime = config.start().unwrap();
        assert_eq!(runtime.recovered_streams()[0].durable_record_count, 0);

        let mut submitter = runtime.submitter().unwrap();
        submitter.try_submit(record()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !runtime.poll_health().unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(runtime.health().streams()[0].durable_record_count, 1);
        let shutdown = runtime.shutdown().unwrap();
        assert_eq!(shutdown.repository.recoveries().next().unwrap().1, 1);
    }

    #[test]
    fn runtime_registers_a_new_epoch_before_accepting_its_observations() {
        let temp = TempDirectory::new();
        let previous = spec(&temp.0);
        let previous_key = TimeStreamKey {
            hardware_id: previous.config.hardware_id,
            node_address: previous.config.node_address,
            boot_epoch: previous.config.boot_epoch,
        };
        let config = TimeRuntimeConfig::new(vec![previous.clone()], 2, "time-rollover").unwrap();
        let mut runtime = config.start().unwrap();
        let mut next = previous;
        next.path = temp.0.join("time-epoch-8.wal");
        next.config.boot_epoch = BootEpoch::new(8);

        let rollover = runtime.rollover_stream(previous_key, next).unwrap();
        assert_eq!(rollover.stream.boot_epoch, BootEpoch::new(8));
        assert_eq!(runtime.health().streams().len(), 2);
        assert_eq!(runtime.shutdown().unwrap().repository.stream_count(), 2);
    }

    #[test]
    fn configuration_rejects_unbounded_or_ambiguous_startup() {
        assert!(matches!(
            TimeRuntimeConfig::new(Vec::new(), 1, "time"),
            Err(TimeRuntimeConfigError::NoStreams)
        ));
        assert!(matches!(
            TimeRuntimeConfig::new(vec![spec(Path::new("root"))], 0, "time"),
            Err(TimeRuntimeConfigError::ZeroQueueCapacity)
        ));
        assert!(matches!(
            TimeRuntimeConfig::new(vec![spec(Path::new("root"))], 1, "  "),
            Err(TimeRuntimeConfigError::EmptyWorkerName)
        ));
    }
}
