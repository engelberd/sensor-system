use core::fmt;
use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use sensor_core::{ClockObservation, HardwareId};
use sensor_protocol::{BROADCAST_ADDRESS, HOST_ADDRESS};

use crate::time_repository::{
    TimeDurableReceipt, TimeRepository, TimeRepositoryError, TimeStreamKey, TimeStreamRollover,
    TimeStreamSpec,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeObservationRecord {
    pub hardware_id: HardwareId,
    pub node_address: u8,
    pub observation: ClockObservation,
}

impl TimeObservationRecord {
    pub const fn new(
        hardware_id: HardwareId,
        node_address: u8,
        observation: ClockObservation,
    ) -> Result<Self, TimeObservationRecordError> {
        if node_address == HOST_ADDRESS || node_address == BROADCAST_ADDRESS {
            return Err(TimeObservationRecordError::InvalidNodeAddress(node_address));
        }
        Ok(Self {
            hardware_id,
            node_address,
            observation,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeObservationRecordError {
    InvalidNodeAddress(u8),
}

impl fmt::Display for TimeObservationRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid time observation record: {self:?}")
    }
}

impl std::error::Error for TimeObservationRecordError {}

/// Nonblocking boundary used by the channel owner.
///
/// Implementations must return immediately and return ownership of a rejected
/// record. Disk I/O, allocation driven by backlog and waiting for capacity are
/// forbidden behind this interface.
pub trait TimeObservationSubmitter {
    fn try_submit(
        &mut self,
        record: TimeObservationRecord,
    ) -> Result<(), TimeObservationSubmitError>;
}

/// Fail-closed default for compositions which have not installed a bounded
/// observation queue. It never discards a record silently.
#[derive(Clone, Copy, Debug, Default)]
pub struct RejectingTimeObservationSubmitter;

impl TimeObservationSubmitter for RejectingTimeObservationSubmitter {
    fn try_submit(
        &mut self,
        record: TimeObservationRecord,
    ) -> Result<(), TimeObservationSubmitError> {
        Err(TimeObservationSubmitError {
            kind: TimeObservationSubmitErrorKind::Disconnected,
            record,
        })
    }
}

#[derive(Clone)]
pub struct TimeObservationQueueSender {
    sender: SyncSender<TimeObservationRecord>,
}

impl TimeObservationSubmitter for TimeObservationQueueSender {
    fn try_submit(
        &mut self,
        record: TimeObservationRecord,
    ) -> Result<(), TimeObservationSubmitError> {
        match self.sender.try_send(record) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(record)) => Err(TimeObservationSubmitError {
                kind: TimeObservationSubmitErrorKind::QueueFull,
                record,
            }),
            Err(TrySendError::Disconnected(record)) => Err(TimeObservationSubmitError {
                kind: TimeObservationSubmitErrorKind::Disconnected,
                record,
            }),
        }
    }
}

pub struct TimeObservationQueueReceiver {
    receiver: Receiver<TimeObservationRecord>,
}

impl TimeObservationQueueReceiver {
    /// Storage-side nonblocking receive. A storage executor may instead own
    /// pacing around this method; the channel side never receives from here.
    pub fn try_receive(
        &self,
    ) -> Result<Option<TimeObservationRecord>, TimeObservationReceiveError> {
        match self.receiver.try_recv() {
            Ok(record) => Ok(Some(record)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(TimeObservationReceiveError::Disconnected),
        }
    }

    fn receive_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Option<TimeObservationRecord>, TimeObservationReceiveError> {
        match self.receiver.recv_timeout(timeout) {
            Ok(record) => Ok(Some(record)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(TimeObservationReceiveError::Disconnected),
        }
    }
}

pub fn time_observation_queue(
    capacity: usize,
) -> Result<(TimeObservationQueueSender, TimeObservationQueueReceiver), TimeObservationQueueError> {
    if capacity == 0 {
        return Err(TimeObservationQueueError::ZeroCapacity);
    }
    let (sender, receiver) = mpsc::sync_channel(capacity);
    Ok((
        TimeObservationQueueSender { sender },
        TimeObservationQueueReceiver { receiver },
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeObservationSubmitErrorKind {
    QueueFull,
    Disconnected,
}

#[derive(Debug, Eq, PartialEq)]
pub struct TimeObservationSubmitError {
    kind: TimeObservationSubmitErrorKind,
    record: TimeObservationRecord,
}

impl TimeObservationSubmitError {
    pub const fn kind(&self) -> TimeObservationSubmitErrorKind {
        self.kind
    }

    pub fn into_record(self) -> TimeObservationRecord {
        self.record
    }
}

impl fmt::Display for TimeObservationSubmitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "time observation submission failed: {:?}",
            self.kind
        )
    }
}

impl std::error::Error for TimeObservationSubmitError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeObservationReceiveError {
    Disconnected,
}

impl fmt::Display for TimeObservationReceiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("time observation queue disconnected")
    }
}

impl std::error::Error for TimeObservationReceiveError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeObservationQueueError {
    ZeroCapacity,
}

impl fmt::Display for TimeObservationQueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("time observation queue capacity must be greater than zero")
    }
}

