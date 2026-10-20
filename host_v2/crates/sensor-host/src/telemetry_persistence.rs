use core::fmt;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::telemetry_repository::{
    TelemetryDurableReceipt, TelemetryObservationRecord, TelemetryRepository,
    TelemetryRepositoryError, TelemetryStreamKey, TelemetryStreamRollover, TelemetryStreamSpec,
};

/// Nonblocking boundary owned by an RS485 channel worker.
pub trait TelemetryObservationSubmitter: Send {
    fn try_submit(
        &mut self,
        record: TelemetryObservationRecord,
    ) -> Result<(), TelemetryObservationSubmitError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RejectingTelemetryObservationSubmitter;

impl TelemetryObservationSubmitter for RejectingTelemetryObservationSubmitter {
    fn try_submit(
        &mut self,
        record: TelemetryObservationRecord,
    ) -> Result<(), TelemetryObservationSubmitError> {
        Err(TelemetryObservationSubmitError {
            kind: TelemetryObservationSubmitErrorKind::Disconnected,
            record,
        })
    }
}

#[derive(Clone)]
pub struct TelemetryObservationQueueSender {
    sender: SyncSender<TelemetryObservationRecord>,
}

impl TelemetryObservationSubmitter for TelemetryObservationQueueSender {
    fn try_submit(
        &mut self,
        record: TelemetryObservationRecord,
    ) -> Result<(), TelemetryObservationSubmitError> {
        match self.sender.try_send(record) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(record)) => Err(TelemetryObservationSubmitError {
                kind: TelemetryObservationSubmitErrorKind::QueueFull,
                record,
            }),
            Err(TrySendError::Disconnected(record)) => Err(TelemetryObservationSubmitError {
                kind: TelemetryObservationSubmitErrorKind::Disconnected,
                record,
            }),
        }
    }
}

pub struct TelemetryObservationQueueReceiver {
    receiver: Receiver<TelemetryObservationRecord>,
}

impl TelemetryObservationQueueReceiver {
    pub fn try_receive(
        &self,
    ) -> Result<Option<TelemetryObservationRecord>, TelemetryObservationReceiveError> {
        match self.receiver.try_recv() {
            Ok(record) => Ok(Some(record)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(TelemetryObservationReceiveError::Disconnected),
        }
    }
}

pub fn telemetry_observation_queue(
    capacity: usize,
) -> Result<
    (
        TelemetryObservationQueueSender,
        TelemetryObservationQueueReceiver,
    ),
    TelemetryObservationQueueError,
> {
    if capacity == 0 {
        return Err(TelemetryObservationQueueError::ZeroCapacity);
    }
    let (sender, receiver) = mpsc::sync_channel(capacity);
    Ok((
        TelemetryObservationQueueSender { sender },
        TelemetryObservationQueueReceiver { receiver },
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryObservationSubmitErrorKind {
    QueueFull,
    Disconnected,
}

#[derive(Debug, Eq, PartialEq)]
pub struct TelemetryObservationSubmitError {
    kind: TelemetryObservationSubmitErrorKind,
    record: TelemetryObservationRecord,
}

impl TelemetryObservationSubmitError {
    pub const fn kind(&self) -> TelemetryObservationSubmitErrorKind {
        self.kind
    }

    pub const fn into_record(self) -> TelemetryObservationRecord {
        self.record
    }
}

impl fmt::Display for TelemetryObservationSubmitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "telemetry submission failed: {:?}", self.kind)
    }
}

impl std::error::Error for TelemetryObservationSubmitError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryObservationQueueError {
    ZeroCapacity,
}

impl fmt::Display for TelemetryObservationQueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("telemetry queue capacity must be greater than zero")
    }
}

impl std::error::Error for TelemetryObservationQueueError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryObservationReceiveError {
    Disconnected,
}

impl fmt::Display for TelemetryObservationReceiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("telemetry queue disconnected")
    }
}

impl std::error::Error for TelemetryObservationReceiveError {}

#[derive(Debug)]
pub struct TelemetryPersistenceCompletion {
    pub record: TelemetryObservationRecord,
    pub result: Result<TelemetryDurableReceipt, TelemetryRepositoryError>,
}

