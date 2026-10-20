use core::fmt;

use crate::telemetry_persistence::{
    TelemetryObservationQueueSender, TelemetryPersistenceExecutor,
    TelemetryPersistenceExecutorError, TelemetryPersistenceManagementError,
};
use crate::telemetry_repository::{
    TelemetryRepository, TelemetryRepositoryError, TelemetryStreamKey, TelemetryStreamRollover,
    TelemetryStreamSpec,
};

pub struct TelemetryRuntimeConfig {
    streams: Vec<TelemetryStreamSpec>,
    queue_capacity_records: usize,
    worker_name: String,
}

impl TelemetryRuntimeConfig {
    pub fn new(
        streams: Vec<TelemetryStreamSpec>,
        queue_capacity_records: usize,
        worker_name: impl Into<String>,
    ) -> Result<Self, TelemetryRuntimeConfigError> {
        if streams.is_empty() {
            return Err(TelemetryRuntimeConfigError::NoStreams);
        }
        if queue_capacity_records == 0 {
            return Err(TelemetryRuntimeConfigError::ZeroQueueCapacity);
        }
        let worker_name = worker_name.into();
        if worker_name.trim().is_empty() {
            return Err(TelemetryRuntimeConfigError::EmptyWorkerName);
        }
        Ok(Self {
            streams,
            queue_capacity_records,
            worker_name,
        })
    }

    pub fn start(self) -> Result<TelemetryRuntime, TelemetryRuntimeError> {
        let repository = TelemetryRepository::open(self.streams.clone())
            .map_err(TelemetryRuntimeError::Repository)?;
        let executor = TelemetryPersistenceExecutor::start(
            repository,
            self.queue_capacity_records,
            self.worker_name,
        )
        .map_err(TelemetryRuntimeError::Executor)?;
        Ok(TelemetryRuntime {
            streams: self.streams,
            executor,
            durable_completions: 0,
        })
    }
}

pub struct TelemetryRuntime {
    streams: Vec<TelemetryStreamSpec>,
    executor: TelemetryPersistenceExecutor,
    durable_completions: u64,
}

impl TelemetryRuntime {
    pub fn submitter(
        &self,
    ) -> Result<TelemetryObservationQueueSender, TelemetryPersistenceExecutorError> {
        self.executor.submitter()
    }

    pub fn poll_health(&mut self) -> Result<bool, TelemetryRuntimeHealthError> {
        let Some(completion) = self
            .executor
            .try_completion()
            .map_err(TelemetryRuntimeHealthError::Executor)?
        else {
            return Ok(false);
        };
        completion
            .result
            .map_err(TelemetryRuntimeHealthError::Repository)?;
        self.durable_completions = self.durable_completions.saturating_add(1);
        Ok(true)
    }

    pub const fn durable_completions(&self) -> u64 {
        self.durable_completions
    }

    pub fn rollover_stream(
        &mut self,
        previous: TelemetryStreamKey,
        next: TelemetryStreamSpec,
    ) -> Result<TelemetryStreamRollover, TelemetryRuntimeManagementError> {
        let next_key = TelemetryStreamKey::from_config(next.config);
        let already_registered = self
            .streams
            .iter()
            .any(|spec| TelemetryStreamKey::from_config(spec.config) == next_key);
        let rollover = self
            .executor
            .rollover_stream(previous, next.clone())
            .map_err(TelemetryRuntimeManagementError::Executor)?;
        if !already_registered {
            self.streams.push(next);
        }
        Ok(rollover)
    }

    pub fn shutdown(self) -> Result<TelemetryRepository, TelemetryPersistenceExecutorError> {
        self.executor.shutdown()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryRuntimeConfigError {
    NoStreams,
    ZeroQueueCapacity,
    EmptyWorkerName,
}

impl fmt::Display for TelemetryRuntimeConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "telemetry runtime configuration error: {self:?}")
    }
}
impl std::error::Error for TelemetryRuntimeConfigError {}

#[derive(Debug)]
pub enum TelemetryRuntimeError {
    Repository(TelemetryRepositoryError),
    Executor(TelemetryPersistenceExecutorError),
}
impl fmt::Display for TelemetryRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "telemetry runtime startup error: {self:?}")
    }
}
impl std::error::Error for TelemetryRuntimeError {}

#[derive(Debug)]
pub enum TelemetryRuntimeHealthError {
    Executor(TelemetryPersistenceExecutorError),
    Repository(TelemetryRepositoryError),
}
impl fmt::Display for TelemetryRuntimeHealthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "telemetry runtime health error: {self:?}")
    }
}
impl std::error::Error for TelemetryRuntimeHealthError {}

#[derive(Debug)]
pub enum TelemetryRuntimeManagementError {
    Executor(TelemetryPersistenceManagementError),
}
impl fmt::Display for TelemetryRuntimeManagementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "telemetry runtime management error: {self:?}")
    }
}
impl std::error::Error for TelemetryRuntimeManagementError {}