impl std::error::Error for TimeObservationQueueError {}

#[derive(Debug)]
pub struct TimePersistenceCompletion {
    pub record: TimeObservationRecord,
    /// An error reports the failed attempt. The same record remains owned by
    /// the faulted worker until recovery classifies the uncertain append.
    pub result: Result<TimeDurableReceipt, TimeRepositoryError>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimePersistenceRecoveryDisposition {
    RetryRequired,
    AlreadyDurable,
}

#[derive(Clone, Copy, Debug)]
struct FailedTimeObservation {
    record: TimeObservationRecord,
    base_record_count: Option<u64>,
}

/// Storage-side owner for one bounded Time observation queue.
///
/// `process_ready` performs filesystem I/O and must run outside the RS485
/// channel loop. On the first error it stops consuming input and retains the
/// uncertain record until a recovered repository proves whether that append
/// is absent or already durable.
pub struct TimePersistenceWorker {
    repository: TimeRepository,
    input: TimeObservationQueueReceiver,
    completion_capacity: usize,
    completions: VecDeque<TimePersistenceCompletion>,
    retry: Option<TimeObservationRecord>,
    failed: Option<FailedTimeObservation>,
    faulted: bool,
}

impl TimePersistenceWorker {
    pub fn new(
        repository: TimeRepository,
        input: TimeObservationQueueReceiver,
        completion_capacity: usize,
    ) -> Result<Self, TimePersistenceWorkerError> {
        if completion_capacity == 0 {
            return Err(TimePersistenceWorkerError::ZeroCapacity);
        }
        Ok(Self {
            repository,
            input,
            completion_capacity,
            completions: VecDeque::with_capacity(completion_capacity),
            retry: None,
            failed: None,
            faulted: false,
        })
    }

    /// Persists at most `limit` observations and never lets completions grow
    /// beyond the configured bound.
    pub fn process_ready(&mut self, limit: usize) -> Result<usize, TimePersistenceWorkerError> {
        if limit == 0 {
            return Err(TimePersistenceWorkerError::ZeroProcessLimit);
        }
        if self.faulted {
            return Ok(0);
        }
        let mut processed = 0;
        while processed < limit && self.completions.len() < self.completion_capacity {
            let record = if let Some(record) = self.retry.take() {
                record
            } else {
                match self.input.try_receive() {
                    Ok(Some(record)) => record,
                    Ok(None) => break,
                    Err(TimeObservationReceiveError::Disconnected) => {
                        return Err(TimePersistenceWorkerError::InputDisconnected);
                    }
                }
            };
            self.persist_record(record);
            processed += 1;
            if self.faulted {
                break;
            }
        }
        Ok(processed)
    }