enum TelemetryPersistenceControl {
    RolloverStream {
        previous: TelemetryStreamKey,
        next: TelemetryStreamSpec,
        result: SyncSender<Result<TelemetryStreamRollover, TelemetryRepositoryError>>,
    },
    Shutdown,
}

pub struct TelemetryPersistenceExecutor {
    submitter: Option<TelemetryObservationQueueSender>,
    control: Option<SyncSender<TelemetryPersistenceControl>>,
    completions: Receiver<TelemetryPersistenceCompletion>,
    worker: Option<JoinHandle<TelemetryRepository>>,
}

impl TelemetryPersistenceExecutor {
    pub fn start(
        repository: TelemetryRepository,
        capacity: usize,
        worker_name: impl Into<String>,
    ) -> Result<Self, TelemetryPersistenceExecutorError> {
        let (submitter, input) = telemetry_observation_queue(capacity)
            .map_err(TelemetryPersistenceExecutorError::Queue)?;
        let completion_capacity = capacity
            .checked_add(1)
            .ok_or(TelemetryPersistenceExecutorError::CapacityOverflow)?;
        let (completion_sender, completions) = mpsc::sync_channel(completion_capacity);
        let (control_sender, control) = mpsc::sync_channel(1);
        let worker_name = worker_name.into();
        if worker_name.trim().is_empty() {
            return Err(TelemetryPersistenceExecutorError::EmptyWorkerName);
        }
        let worker = thread::Builder::new()
            .name(worker_name)
            .spawn(move || {
                run_telemetry_persistence_executor(
                    repository,
                    input.receiver,
                    control,
                    completion_sender,
                )
            })
            .map_err(TelemetryPersistenceExecutorError::Spawn)?;
        Ok(Self {
            submitter: Some(submitter),
            control: Some(control_sender),
            completions,
            worker: Some(worker),
        })
    }

    pub fn submitter(
        &self,
    ) -> Result<TelemetryObservationQueueSender, TelemetryPersistenceExecutorError> {
        self.submitter
            .clone()
            .ok_or(TelemetryPersistenceExecutorError::Stopped)
    }

    pub fn try_completion(
        &self,
    ) -> Result<Option<TelemetryPersistenceCompletion>, TelemetryPersistenceExecutorError> {
        match self.completions.try_recv() {
            Ok(completion) => Ok(Some(completion)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(TelemetryPersistenceExecutorError::Disconnected),
        }
    }

    pub fn rollover_stream(
        &self,
        previous: TelemetryStreamKey,
        next: TelemetryStreamSpec,
    ) -> Result<TelemetryStreamRollover, TelemetryPersistenceManagementError> {
        let control =
            self.control
                .as_ref()
                .ok_or(TelemetryPersistenceManagementError::Executor(
                    TelemetryPersistenceExecutorError::Stopped,
                ))?;
        let (result_sender, result) = mpsc::sync_channel(1);
        control
            .try_send(TelemetryPersistenceControl::RolloverStream {
                previous,
                next,
                result: result_sender,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => TelemetryPersistenceManagementError::Executor(
                    TelemetryPersistenceExecutorError::ControlPending,
                ),
                TrySendError::Disconnected(_) => TelemetryPersistenceManagementError::Executor(
                    TelemetryPersistenceExecutorError::Disconnected,
                ),
            })?;
        result
            .recv()
            .map_err(|_| {
                TelemetryPersistenceManagementError::Executor(
                    TelemetryPersistenceExecutorError::Disconnected,
                )
            })?
            .map_err(TelemetryPersistenceManagementError::Repository)
    }

    /// All submitter clones owned by channel workers must be dropped first.
    pub fn shutdown(mut self) -> Result<TelemetryRepository, TelemetryPersistenceExecutorError> {
        self.submitter.take();
        if let Some(control) = self.control.take() {
            let _ = control.send(TelemetryPersistenceControl::Shutdown);
        }
        let mut storage_error = None;
        while let Ok(completion) = self.completions.recv() {
            if let Err(error) = completion.result {
                if storage_error.is_none() {
                    storage_error = Some(error);
                }
            }
        }
        let repository = self
            .worker
            .take()
            .ok_or(TelemetryPersistenceExecutorError::Stopped)?
            .join()
            .map_err(|_| TelemetryPersistenceExecutorError::WorkerPanicked)?;
        if let Some(error) = storage_error {
            Err(TelemetryPersistenceExecutorError::Storage(error))
        } else {
            Ok(repository)
        }
    }
}

#[derive(Debug)]
pub enum TelemetryPersistenceExecutorError {
    Queue(TelemetryObservationQueueError),
    CapacityOverflow,
    EmptyWorkerName,
    Spawn(std::io::Error),
    Storage(TelemetryRepositoryError),
    Disconnected,
    ControlPending,
    Stopped,
    WorkerPanicked,
}

#[derive(Debug)]
pub enum TelemetryPersistenceManagementError {
    Executor(TelemetryPersistenceExecutorError),
    Repository(TelemetryRepositoryError),
}

impl fmt::Display for TelemetryPersistenceManagementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "telemetry persistence management error: {self:?}"
        )
    }
}