    fn process_next_wait(
        &mut self,
        timeout: Duration,
    ) -> Result<usize, TimePersistenceWorkerError> {
        if self.faulted || self.completions.len() >= self.completion_capacity {
            return Ok(0);
        }
        let record = if let Some(record) = self.retry.take() {
            Some(record)
        } else {
            self.input
                .receive_timeout(timeout)
                .map_err(|_| TimePersistenceWorkerError::InputDisconnected)?
        };
        let Some(record) = record else {
            return Ok(0);
        };
        self.persist_record(record);
        Ok(1)
    }

    fn persist_record(&mut self, record: TimeObservationRecord) {
        let stream = TimeStreamKey::from_record(record);
        let base_record_count = self.repository.durable_record_count(stream).ok();
        let result = self.repository.persist(record);
        if result.is_err() {
            self.failed = Some(FailedTimeObservation {
                record,
                base_record_count,
            });
            self.faulted = true;
        }
        self.completions
            .push_back(TimePersistenceCompletion { record, result });
    }

    pub fn poll_completion(&mut self) -> Option<TimePersistenceCompletion> {
        self.completions.pop_front()
    }

    pub const fn is_faulted(&self) -> bool {
        self.faulted
    }

    pub fn completion_count(&self) -> usize {
        self.completions.len()
    }

    fn rollover_stream(
        &mut self,
        previous: TimeStreamKey,
        next: TimeStreamSpec,
    ) -> Result<TimeStreamRollover, TimeStreamManagementError> {
        if self.faulted {
            return Err(TimeStreamManagementError::WorkerFaulted);
        }
        self.repository
            .rollover_stream(previous, next)
            .map_err(TimeStreamManagementError::Repository)
    }

    /// Reconciles the one uncertain append against an independently reopened
    /// repository. The failure completion must be consumed first so the final
    /// success cannot exceed the bounded completion queue.
    pub fn replace_recovered_repository(
        &mut self,
        repository: TimeRepository,
    ) -> Result<TimePersistenceRecoveryDisposition, TimePersistenceRecoveryError> {
        if !self.faulted {
            return Err(TimePersistenceRecoveryError::WorkerNotFaulted);
        }
        if !self.completions.is_empty() {
            return Err(TimePersistenceRecoveryError::CompletionPending);
        }
        if repository.host_clock_id() != self.repository.host_clock_id() {
            return Err(TimePersistenceRecoveryError::HostClockMismatch);
        }
        let failed = self
            .failed
            .ok_or(TimePersistenceRecoveryError::MissingFailedRecord)?;
        let base = failed
            .base_record_count
            .ok_or(TimePersistenceRecoveryError::UnclassifiableAttempt)?;
        let stream = TimeStreamKey::from_record(failed.record);
        let recovered = repository
            .durable_record_count(stream)
            .map_err(TimePersistenceRecoveryError::Repository)?;
        let durable = base
            .checked_add(1)
            .ok_or(TimePersistenceRecoveryError::CounterOverflow)?;
        let disposition = if recovered == base {
            TimePersistenceRecoveryDisposition::RetryRequired
        } else if recovered == durable {
            TimePersistenceRecoveryDisposition::AlreadyDurable
        } else {
            return Err(TimePersistenceRecoveryError::UnexpectedRecordCount {
                expected_absent: base,
                expected_durable: durable,
                recovered,
            });
        };

        self.repository = repository;
        self.failed = None;
        self.faulted = false;
        match disposition {
            TimePersistenceRecoveryDisposition::RetryRequired => {
                self.retry = Some(failed.record);
            }
            TimePersistenceRecoveryDisposition::AlreadyDurable => {
                self.completions.push_back(TimePersistenceCompletion {
                    record: failed.record,
                    result: Ok(TimeDurableReceipt {
                        stream,
                        record_count: durable,
                    }),
                });
            }
        }
        Ok(disposition)
    }

    pub fn into_repository(self) -> TimeRepository {
        self.repository
    }
}

pub fn time_persistence_pipeline(
    repository: TimeRepository,
    capacity: usize,
) -> Result<(TimeObservationQueueSender, TimePersistenceWorker), TimePersistencePipelineError> {
    let (sender, receiver) =
        time_observation_queue(capacity).map_err(TimePersistencePipelineError::Queue)?;
    let worker = TimePersistenceWorker::new(repository, receiver, capacity)
        .map_err(TimePersistencePipelineError::Worker)?;
    Ok((sender, worker))
}

enum TimePersistenceControl {
    Replace {
        repository: TimeRepository,
        result:
            SyncSender<Result<TimePersistenceRecoveryDisposition, TimePersistenceRecoveryError>>,
    },
    RolloverStream {
        previous: TimeStreamKey,
        next: TimeStreamSpec,
        result: SyncSender<Result<TimeStreamRollover, TimeStreamManagementError>>,
    },
    Shutdown,
}

/// Threaded owner of a Time repository and worker.
///
/// Channel loops receive cloned bounded submitters. Only this executor touches
/// the filesystem; completion and recovery remain on the coordinator side.
pub struct TimePersistenceExecutor {
    submitter: Option<TimeObservationQueueSender>,
    control: Option<SyncSender<TimePersistenceControl>>,
    completions: Receiver<TimePersistenceCompletion>,
    stopped_repository: Receiver<TimeRepository>,
    thread: Option<JoinHandle<()>>,
}

impl TimePersistenceExecutor {
    pub fn start(
        repository: TimeRepository,
        capacity: usize,
        thread_name: impl Into<String>,
    ) -> Result<Self, TimePersistenceExecutorError> {
        let (submitter, worker) =
            time_persistence_pipeline(repository, capacity).map_err(|error| match error {
                TimePersistencePipelineError::Queue(TimeObservationQueueError::ZeroCapacity)
                | TimePersistencePipelineError::Worker(
                    TimePersistenceWorkerError::ZeroCapacity,
                ) => TimePersistenceExecutorError::ZeroCapacity,
                TimePersistencePipelineError::Worker(error) => {
                    TimePersistenceExecutorError::Worker(error)
                }
            })?;
        let (control_tx, control_rx) = mpsc::sync_channel(1);
        let completion_capacity = capacity
            .checked_add(1)
            .ok_or(TimePersistenceExecutorError::CapacityOverflow)?;
        let (completion_tx, completion_rx) = mpsc::sync_channel(completion_capacity);
        let (stopped_tx, stopped_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || {
                let repository = run_time_persistence_executor(worker, control_rx, completion_tx);
                let _ = stopped_tx.send(repository);
            })
            .map_err(TimePersistenceExecutorError::Spawn)?;
        Ok(Self {
            submitter: Some(submitter),
            control: Some(control_tx),
            completions: completion_rx,
            stopped_repository: stopped_rx,
            thread: Some(thread),
        })
    }

    pub fn submitter(&self) -> Result<TimeObservationQueueSender, TimePersistenceExecutorError> {
        self.submitter
            .as_ref()
            .cloned()
            .ok_or(TimePersistenceExecutorError::AlreadyShutdown)
    }

    pub fn try_completion(
        &self,
    ) -> Result<Option<TimePersistenceCompletion>, TimePersistenceExecutorError> {
        match self.completions.try_recv() {
            Ok(completion) => Ok(Some(completion)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err(TimePersistenceExecutorError::WorkerDisconnected)
            }
        }
    }

    pub fn replace_recovered_repository(
        &self,
        repository: TimeRepository,
    ) -> Result<TimePersistenceRecoveryDisposition, TimePersistenceExecutorRecoveryError> {
        let control =
            self.control
                .as_ref()
                .ok_or(TimePersistenceExecutorRecoveryError::Executor(
                    TimePersistenceExecutorError::AlreadyShutdown,
                ))?;
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        control
            .try_send(TimePersistenceControl::Replace {
                repository,
                result: result_tx,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => TimePersistenceExecutorRecoveryError::Executor(
                    TimePersistenceExecutorError::ControlPending,
                ),
                TrySendError::Disconnected(_) => TimePersistenceExecutorRecoveryError::Executor(
                    TimePersistenceExecutorError::WorkerDisconnected,
                ),
            })?;
        result_rx
            .recv()
            .map_err(|_| {
                TimePersistenceExecutorRecoveryError::Executor(
                    TimePersistenceExecutorError::WorkerDisconnected,
                )
            })?
            .map_err(TimePersistenceExecutorRecoveryError::Recovery)
    }