impl std::error::Error for TelemetryPersistenceManagementError {}

fn run_telemetry_persistence_executor(
    mut repository: TelemetryRepository,
    input: Receiver<TelemetryObservationRecord>,
    control: Receiver<TelemetryPersistenceControl>,
    completions: SyncSender<TelemetryPersistenceCompletion>,
) -> TelemetryRepository {
    const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(10);
    let mut faulted = false;
    loop {
        match control.try_recv() {
            Ok(TelemetryPersistenceControl::Shutdown) => {
                while let Ok(record) = input.recv() {
                    let result = repository.persist(record);
                    let failed = result.is_err();
                    if completions
                        .send(TelemetryPersistenceCompletion { record, result })
                        .is_err()
                    {
                        return repository;
                    }
                    if failed {
                        break;
                    }
                }
                break;
            }
            Err(TryRecvError::Disconnected) => break,
            Ok(TelemetryPersistenceControl::RolloverStream {
                previous,
                next,
                result,
            }) => {
                let outcome = if faulted {
                    Err(TelemetryRepositoryError::PersistenceFaulted)
                } else {
                    repository.rollover_stream(previous, next)
                };
                let _ = result.send(outcome);
            }
            Err(TryRecvError::Empty) => {}
        }
        if faulted {
            match control.recv() {
                Ok(TelemetryPersistenceControl::Shutdown) | Err(_) => break,
                Ok(TelemetryPersistenceControl::RolloverStream { result, .. }) => {
                    let _ = result.send(Err(TelemetryRepositoryError::PersistenceFaulted));
                }
            }
            continue;
        }
        match input.recv_timeout(CONTROL_POLL_INTERVAL) {
            Ok(record) => {
                let result = repository.persist(record);
                faulted = result.is_err();
                if completions
                    .send(TelemetryPersistenceCompletion { record, result })
                    .is_err()
                {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    repository
}

impl fmt::Display for TelemetryPersistenceExecutorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "telemetry persistence executor error: {self:?}")
    }
}

impl std::error::Error for TelemetryPersistenceExecutorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Queue(error) => Some(error),
            Self::Spawn(error) => Some(error),
            Self::Storage(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry_repository::{TelemetryStreamKey, TelemetryStreamSpec};
    use crate::telemetry_wal::TelemetryWalConfig;
    use sensor_core::{BootEpoch, HardwareId};
    use sensor_protocol::{Adxl355TemperatureCode, TelemetrySnapshot};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    struct TempFile(PathBuf);
    impl TempFile {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!(
                "sensor-telemetry-executor-{}-{id}.wal",
                std::process::id()
            )))
        }
    }
    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn config() -> TelemetryWalConfig {
        TelemetryWalConfig {
            hardware_id: HardwareId::from_bytes([1; 8]),
            node_address: 1,
            boot_epoch: BootEpoch::new(7),
        }
    }
    fn record(time: u64) -> TelemetryObservationRecord {
        record_for(config(), time)
    }
    fn record_for(config: TelemetryWalConfig, time: u64) -> TelemetryObservationRecord {
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

    fn unknown_record(time: u64) -> TelemetryObservationRecord {
        TelemetryObservationRecord::new(
            HardwareId::from_bytes([2; 8]),
            2,
            BootEpoch::new(8),
            TelemetrySnapshot {
                captured_device_time_us: time,
                adxl355_temperature: Adxl355TemperatureCode::new(1885).unwrap(),
            },
        )
        .unwrap()
    }

    #[test]
    fn bounded_queue_returns_the_exact_rejected_record() {
        let (mut sender, _receiver) = telemetry_observation_queue(1).unwrap();
        sender.try_submit(record(1)).unwrap();
        let rejected = sender.try_submit(record(2)).unwrap_err();
        assert_eq!(
            rejected.kind(),
            TelemetryObservationSubmitErrorKind::QueueFull
        );
        assert_eq!(rejected.into_record(), record(2));
    }

    #[test]
    fn executor_owns_io_and_reports_durable_receipt() {
        let temp = TempFile::new();
        let repository = TelemetryRepository::open([TelemetryStreamSpec {
            path: temp.0.clone(),
            config: config(),
        }])
        .unwrap();
        let executor =
            TelemetryPersistenceExecutor::start(repository, 2, "telemetry-test").unwrap();
        let mut sender = executor.submitter().unwrap();
        sender.try_submit(record(100)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let completion = loop {
            if let Some(completion) = executor.try_completion().unwrap() {
                break completion;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert_eq!(completion.record, record(100));
        assert_eq!(completion.result.unwrap().record_count, 1);
        drop(sender);
        let repository = executor.shutdown().unwrap();
        assert_eq!(
            repository
                .durable_record_count(TelemetryStreamKey::from_config(config()))
                .unwrap(),
            1
        );
    }

    #[test]
    fn shutdown_drains_bounded_completions_before_joining_worker() {
        let temp = TempFile::new();
        let repository = TelemetryRepository::open([TelemetryStreamSpec {
            path: temp.0.clone(),
            config: config(),
        }])
        .unwrap();
        let executor =
            TelemetryPersistenceExecutor::start(repository, 2, "telemetry-shutdown").unwrap();
        let mut sender = executor.submitter().unwrap();
        sender.try_submit(record(100)).unwrap();
        sender.try_submit(record(200)).unwrap();
        drop(sender);
        let repository = executor.shutdown().unwrap();
        assert_eq!(
            repository
                .durable_record_count(TelemetryStreamKey::from_config(config()))
                .unwrap(),
            2
        );
    }

    #[test]
    fn unconsumed_storage_failure_is_reported_by_shutdown() {
        let temp = TempFile::new();
        let repository = TelemetryRepository::open([TelemetryStreamSpec {
            path: temp.0.clone(),
            config: config(),
        }])
        .unwrap();
        let executor =
            TelemetryPersistenceExecutor::start(repository, 1, "telemetry-fault").unwrap();
        let mut sender = executor.submitter().unwrap();
        sender.try_submit(unknown_record(100)).unwrap();
        drop(sender);
        assert!(matches!(
            executor.shutdown(),
            Err(TelemetryPersistenceExecutorError::Storage(
                TelemetryRepositoryError::UnknownStream(_)
            ))
        ));
    }

    #[test]
    fn rollover_is_executed_by_the_exclusive_storage_owner() {
        let previous_file = TempFile::new();
        let next_file = TempFile::new();
        let previous = config();
        let repository = TelemetryRepository::open([TelemetryStreamSpec {
            path: previous_file.0.clone(),
            config: previous,
        }])
        .unwrap();
        let executor =
            TelemetryPersistenceExecutor::start(repository, 2, "telemetry-rollover").unwrap();
        let next = TelemetryWalConfig {
            boot_epoch: BootEpoch::new(8),
            ..previous
        };
        let rollover = executor
            .rollover_stream(
                TelemetryStreamKey::from_config(previous),
                TelemetryStreamSpec {
                    path: next_file.0.clone(),
                    config: next,
                },
            )
            .unwrap();
        assert_eq!(rollover.stream, TelemetryStreamKey::from_config(next));
        let mut sender = executor.submitter().unwrap();
        sender.try_submit(record_for(next, 100)).unwrap();
        drop(sender);
        let repository = executor.shutdown().unwrap();
        assert_eq!(
            repository
                .durable_record_count(TelemetryStreamKey::from_config(next))
                .unwrap(),
            1
        );
    }
}