    pub fn rollover_stream(
        &self,
        previous: TimeStreamKey,
        next: TimeStreamSpec,
    ) -> Result<TimeStreamRollover, TimePersistenceExecutorManagementError> {
        let control =
            self.control
                .as_ref()
                .ok_or(TimePersistenceExecutorManagementError::Executor(
                    TimePersistenceExecutorError::AlreadyShutdown,
                ))?;
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        control
            .try_send(TimePersistenceControl::RolloverStream {
                previous,
                next,
                result: result_tx,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => TimePersistenceExecutorManagementError::Executor(
                    TimePersistenceExecutorError::ControlPending,
                ),
                TrySendError::Disconnected(_) => TimePersistenceExecutorManagementError::Executor(
                    TimePersistenceExecutorError::WorkerDisconnected,
                ),
            })?;
        result_rx
            .recv()
            .map_err(|_| {
                TimePersistenceExecutorManagementError::Executor(
                    TimePersistenceExecutorError::WorkerDisconnected,
                )
            })?
            .map_err(TimePersistenceExecutorManagementError::Management)
    }

    pub fn shutdown(mut self) -> Result<TimePersistenceShutdown, TimePersistenceExecutorError> {
        self.submitter.take();
        if let Some(control) = self.control.take() {
            let _ = control.try_send(TimePersistenceControl::Shutdown);
        }
        let completions = self.completions.iter().collect();
        let thread = self
            .thread
            .take()
            .ok_or(TimePersistenceExecutorError::AlreadyShutdown)?;
        thread
            .join()
            .map_err(|_| TimePersistenceExecutorError::WorkerPanicked)?;
        let repository = self
            .stopped_repository
            .recv()
            .map_err(|_| TimePersistenceExecutorError::RepositoryUnavailable)?;
        Ok(TimePersistenceShutdown {
            repository,
            completions,
        })
    }
}

impl TimeObservationSubmitter for TimePersistenceExecutor {
    fn try_submit(
        &mut self,
        record: TimeObservationRecord,
    ) -> Result<(), TimeObservationSubmitError> {
        match self.submitter.as_mut() {
            Some(submitter) => submitter.try_submit(record),
            None => Err(TimeObservationSubmitError {
                kind: TimeObservationSubmitErrorKind::Disconnected,
                record,
            }),
        }
    }
}

impl Drop for TimePersistenceExecutor {
    fn drop(&mut self) {
        self.submitter.take();
        if let Some(control) = self.control.take() {
            let _ = control.try_send(TimePersistenceControl::Shutdown);
        }
        self.thread.take();
    }
}

pub struct TimePersistenceShutdown {
    pub repository: TimeRepository,
    pub completions: Vec<TimePersistenceCompletion>,
}

fn run_time_persistence_executor(
    mut worker: TimePersistenceWorker,
    control: Receiver<TimePersistenceControl>,
    completions: SyncSender<TimePersistenceCompletion>,
) -> TimeRepository {
    const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(10);
    loop {
        if worker.is_faulted() {
            match control.recv() {
                Ok(TimePersistenceControl::Replace { repository, result }) => {
                    let _ = result.send(worker.replace_recovered_repository(repository));
                }
                Ok(TimePersistenceControl::RolloverStream { result, .. }) => {
                    let _ = result.send(Err(TimeStreamManagementError::WorkerFaulted));
                }
                Ok(TimePersistenceControl::Shutdown) | Err(_) => break,
            }
        } else {
            match control.try_recv() {
                Ok(TimePersistenceControl::Shutdown) | Err(TryRecvError::Disconnected) => break,
                Ok(TimePersistenceControl::Replace {
                    repository: _,
                    result,
                }) => {
                    let _ = result.send(Err(TimePersistenceRecoveryError::WorkerNotFaulted));
                }
                Ok(TimePersistenceControl::RolloverStream {
                    previous,
                    next,
                    result,
                }) => {
                    let _ = result.send(worker.rollover_stream(previous, next));
                }
                Err(TryRecvError::Empty) => {}
            }
            match worker.process_next_wait(CONTROL_POLL_INTERVAL) {
                Ok(_) => {}
                Err(TimePersistenceWorkerError::InputDisconnected) => break,
                Err(_) => break,
            }
        }
        while let Some(completion) = worker.poll_completion() {
            if completions.send(completion).is_err() {
                return worker.into_repository();
            }
        }
    }
    while let Some(completion) = worker.poll_completion() {
        if completions.send(completion).is_err() {
            break;
        }
    }
    worker.into_repository()
}

#[derive(Debug)]
pub enum TimePersistenceExecutorError {
    ZeroCapacity,
    CapacityOverflow,
    Spawn(std::io::Error),
    Worker(TimePersistenceWorkerError),
    WorkerDisconnected,
    ControlPending,
    AlreadyShutdown,
    WorkerPanicked,
    RepositoryUnavailable,
}

#[derive(Debug)]
pub enum TimeStreamManagementError {
    WorkerFaulted,
    Repository(TimeRepositoryError),
}

impl fmt::Display for TimeStreamManagementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "time stream management error: {self:?}")
    }
}

impl std::error::Error for TimeStreamManagementError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Repository(error) => Some(error),
            Self::WorkerFaulted => None,
        }
    }
}

#[derive(Debug)]
pub enum TimePersistenceExecutorManagementError {
    Executor(TimePersistenceExecutorError),
    Management(TimeStreamManagementError),
}

impl fmt::Display for TimePersistenceExecutorManagementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "time persistence management error: {self:?}")
    }
}

impl std::error::Error for TimePersistenceExecutorManagementError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Executor(error) => Some(error),
            Self::Management(error) => Some(error),
        }
    }
}

impl fmt::Display for TimePersistenceExecutorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "time persistence executor error: {self:?}")
    }
}

impl std::error::Error for TimePersistenceExecutorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) => Some(error),
            Self::Worker(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum TimePersistenceExecutorRecoveryError {
    Executor(TimePersistenceExecutorError),
    Recovery(TimePersistenceRecoveryError),
}

impl fmt::Display for TimePersistenceExecutorRecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "time persistence executor recovery error: {self:?}"
        )
    }
}

impl std::error::Error for TimePersistenceExecutorRecoveryError {}

#[derive(Debug)]
pub enum TimePersistencePipelineError {
    Queue(TimeObservationQueueError),
    Worker(TimePersistenceWorkerError),
}

impl fmt::Display for TimePersistencePipelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "time persistence pipeline error: {self:?}")
    }
}

impl std::error::Error for TimePersistencePipelineError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimePersistenceWorkerError {
    ZeroCapacity,
    ZeroProcessLimit,
    InputDisconnected,
}

impl fmt::Display for TimePersistenceWorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "time persistence worker error: {self:?}")
    }
}

impl std::error::Error for TimePersistenceWorkerError {}

#[derive(Debug)]
pub enum TimePersistenceRecoveryError {
    WorkerNotFaulted,
    CompletionPending,
    HostClockMismatch,
    MissingFailedRecord,
    UnclassifiableAttempt,
    Repository(TimeRepositoryError),
    CounterOverflow,
    UnexpectedRecordCount {
        expected_absent: u64,
        expected_durable: u64,
        recovered: u64,
    },
}

impl fmt::Display for TimePersistenceRecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "time persistence recovery error: {self:?}")
    }
}

impl std::error::Error for TimePersistenceRecoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Repository(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_core::BootEpoch;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    use crate::time_repository::TimeStreamSpec;
    use crate::time_wal::{HostClockId, TimeWalConfig, TIME_WAL_HEADER_BYTES};

    const HARDWARE_ID: HardwareId = HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);
    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("sensor-time-worker-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn record(node_address: u8, host_send_us: u64) -> TimeObservationRecord {
        TimeObservationRecord::new(
            HARDWARE_ID,
            node_address,
            ClockObservation::new(
                BootEpoch::new(9),
                host_send_us,
                1_000,
                1_001,
                host_send_us + 10,
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn wal_config() -> TimeWalConfig {
        TimeWalConfig {
            hardware_id: HARDWARE_ID,
            node_address: 1,
            boot_epoch: BootEpoch::new(9),
            host_clock_id: HostClockId::from_bytes([7; 16]),
        }
    }

    fn worker_record(index: u64) -> TimeObservationRecord {
        let base = 10_000 + index * 1_000;
        TimeObservationRecord::new(
            HARDWARE_ID,
            1,
            ClockObservation::new(BootEpoch::new(9), base, base + 10, base + 20, base + 40)
                .unwrap(),
        )
        .unwrap()
    }

    fn repository(path: PathBuf) -> TimeRepository {
        TimeRepository::open([TimeStreamSpec {
            path,
            config: wal_config(),
        }])
        .unwrap()
    }

    fn wait_completion(executor: &TimePersistenceExecutor) -> TimePersistenceCompletion {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(completion) = executor.try_completion().unwrap() {
                return completion;
            }
            assert!(
                Instant::now() < deadline,
                "time persistence completion timed out"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn rejects_non_node_addresses_and_zero_capacity() {
        let observation = record(1, 10).observation;
        assert_eq!(
            TimeObservationRecord::new(HARDWARE_ID, HOST_ADDRESS, observation),
            Err(TimeObservationRecordError::InvalidNodeAddress(HOST_ADDRESS))
        );
        assert!(matches!(
            time_observation_queue(0),
            Err(TimeObservationQueueError::ZeroCapacity)
        ));
    }

    #[test]
    fn full_queue_returns_the_exact_record_without_waiting() {
        let (mut sender, receiver) = time_observation_queue(1).unwrap();
        let first = record(1, 10);
        let second = record(2, 20);
        sender.try_submit(first).unwrap();
        let error = sender.try_submit(second).unwrap_err();
        assert_eq!(error.kind(), TimeObservationSubmitErrorKind::QueueFull);
        assert_eq!(error.into_record(), second);
        assert_eq!(receiver.try_receive().unwrap(), Some(first));
        assert_eq!(receiver.try_receive().unwrap(), None);
    }

    #[test]
    fn disconnected_queue_returns_ownership_to_the_channel() {
        let (mut sender, receiver) = time_observation_queue(1).unwrap();
        drop(receiver);
        let value = record(1, 10);
        let error = sender.try_submit(value).unwrap_err();
        assert_eq!(error.kind(), TimeObservationSubmitErrorKind::Disconnected);
        assert_eq!(error.into_record(), value);
    }

    #[test]
    fn worker_persists_off_channel_and_emits_durable_completion() {
        let temp = TempDirectory::new();
        let path = temp.0.join("time.wal");
        let (mut sender, mut worker) = time_persistence_pipeline(repository(path), 2).unwrap();
        let first = worker_record(0);
        let second = worker_record(1);
        sender.try_submit(first).unwrap();
        sender.try_submit(second).unwrap();

        assert_eq!(worker.process_ready(8).unwrap(), 2);
        for (expected, count) in [(first, 1), (second, 2)] {
            let completion = worker.poll_completion().unwrap();
            assert_eq!(completion.record, expected);
            let receipt = completion.result.unwrap();
            assert_eq!(receipt.record_count, count);
        }
        assert!(!worker.is_faulted());
    }

    #[test]
    fn recovery_confirms_an_uncertain_record_without_duplicate_append() {
        let temp = TempDirectory::new();
        let path = temp.0.join("uncertain.wal");
        let mut initial_repository = repository(path.clone());
        let stream = TimeStreamKey::from_record(worker_record(0));
        initial_repository
            .inject_failure_before_next_sync(stream)
            .unwrap();
        let (mut sender, mut worker) = time_persistence_pipeline(initial_repository, 2).unwrap();
        let value = worker_record(0);
        sender.try_submit(value).unwrap();

        assert_eq!(worker.process_ready(1).unwrap(), 1);
        assert!(worker.is_faulted());
        assert!(worker.poll_completion().unwrap().result.is_err());
        assert_eq!(
            worker
                .replace_recovered_repository(repository(path))
                .unwrap(),
            TimePersistenceRecoveryDisposition::AlreadyDurable
        );
        let completion = worker.poll_completion().unwrap();
        assert_eq!(completion.record, value);
        assert_eq!(completion.result.unwrap().record_count, 1);
        assert_eq!(
            worker
                .into_repository()
                .durable_record_count(stream)
                .unwrap(),
            1
        );
    }

    #[test]
    fn recovery_retries_only_when_the_uncertain_record_is_absent() {
        let temp = TempDirectory::new();
        let path = temp.0.join("retry.wal");
        let mut initial_repository = repository(path.clone());
        let stream = TimeStreamKey::from_record(worker_record(0));
        initial_repository
            .inject_failure_before_next_sync(stream)
            .unwrap();
        let (mut sender, mut worker) = time_persistence_pipeline(initial_repository, 2).unwrap();
        let value = worker_record(0);
        sender.try_submit(value).unwrap();
        worker.process_ready(1).unwrap();
        assert!(worker.poll_completion().unwrap().result.is_err());

        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(TIME_WAL_HEADER_BYTES as u64)
            .unwrap();
        assert_eq!(
            worker
                .replace_recovered_repository(repository(path))
                .unwrap(),
            TimePersistenceRecoveryDisposition::RetryRequired
        );
        assert_eq!(worker.process_ready(1).unwrap(), 1);
        assert_eq!(worker.poll_completion().unwrap().record, value);
        assert_eq!(
            worker
                .into_repository()
                .durable_record_count(stream)
                .unwrap(),
            1
        );
    }

    #[test]
    fn threaded_executor_owns_io_and_stops_with_live_submitter_clone() {
        let temp = TempDirectory::new();
        let path = temp.0.join("threaded.wal");
        let executor =
            TimePersistenceExecutor::start(repository(path), 4, "test-time-persistence").unwrap();
        let mut first_channel = executor.submitter().unwrap();
        let _still_live = executor.submitter().unwrap();
        let first = worker_record(0);
        let second = worker_record(1);
        first_channel.try_submit(first).unwrap();
        first_channel.try_submit(second).unwrap();

        assert_eq!(wait_completion(&executor).record, first);
        assert_eq!(wait_completion(&executor).record, second);
        let shutdown = executor.shutdown().unwrap();
        assert!(shutdown.completions.is_empty());
        assert_eq!(
            shutdown
                .repository
                .durable_record_count(TimeStreamKey::from_record(first))
                .unwrap(),
            2
        );
    }

    #[test]
    fn threaded_executor_reconciles_uncertain_append_before_resuming() {
        let temp = TempDirectory::new();
        let path = temp.0.join("threaded-recovery.wal");
        let value = worker_record(0);
        let stream = TimeStreamKey::from_record(value);
        let mut initial_repository = repository(path.clone());
        initial_repository
            .inject_failure_before_next_sync(stream)
            .unwrap();
        let mut executor =
            TimePersistenceExecutor::start(initial_repository, 2, "test-time-persistence-recovery")
                .unwrap();
        executor.try_submit(value).unwrap();
        assert!(wait_completion(&executor).result.is_err());

        assert_eq!(
            executor
                .replace_recovered_repository(repository(path))
                .unwrap(),
            TimePersistenceRecoveryDisposition::AlreadyDurable
        );
        let completion = wait_completion(&executor);
        assert_eq!(completion.record, value);
        assert_eq!(completion.result.unwrap().record_count, 1);
        assert_eq!(
            executor
                .shutdown()
                .unwrap()
                .repository
                .durable_record_count(stream)
                .unwrap(),
            1
        );
    }
}
