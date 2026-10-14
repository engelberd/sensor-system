use core::fmt;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::bounded_writer::{
    GroupDurableMeasurementSink, GroupPersistError, PersistenceEnvelope, PersistenceSubmitter,
    SubmitError, SubmitErrorKind, WriteCompletion, WriteJobId,
};
use crate::measurement_persistence::DurableMeasurementSink;
use sensor_core::{BootEpoch, HardwareId, SequenceCursor};

struct WriteCommand {
    id: WriteJobId,
    envelope: PersistenceEnvelope,
}

enum RecoveryCommand<S> {
    ReplaceSink {
        sink: S,
        disposition: RecoveryDisposition,
    },
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryDisposition {
    RetryUncommitted,
    ConfirmAlreadyDurable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriteRecoveryRange {
    pub hardware_id: HardwareId,
    pub boot_epoch: BootEpoch,
    pub start: SequenceCursor,
    pub end: SequenceCursor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveredCursorEvidence {
    pub hardware_id: HardwareId,
    pub boot_epoch: BootEpoch,
    pub durable: SequenceCursor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GroupRecoveryDisposition {
    /// Jobs at the start of the attempted group which recovery proved durable.
    pub confirmed_prefix_records: usize,
}

pub fn classify_recovery(
    pending: WriteRecoveryRange,
    recovered: RecoveredCursorEvidence,
) -> Result<RecoveryDisposition, RecoveryClassificationError> {
    if pending.hardware_id != recovered.hardware_id || pending.boot_epoch != recovered.boot_epoch {
        return Err(RecoveryClassificationError::IdentityMismatch);
    }
    if pending.end <= pending.start {
        return Err(RecoveryClassificationError::InvalidPendingRange);
    }
    if recovered.durable == pending.start {
        return Ok(RecoveryDisposition::RetryUncommitted);
    }
    if recovered.durable >= pending.end {
        return Ok(RecoveryDisposition::ConfirmAlreadyDurable);
    }
    Err(RecoveryClassificationError::CursorInsidePendingRecord)
}

/// Classifies an uncertain group written to one ordered physical WAL.
///
/// A failed durability barrier may recover no record, a complete prefix, or
/// the entire group. A cursor inside a record is never approximated.
pub fn classify_group_recovery(
    pending: &[WriteRecoveryRange],
    recovered: RecoveredCursorEvidence,
) -> Result<GroupRecoveryDisposition, RecoveryClassificationError> {
    let Some(first) = pending.first().copied() else {
        return Err(RecoveryClassificationError::EmptyPendingGroup);
    };
    let mut expected_start = first.start;
    for range in pending {
        if range.hardware_id != recovered.hardware_id || range.boot_epoch != recovered.boot_epoch {
            return Err(RecoveryClassificationError::IdentityMismatch);
        }
        if range.end <= range.start {
            return Err(RecoveryClassificationError::InvalidPendingRange);
        }
        if range.start != expected_start {
            return Err(RecoveryClassificationError::NonContiguousPendingGroup);
        }
        expected_start = range.end;
    }
    if recovered.durable < first.start {
        return Err(RecoveryClassificationError::CursorBeforePendingRange);
    }
    if recovered.durable >= expected_start {
        return Ok(GroupRecoveryDisposition {
            confirmed_prefix_records: pending.len(),
        });
    }
    for (index, range) in pending.iter().enumerate() {
        if recovered.durable == range.start {
            return Ok(GroupRecoveryDisposition {
                confirmed_prefix_records: index,
            });
        }
        if recovered.durable < range.end {
            return Err(RecoveryClassificationError::CursorInsidePendingRecord);
        }
    }
    unreachable!("cursor before group end must match a boundary or lie inside a record")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryClassificationError {
    IdentityMismatch,
    InvalidPendingRange,
    CursorInsidePendingRecord,
    EmptyPendingGroup,
    NonContiguousPendingGroup,
    CursorBeforePendingRange,
}

impl fmt::Display for RecoveryClassificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "persistence recovery classification error: {self:?}"
        )
    }
}

impl std::error::Error for RecoveryClassificationError {}

/// Deterministic core for group ownership and recovery.
///
/// It is deliberately separate from the production thread. This makes the
/// uncertain-group rules testable before the threaded executor enables group
/// commit for live recording.
pub struct GroupedPersistenceWorker<S>
where
    S: GroupDurableMeasurementSink,
{
    sink: S,
    capacity: usize,
    max_group_records: usize,
    next_job_id: u64,
    pending: VecDeque<WriteCommand>,
    failed_group: VecDeque<WriteCommand>,
    completions: VecDeque<WriteCompletion<S::Error>>,
    faulted: bool,
}

impl<S> GroupedPersistenceWorker<S>
where
    S: GroupDurableMeasurementSink,
{
    pub fn new(
        sink: S,
        capacity: usize,
        max_group_records: usize,
    ) -> Result<Self, GroupedWriterError> {
        if capacity == 0 {
            return Err(GroupedWriterError::ZeroCapacity);
        }
        if max_group_records == 0 {
            return Err(GroupedWriterError::ZeroGroupLimit);
        }
        Ok(Self {
            sink,
            capacity,
            max_group_records,
            next_job_id: 1,
            pending: VecDeque::with_capacity(capacity),
            failed_group: VecDeque::new(),
            completions: VecDeque::with_capacity(capacity),
            faulted: false,
        })
    }

    pub fn process_one_group(&mut self) -> Result<usize, GroupedWriterError> {
        if self.faulted || self.pending.is_empty() {
            return Ok(0);
        }
        let group_len = self.contiguous_group_len();
        let result = {
            let envelopes = self
                .pending
                .iter()
                .take(group_len)
                .map(|command| &command.envelope)
                .collect::<Vec<_>>();
            self.sink.persist_group(&envelopes)
        };
        match result {
            Ok(success) => {
                let persisted = success.persisted_records;
                if persisted == 0 || persisted > group_len {
                    return Err(GroupedWriterError::InvalidSinkProgress);
                }
                for _ in 0..persisted {
                    let command = self.pending.pop_front().expect("validated group length");
                    self.completions.push_back(WriteCompletion {
                        id: command.id,
                        result: Ok(command.envelope.receipt()),
                    });
                }
                Ok(persisted)
            }
            Err(GroupPersistError::Preparation(error)) => {
                let command = self.pending.pop_front().expect("non-empty group");
                let id = command.id;
                self.failed_group.push_back(command);
                self.completions.push_back(WriteCompletion {
                    id,
                    result: Err(error),
                });
                self.faulted = true;
                Ok(1)
            }
            Err(GroupPersistError::Commit {
                attempted_records,
                error,
            }) => {
                if attempted_records == 0 || attempted_records > group_len {
                    return Err(GroupedWriterError::InvalidSinkProgress);
                }
                for _ in 0..attempted_records {
                    self.failed_group
                        .push_back(self.pending.pop_front().expect("validated group length"));
                }
                let id = self.failed_group.front().expect("non-empty failure").id;
                self.completions.push_back(WriteCompletion {
                    id,
                    result: Err(error),
                });
                self.faulted = true;
                Ok(attempted_records)
            }
        }
    }

    pub fn failed_ranges(&self) -> Vec<WriteRecoveryRange> {
        self.failed_group
            .iter()
            .map(|command| WriteRecoveryRange {
                hardware_id: command.envelope.hardware_id(),
                boot_epoch: command.envelope.boot_epoch(),
                start: command.envelope.start_cursor(),
                end: command.envelope.end_cursor(),
            })
            .collect()
    }

    pub fn replace_recovered_sink(
        &mut self,
        sink: S,
        evidence: RecoveredCursorEvidence,
    ) -> Result<GroupRecoveryDisposition, RecoveryClassificationError> {
        let decision = classify_group_recovery(&self.failed_ranges(), evidence)?;
        self.sink = sink;
        for _ in 0..decision.confirmed_prefix_records {
            let command = self
                .failed_group
                .pop_front()
                .expect("classification used the same failed group");
            self.completions.push_back(WriteCompletion {
                id: command.id,
                result: Ok(command.envelope.receipt()),
            });
        }
        while let Some(command) = self.failed_group.pop_back() {
            self.pending.push_front(command);
        }
        self.faulted = false;
        Ok(decision)
    }

    pub fn poll_completion(&mut self) -> Option<WriteCompletion<S::Error>> {
        self.completions.pop_front()
    }

    fn enqueue_assigned(&mut self, command: WriteCommand) -> bool {
        if self.faulted
            || self.pending.len() + self.failed_group.len() + self.completions.len()
                >= self.capacity
        {
            return false;
        }
        self.pending.push_back(command);
        true
    }

    pub const fn is_faulted(&self) -> bool {
        self.faulted
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn failed_count(&self) -> usize {
        self.failed_group.len()
    }

    fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }

    pub fn into_sink(self) -> S {
        self.sink
    }

    fn contiguous_group_len(&self) -> usize {
        let Some(first) = self.pending.front() else {
            return 0;
        };
        let mut expected = first.envelope.start_cursor();
        let mut count = 0;
        for command in self.pending.iter().take(self.max_group_records) {
            let envelope = &command.envelope;
            if envelope.hardware_id() != first.envelope.hardware_id()
                || envelope.boot_epoch() != first.envelope.boot_epoch()
                || envelope.start_cursor() != expected
            {
                break;
            }
            expected = envelope.end_cursor();
            count += 1;
        }
        count
    }
}

impl<S> PersistenceSubmitter for GroupedPersistenceWorker<S>
where
    S: GroupDurableMeasurementSink,
{
    fn submit_record(&mut self, envelope: PersistenceEnvelope) -> Result<WriteJobId, SubmitError> {
        if self.faulted {
            return Err(SubmitError::new(SubmitErrorKind::WorkerFaulted, envelope));
        }
        if self.pending.len() + self.failed_group.len() + self.completions.len() >= self.capacity {
            return Err(SubmitError::new(SubmitErrorKind::QueueFull, envelope));
        }
        let id = WriteJobId::new(self.next_job_id);
        let Some(next) = self.next_job_id.checked_add(1) else {
            return Err(SubmitError::new(SubmitErrorKind::JobIdExhausted, envelope));
        };
        self.next_job_id = next;
        self.pending.push_back(WriteCommand { id, envelope });
        Ok(id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GroupedWriterError {
    ZeroCapacity,
    ZeroGroupLimit,
    InvalidSinkProgress,
}

impl fmt::Display for GroupedWriterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "grouped persistence writer error: {self:?}")
    }
}

impl std::error::Error for GroupedWriterError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GroupCommitPolicy {
    max_records: usize,
    max_wait: Duration,
}

impl GroupCommitPolicy {
    pub const fn max_records(self) -> usize {
        self.max_records
    }

    pub const fn max_wait(self) -> Duration {
        self.max_wait
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GroupCommitMetricsSnapshot {
    pub committed_groups: u64,
    pub committed_records: u64,
    pub failed_groups: u64,
    pub maximum_persist_us: u64,
    pub maximum_group_records: u64,
    pub queue_high_water: u64,
}

#[derive(Default)]
struct GroupCommitMetricsAtomic {
    committed_groups: AtomicU64,
    committed_records: AtomicU64,
    failed_groups: AtomicU64,
    maximum_persist_us: AtomicU64,
    maximum_group_records: AtomicU64,
    queue_high_water: AtomicU64,
}

struct GroupedWorkerTelemetry {
    in_flight: Arc<AtomicUsize>,
    metrics: Arc<GroupCommitMetricsAtomic>,
}

#[derive(Clone)]
pub struct GroupCommitMetricsReader {
    metrics: Arc<GroupCommitMetricsAtomic>,
}

impl GroupCommitMetricsReader {
    pub fn snapshot(&self) -> GroupCommitMetricsSnapshot {
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        GroupCommitMetricsSnapshot {
            committed_groups: load(&self.metrics.committed_groups),
            committed_records: load(&self.metrics.committed_records),
            failed_groups: load(&self.metrics.failed_groups),
            maximum_persist_us: load(&self.metrics.maximum_persist_us),
            maximum_group_records: load(&self.metrics.maximum_group_records),
            queue_high_water: load(&self.metrics.queue_high_water),
        }
    }
}

impl GroupCommitPolicy {
    pub fn new(max_records: usize, max_wait: Duration) -> Result<Self, GroupedWriterError> {
        if max_records == 0 {
            return Err(GroupedWriterError::ZeroGroupLimit);
        }
        Ok(Self {
            max_records,
            max_wait,
        })
    }
}

enum GroupRecoveryCommand<S> {
    Replace {
        sink: S,
        evidence: RecoveredCursorEvidence,
        result: SyncSender<Result<GroupRecoveryDisposition, RecoveryClassificationError>>,
    },
    Shutdown,
}

type GroupManagementOperation<S> = Box<dyn FnOnce(&mut S) + Send>;

enum GroupExecutorCommand<S> {
    Write(WriteCommand),
    Manage(GroupManagementOperation<S>),
}

/// Threaded group-commit executor. The producer never performs disk I/O.
pub struct GroupedPersistenceExecutor<S>
where
    S: GroupDurableMeasurementSink + Send + 'static,
    S::Error: Send + 'static,
{
    writes: Option<SyncSender<GroupExecutorCommand<S>>>,
    recovery: Option<SyncSender<GroupRecoveryCommand<S>>>,
    completions: Receiver<WriteCompletion<S::Error>>,
    stopped_sink: Receiver<S>,
    thread: Option<JoinHandle<()>>,
    in_flight: Arc<AtomicUsize>,
    metrics: Arc<GroupCommitMetricsAtomic>,
    capacity: usize,
    next_job_id: u64,
}

impl<S> GroupedPersistenceExecutor<S>
where
    S: GroupDurableMeasurementSink + Send + 'static,
    S::Error: Send + 'static,
{
    pub fn start(
        sink: S,
        capacity: usize,
        policy: GroupCommitPolicy,
        worker_name: impl Into<String>,
    ) -> Result<Self, PersistenceExecutorError> {
        if capacity == 0 {
            return Err(PersistenceExecutorError::ZeroCapacity);
        }
        let (write_tx, write_rx) = mpsc::sync_channel(capacity);
        let (recovery_tx, recovery_rx) = mpsc::sync_channel(1);
        let (completion_tx, completion_rx) = mpsc::sync_channel(capacity);
        let (stopped_tx, stopped_rx) = mpsc::sync_channel(1);
        let in_flight = Arc::new(AtomicUsize::new(0));
        let metrics = Arc::new(GroupCommitMetricsAtomic::default());
        let worker_telemetry = GroupedWorkerTelemetry {
            in_flight: Arc::clone(&in_flight),
            metrics: Arc::clone(&metrics),
        };
        let thread = thread::Builder::new()
            .name(worker_name.into())
            .spawn(move || {
                let sink = run_grouped_executor(
                    sink,
                    capacity,
                    policy,
                    write_rx,
                    recovery_rx,
                    completion_tx,
                    worker_telemetry,
                );
                let _ = stopped_tx.send(sink);
            })
            .map_err(PersistenceExecutorError::Spawn)?;
        Ok(Self {
            writes: Some(write_tx),
            recovery: Some(recovery_tx),
            completions: completion_rx,
            stopped_sink: stopped_rx,
            thread: Some(thread),
            in_flight,
            metrics,
            capacity,
            next_job_id: 1,
        })
    }

    pub fn try_completion(
        &self,
    ) -> Result<Option<WriteCompletion<S::Error>>, PersistenceExecutorError> {
        match self.completions.try_recv() {
            Ok(value) => Ok(Some(value)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(PersistenceExecutorError::WorkerDisconnected),
        }
    }

    pub fn metrics_reader(&self) -> GroupCommitMetricsReader {
        GroupCommitMetricsReader {
            metrics: Arc::clone(&self.metrics),
        }
    }

    pub fn replace_recovered_sink(
        &self,
        sink: S,
        evidence: RecoveredCursorEvidence,
    ) -> Result<GroupRecoveryDisposition, GroupedExecutorRecoveryError> {
        let recovery = self
            .recovery
            .as_ref()
            .ok_or(GroupedExecutorRecoveryError::Executor(
                PersistenceExecutorError::AlreadyShutdown,
            ))?;
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        recovery
            .try_send(GroupRecoveryCommand::Replace {
                sink,
                evidence,
                result: result_tx,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => GroupedExecutorRecoveryError::Executor(
                    PersistenceExecutorError::RecoveryAlreadyPending,
                ),
                TrySendError::Disconnected(_) => GroupedExecutorRecoveryError::Executor(
                    PersistenceExecutorError::WorkerDisconnected,
                ),
            })?;
        result_rx
            .recv()
            .map_err(|_| {
                GroupedExecutorRecoveryError::Executor(PersistenceExecutorError::WorkerDisconnected)
            })?
            .map_err(GroupedExecutorRecoveryError::Classification)
    }

    /// Runs a bounded management operation on the exclusive storage owner.
    /// Callers must first drain all submitted writes and their completions.
    pub(crate) fn manage_sink<R, F>(&self, operation: F) -> Result<R, PersistenceExecutorError>
    where
        R: Send + 'static,
        F: FnOnce(&mut S) -> R + Send + 'static,
    {
        if self.in_flight.load(Ordering::Acquire) != 0 {
            return Err(PersistenceExecutorError::ManagementRequiresIdle);
        }
        let writes = self
            .writes
            .as_ref()
            .ok_or(PersistenceExecutorError::AlreadyShutdown)?;
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let command = GroupExecutorCommand::Manage(Box::new(move |sink| {
            let _ = result_tx.send(operation(sink));
        }));
        writes.try_send(command).map_err(|error| match error {
            TrySendError::Full(_) => PersistenceExecutorError::ManagementAlreadyPending,
            TrySendError::Disconnected(_) => PersistenceExecutorError::WorkerDisconnected,
        })?;
        result_rx
            .recv()
            .map_err(|_| PersistenceExecutorError::WorkerDisconnected)
    }

    pub fn shutdown(
        mut self,
    ) -> Result<PersistenceShutdown<S, S::Error>, PersistenceExecutorError> {
        if let Some(recovery) = self.recovery.take() {
            let _ = recovery.try_send(GroupRecoveryCommand::Shutdown);
        }
        self.writes.take();
        let completions = self.completions.iter().collect();
        let thread = self
            .thread
            .take()
            .ok_or(PersistenceExecutorError::AlreadyShutdown)?;
        thread
            .join()
            .map_err(|_| PersistenceExecutorError::WorkerPanicked)?;
        let sink = self
            .stopped_sink
            .recv()
            .map_err(|_| PersistenceExecutorError::RecoveredSinkUnavailable)?;
        Ok(PersistenceShutdown { sink, completions })
    }
}

impl<S> PersistenceSubmitter for GroupedPersistenceExecutor<S>
where
    S: GroupDurableMeasurementSink + Send + 'static,
    S::Error: Send + 'static,
{
    fn submit_record(&mut self, envelope: PersistenceEnvelope) -> Result<WriteJobId, SubmitError> {
        let permit = self
            .in_flight
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                (value < self.capacity).then_some(value + 1)
            });
        if permit.is_err() {
            return Err(SubmitError::new(SubmitErrorKind::QueueFull, envelope));
        }
        let queue_depth = permit.expect("successful permit was checked") + 1;
        self.metrics
            .queue_high_water
            .fetch_max(queue_depth as u64, Ordering::Relaxed);
        let id = WriteJobId::new(self.next_job_id);
        let Some(next) = self.next_job_id.checked_add(1) else {
            self.in_flight.fetch_sub(1, Ordering::AcqRel);
            return Err(SubmitError::new(SubmitErrorKind::JobIdExhausted, envelope));
        };
        let command = WriteCommand { id, envelope };
        let result = match self.writes.as_ref() {
            Some(writes) => writes.try_send(GroupExecutorCommand::Write(command)),
            None => Err(TrySendError::Disconnected(GroupExecutorCommand::Write(
                command,
            ))),
        };
        match result {
            Ok(()) => {
                self.next_job_id = next;
                Ok(id)
            }
            Err(error) => {
                self.in_flight.fetch_sub(1, Ordering::AcqRel);
                let (kind, command) = match error {
                    TrySendError::Full(GroupExecutorCommand::Write(command)) => {
                        (SubmitErrorKind::QueueFull, command)
                    }
                    TrySendError::Disconnected(GroupExecutorCommand::Write(command)) => {
                        (SubmitErrorKind::WorkerFaulted, command)
                    }
                    TrySendError::Full(GroupExecutorCommand::Manage(_))
                    | TrySendError::Disconnected(GroupExecutorCommand::Manage(_)) => {
                        unreachable!("submit_record only sends write commands")
                    }
                };
                Err(SubmitError::new(kind, command.envelope))
            }
        }
    }
}

impl<S> Drop for GroupedPersistenceExecutor<S>
where
    S: GroupDurableMeasurementSink + Send + 'static,
    S::Error: Send + 'static,
{
    fn drop(&mut self) {
        if let Some(recovery) = self.recovery.take() {
            let _ = recovery.try_send(GroupRecoveryCommand::Shutdown);
        }
        self.writes.take();
        self.thread.take();
    }
}

#[derive(Debug)]
pub enum GroupedExecutorRecoveryError {
    Executor(PersistenceExecutorError),
    Classification(RecoveryClassificationError),
}

impl fmt::Display for GroupedExecutorRecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "grouped executor recovery failed: {self:?}")
    }
}

impl std::error::Error for GroupedExecutorRecoveryError {}

fn run_grouped_executor<S>(
    sink: S,
    capacity: usize,
    policy: GroupCommitPolicy,
    writes: Receiver<GroupExecutorCommand<S>>,
    recovery: Receiver<GroupRecoveryCommand<S>>,
    completions: SyncSender<WriteCompletion<S::Error>>,
    telemetry: GroupedWorkerTelemetry,
) -> S
where
    S: GroupDurableMeasurementSink,
{
    let mut worker = GroupedPersistenceWorker::new(sink, capacity, policy.max_records)
        .expect("executor validates grouped worker limits");
    let mut pending_management: Option<GroupManagementOperation<S>> = None;
    loop {
        if worker.is_faulted() {
            match recovery.recv() {
                Ok(GroupRecoveryCommand::Replace {
                    sink,
                    evidence,
                    result,
                }) => {
                    let decision = worker.replace_recovered_sink(sink, evidence);
                    let recovered = decision.is_ok();
                    let _ = result.send(decision);
                    if recovered
                        && !forward_grouped_completions(
                            &mut worker,
                            &completions,
                            &telemetry.in_flight,
                        )
                    {
                        break;
                    }
                }
                Ok(GroupRecoveryCommand::Shutdown) | Err(_) => break,
            }
            continue;
        }

        if worker.pending_count() == 0 {
            if let Some(operation) = pending_management.take() {
                operation(worker.sink_mut());
                continue;
            }
            let first = match writes.recv() {
                Ok(GroupExecutorCommand::Write(command)) => command,
                Ok(GroupExecutorCommand::Manage(operation)) => {
                    operation(worker.sink_mut());
                    continue;
                }
                Err(_) => break,
            };
            let hardware_id = first.envelope.hardware_id();
            let boot_epoch = first.envelope.boot_epoch();
            let mut expected = first.envelope.end_cursor();
            if !worker.enqueue_assigned(first) {
                break;
            }
            let deadline = Instant::now().checked_add(policy.max_wait);
            while worker.pending_count() < policy.max_records {
                let remaining = deadline
                    .and_then(|value| value.checked_duration_since(Instant::now()))
                    .unwrap_or(Duration::ZERO);
                let command = match writes.recv_timeout(remaining) {
                    Ok(GroupExecutorCommand::Write(command)) => command,
                    Ok(GroupExecutorCommand::Manage(operation)) => {
                        pending_management = Some(operation);
                        break;
                    }
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => break,
                };
                let contiguous = command.envelope.hardware_id() == hardware_id
                    && command.envelope.boot_epoch() == boot_epoch
                    && command.envelope.start_cursor() == expected;
                expected = command.envelope.end_cursor();
                if !worker.enqueue_assigned(command) {
                    break;
                }
                if !contiguous {
                    break;
                }
            }
        }

        let started = Instant::now();
        let persisted = worker.process_one_group();
        let elapsed_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        telemetry
            .metrics
            .maximum_persist_us
            .fetch_max(elapsed_us, Ordering::Relaxed);
        match persisted {
            Ok(records) if records > 0 && worker.is_faulted() => {
                increment_atomic(&telemetry.metrics.failed_groups, 1);
            }
            Ok(records) if records > 0 => {
                increment_atomic(&telemetry.metrics.committed_groups, 1);
                increment_atomic(&telemetry.metrics.committed_records, records as u64);
                telemetry
                    .metrics
                    .maximum_group_records
                    .fetch_max(records as u64, Ordering::Relaxed);
            }
            Err(_) => increment_atomic(&telemetry.metrics.failed_groups, 1),
            Ok(_) => {}
        }
        if persisted.is_err()
            || !forward_grouped_completions(&mut worker, &completions, &telemetry.in_flight)
        {
            break;
        }
    }
    worker.into_sink()
}

fn increment_atomic(counter: &AtomicU64, amount: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(amount))
    });
}

fn forward_grouped_completions<S>(
    worker: &mut GroupedPersistenceWorker<S>,
    output: &SyncSender<WriteCompletion<S::Error>>,
    in_flight: &AtomicUsize,
) -> bool
where
    S: GroupDurableMeasurementSink,
{
    while let Some(completion) = worker.poll_completion() {
        let durable = completion.result.is_ok();
        if output.send(completion).is_err() {
            return false;
        }
        if durable {
            in_flight.fetch_sub(1, Ordering::AcqRel);
        }
    }
    true
}

/// Producer-side handle for one bounded storage executor.
///
/// `submit_record` never waits for disk I/O. The bounded synchronous channel
/// applies backpressure before sample memory can grow without limit.
pub struct PersistenceExecutor<S>
where
    S: DurableMeasurementSink + Send + 'static,
    S::Error: Send + 'static,
{
    writes: Option<SyncSender<WriteCommand>>,
    recovery: Option<SyncSender<RecoveryCommand<S>>>,
    completions: Receiver<WriteCompletion<S::Error>>,
    stopped_sink: Receiver<S>,
    thread: Option<JoinHandle<()>>,
    next_job_id: u64,
}

impl<S> PersistenceExecutor<S>
where
    S: DurableMeasurementSink + Send + 'static,
    S::Error: Send + 'static,
{
    pub fn start(
        sink: S,
        capacity: usize,
        thread_name: impl Into<String>,
    ) -> Result<Self, PersistenceExecutorError> {
        if capacity == 0 {
            return Err(PersistenceExecutorError::ZeroCapacity);
        }
        let (write_tx, write_rx) = mpsc::sync_channel(capacity);
        let (recovery_tx, recovery_rx) = mpsc::sync_channel(1);
        let (completion_tx, completion_rx) = mpsc::sync_channel(capacity + 1);
        let (stopped_tx, stopped_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || {
                let sink = run_executor(sink, write_rx, recovery_rx, completion_tx);
                let _ = stopped_tx.send(sink);
            })
            .map_err(PersistenceExecutorError::Spawn)?;
        Ok(Self {
            writes: Some(write_tx),
            recovery: Some(recovery_tx),
            completions: completion_rx,
            stopped_sink: stopped_rx,
            thread: Some(thread),
            next_job_id: 1,
        })
    }

    pub fn try_completion(
        &self,
    ) -> Result<Option<WriteCompletion<S::Error>>, PersistenceExecutorError> {
        match self.completions.try_recv() {
            Ok(completion) => Ok(Some(completion)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(PersistenceExecutorError::WorkerDisconnected),
        }
    }

    /// Supplies a sink which has already recovered and reconciled its durable
    /// cursor. The executor retries its retained uncertain job first.
    pub fn replace_recovered_sink(
        &self,
        sink: S,
        disposition: RecoveryDisposition,
    ) -> Result<(), PersistenceExecutorError> {
        let recovery = self
            .recovery
            .as_ref()
            .ok_or(PersistenceExecutorError::AlreadyShutdown)?;
        recovery
            .try_send(RecoveryCommand::ReplaceSink { sink, disposition })
            .map_err(|error| match error {
                TrySendError::Full(_) => PersistenceExecutorError::RecoveryAlreadyPending,
                TrySendError::Disconnected(_) => PersistenceExecutorError::WorkerDisconnected,
            })
    }

    pub fn shutdown(
        mut self,
    ) -> Result<PersistenceShutdown<S, S::Error>, PersistenceExecutorError> {
        if let Some(recovery) = self.recovery.take() {
            let _ = recovery.try_send(RecoveryCommand::Shutdown);
        }
        self.writes.take();
        let completions = self.completions.iter().collect();
        let Some(thread) = self.thread.take() else {
            return Err(PersistenceExecutorError::AlreadyShutdown);
        };
        thread
            .join()
            .map_err(|_| PersistenceExecutorError::WorkerPanicked)?;
        let sink = self
            .stopped_sink
            .recv()
            .map_err(|_| PersistenceExecutorError::RecoveredSinkUnavailable)?;
        Ok(PersistenceShutdown { sink, completions })
    }

    fn allocate_job_id(&mut self) -> Option<WriteJobId> {
        let id = WriteJobId::new(self.next_job_id);
        self.next_job_id = self.next_job_id.checked_add(1)?;
        Some(id)
    }
}

#[derive(Debug)]
pub struct PersistenceShutdown<S, E> {
    pub sink: S,
    pub completions: Vec<WriteCompletion<E>>,
}

impl<S> PersistenceSubmitter for PersistenceExecutor<S>
where
    S: DurableMeasurementSink + Send + 'static,
    S::Error: Send + 'static,
{
    fn submit_record(&mut self, envelope: PersistenceEnvelope) -> Result<WriteJobId, SubmitError> {
        let Some(id) = self.allocate_job_id() else {
            return Err(SubmitError::new(SubmitErrorKind::JobIdExhausted, envelope));
        };
        let Some(writes) = self.writes.as_ref() else {
            return Err(SubmitError::new(SubmitErrorKind::WorkerFaulted, envelope));
        };
        match writes.try_send(WriteCommand { id, envelope }) {
            Ok(()) => Ok(id),
            Err(TrySendError::Full(command)) => Err(SubmitError::new(
                SubmitErrorKind::QueueFull,
                command.envelope,
            )),
            Err(TrySendError::Disconnected(command)) => Err(SubmitError::new(
                SubmitErrorKind::WorkerFaulted,
                command.envelope,
            )),
        }
    }
}

impl<S> Drop for PersistenceExecutor<S>
where
    S: DurableMeasurementSink + Send + 'static,
    S::Error: Send + 'static,
{
    fn drop(&mut self) {
        if let Some(recovery) = self.recovery.take() {
            let _ = recovery.try_send(RecoveryCommand::Shutdown);
        }
        self.writes.take();
        // Dropping JoinHandle detaches. Channel disconnection or the explicit
        // recovery shutdown still causes the worker to release its sink.
        self.thread.take();
    }
}

fn run_executor<S>(
    mut sink: S,
    writes: Receiver<WriteCommand>,
    recovery: Receiver<RecoveryCommand<S>>,
    completions: SyncSender<WriteCompletion<S::Error>>,
) -> S
where
    S: DurableMeasurementSink,
{
    run_executor_loop(&mut sink, &writes, &recovery, &completions);
    sink
}

fn run_executor_loop<S>(
    sink: &mut S,
    writes: &Receiver<WriteCommand>,
    recovery: &Receiver<RecoveryCommand<S>>,
    completions: &SyncSender<WriteCompletion<S::Error>>,
) where
    S: DurableMeasurementSink,
{
    while let Ok(command) = writes.recv() {
        loop {
            match command.envelope.persist(sink) {
                Ok(()) => {
                    if completions
                        .send(WriteCompletion {
                            id: command.id,
                            result: Ok(command.envelope.receipt()),
                        })
                        .is_err()
                    {
                        return;
                    }
                    break;
                }
                Err(error) => {
                    if completions
                        .send(WriteCompletion {
                            id: command.id,
                            result: Err(error),
                        })
                        .is_err()
                    {
                        return;
                    }
                    match recovery.recv() {
                        Ok(RecoveryCommand::ReplaceSink {
                            sink: recovered,
                            disposition,
                        }) => {
                            *sink = recovered;
                            if disposition == RecoveryDisposition::ConfirmAlreadyDurable {
                                if completions
                                    .send(WriteCompletion {
                                        id: command.id,
                                        result: Ok(command.envelope.receipt()),
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                                break;
                            }
                        }
                        Ok(RecoveryCommand::Shutdown) | Err(_) => return,
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
pub enum PersistenceExecutorError {
    ZeroCapacity,
    Spawn(std::io::Error),
    WorkerDisconnected,
    RecoveryAlreadyPending,
    ManagementRequiresIdle,
    ManagementAlreadyPending,
    AlreadyShutdown,
    WorkerPanicked,
    RecoveredSinkUnavailable,
}

impl fmt::Display for PersistenceExecutorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroCapacity => formatter.write_str("persistence executor capacity is zero"),
            Self::Spawn(error) => write!(formatter, "cannot start persistence executor: {error}"),
            Self::WorkerDisconnected => formatter.write_str("persistence worker disconnected"),
            Self::RecoveryAlreadyPending => {
                formatter.write_str("a recovered sink is already pending")
            }
            Self::ManagementRequiresIdle => {
                formatter.write_str("persistence management requires an idle executor")
            }
            Self::ManagementAlreadyPending => {
                formatter.write_str("a persistence management operation is already pending")
            }
            Self::AlreadyShutdown => formatter.write_str("persistence executor is shut down"),
            Self::WorkerPanicked => formatter.write_str("persistence worker panicked"),
            Self::RecoveredSinkUnavailable => {
                formatter.write_str("persistence worker did not return its sink")
            }
        }
    }
}

impl std::error::Error for PersistenceExecutorError {}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;
    use sensor_core::{BootEpoch, ContinuityBreak, Gap, HardwareId, SampleAxes, SampleAxis};
    use sensor_protocol::{SampleChunk, SampleChunkHeader, TimingEvidence, TIMING_VALID};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::bounded_writer::{GroupPersistError, GroupPersistSuccess};
    use crate::capture_repository::{CaptureRepository, CaptureStreamSpec};
    use crate::capture_wal::CaptureWalConfig;
    use crate::capture_window::CaptureWindowPolicy;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct SinkError;

    struct Sink {
        fail: bool,
    }

    impl DurableMeasurementSink for Sink {
        type Error = SinkError;

        fn persist_chunk(
            &mut self,
            _hardware_id: HardwareId,
            _chunk: &SampleChunk,
        ) -> Result<(), Self::Error> {
            if self.fail {
                Err(SinkError)
            } else {
                Ok(())
            }
        }

        fn persist_gap(&mut self, _gap: Gap) -> Result<(), Self::Error> {
            unreachable!()
        }

        fn persist_continuity_break(
            &mut self,
            _observed: ContinuityBreak,
        ) -> Result<(), Self::Error> {
            unreachable!()
        }
    }

    impl GroupDurableMeasurementSink for Sink {
        fn persist_group(
            &mut self,
            envelopes: &[&PersistenceEnvelope],
        ) -> Result<GroupPersistSuccess, GroupPersistError<Self::Error>> {
            if self.fail {
                return Err(GroupPersistError::Commit {
                    attempted_records: envelopes.len(),
                    error: SinkError,
                });
            }
            for envelope in envelopes {
                envelope
                    .persist(self)
                    .map_err(GroupPersistError::Preparation)?;
            }
            Ok(GroupPersistSuccess {
                persisted_records: envelopes.len(),
            })
        }
    }

    fn envelope() -> PersistenceEnvelope {
        envelope_at(0)
    }

    fn envelope_at(first: u64) -> PersistenceEnvelope {
        let axis = SampleAxis::new(7).unwrap();
        PersistenceEnvelope::chunk(
            HardwareId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0]),
            SampleChunk {
                header: SampleChunkHeader {
                    boot_epoch: BootEpoch::new(1),
                    retained_from: sensor_core::SequenceCursor::new(first),
                    produced: sensor_core::SequenceCursor::new(first + 1),
                    first: sensor_core::SequenceCursor::new(first),
                    sample_count: 1,
                },
                config_revision: 1,
                timing: TimingEvidence {
                    segment_id: 1,
                    first_device_time_us: 1,
                    last_device_time_us: 1,
                    max_fit_residual_us: 0,
                    quality_flags: TIMING_VALID,
                },
                samples: vec![SampleAxes::new(axis, axis, axis)],
            },
        )
        .unwrap()
    }

    fn wait_completion(executor: &PersistenceExecutor<Sink>) -> WriteCompletion<SinkError> {
        for _ in 0..100 {
            if let Some(completion) = executor.try_completion().unwrap() {
                return completion;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("storage completion did not arrive");
    }

    fn wait_grouped_completion(
        executor: &GroupedPersistenceExecutor<Sink>,
    ) -> WriteCompletion<SinkError> {
        for _ in 0..200 {
            if let Some(completion) = executor.try_completion().unwrap() {
                return completion;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("grouped storage completion did not arrive");
    }

    #[test]
    fn failed_job_is_retried_with_the_same_id_after_sink_recovery() {
        let mut executor =
            PersistenceExecutor::start(Sink { fail: true }, 2, "test-persistence-executor")
                .unwrap();
        let id = executor.submit_record(envelope()).unwrap();
        let failed = wait_completion(&executor);
        assert_eq!(failed.id, id);
        assert_eq!(failed.result, Err(SinkError));

        executor
            .replace_recovered_sink(Sink { fail: false }, RecoveryDisposition::RetryUncommitted)
            .unwrap();
        let durable = wait_completion(&executor);
        assert_eq!(durable.id, id);
        assert_eq!(durable.result.unwrap().end.get(), 1);
        let shutdown = executor.shutdown().unwrap();
        assert!(!shutdown.sink.fail);
        assert!(shutdown.completions.is_empty());
    }

    #[test]
    fn recovered_durable_record_is_confirmed_without_duplicate_append() {
        let mut executor =
            PersistenceExecutor::start(Sink { fail: true }, 1, "test-persistence-reconcile")
                .unwrap();
        let id = executor.submit_record(envelope()).unwrap();
        assert_eq!(wait_completion(&executor).result, Err(SinkError));

        executor
            .replace_recovered_sink(
                Sink { fail: true },
                RecoveryDisposition::ConfirmAlreadyDurable,
            )
            .unwrap();
        let completion = wait_completion(&executor);
        assert_eq!(completion.id, id);
        assert_eq!(completion.result.unwrap().end.get(), 1);
        executor.shutdown().unwrap();
    }

    #[test]
    fn recovery_disposition_requires_matching_identity_and_record_boundary() {
        let hardware_id = HardwareId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0]);
        let pending = WriteRecoveryRange {
            hardware_id,
            boot_epoch: BootEpoch::new(1),
            start: sensor_core::SequenceCursor::new(10),
            end: sensor_core::SequenceCursor::new(20),
        };
        let evidence = |durable| RecoveredCursorEvidence {
            hardware_id,
            boot_epoch: BootEpoch::new(1),
            durable: sensor_core::SequenceCursor::new(durable),
        };
        assert_eq!(
            classify_recovery(pending, evidence(10)),
            Ok(RecoveryDisposition::RetryUncommitted)
        );
        assert_eq!(
            classify_recovery(pending, evidence(20)),
            Ok(RecoveryDisposition::ConfirmAlreadyDurable)
        );
        assert_eq!(
            classify_recovery(pending, evidence(15)),
            Err(RecoveryClassificationError::CursorInsidePendingRecord)
        );
        assert!(matches!(
            classify_recovery(
                pending,
                RecoveredCursorEvidence {
                    hardware_id: HardwareId::from_bytes([2, 0, 0, 0, 0, 0, 0, 0]),
                    ..evidence(10)
                }
            ),
            Err(RecoveryClassificationError::IdentityMismatch)
        ));
    }

    #[test]
    fn group_recovery_accepts_only_complete_contiguous_prefixes() {
        let hardware_id = HardwareId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0]);
        let boot_epoch = BootEpoch::new(1);
        let range = |start, end| WriteRecoveryRange {
            hardware_id,
            boot_epoch,
            start: SequenceCursor::new(start),
            end: SequenceCursor::new(end),
        };
        let pending = [range(10, 12), range(12, 17), range(17, 18)];
        let evidence = |durable| RecoveredCursorEvidence {
            hardware_id,
            boot_epoch,
            durable: SequenceCursor::new(durable),
        };

        assert_eq!(
            classify_group_recovery(&pending, evidence(10)),
            Ok(GroupRecoveryDisposition {
                confirmed_prefix_records: 0
            })
        );
        assert_eq!(
            classify_group_recovery(&pending, evidence(12)),
            Ok(GroupRecoveryDisposition {
                confirmed_prefix_records: 1
            })
        );
        assert_eq!(
            classify_group_recovery(&pending, evidence(17)),
            Ok(GroupRecoveryDisposition {
                confirmed_prefix_records: 2
            })
        );
        assert_eq!(
            classify_group_recovery(&pending, evidence(18)),
            Ok(GroupRecoveryDisposition {
                confirmed_prefix_records: 3
            })
        );
        assert_eq!(
            classify_group_recovery(&pending, evidence(100)),
            Ok(GroupRecoveryDisposition {
                confirmed_prefix_records: 3
            })
        );
        assert_eq!(
            classify_group_recovery(&pending, evidence(15)),
            Err(RecoveryClassificationError::CursorInsidePendingRecord)
        );
    }

    #[test]
    fn group_recovery_rejects_an_invalid_group_before_deciding() {
        let hardware_id = HardwareId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0]);
        let boot_epoch = BootEpoch::new(1);
        let range = |start, end| WriteRecoveryRange {
            hardware_id,
            boot_epoch,
            start: SequenceCursor::new(start),
            end: SequenceCursor::new(end),
        };
        let evidence = RecoveredCursorEvidence {
            hardware_id,
            boot_epoch,
            durable: SequenceCursor::new(10),
        };

        assert_eq!(
            classify_group_recovery(&[], evidence),
            Err(RecoveryClassificationError::EmptyPendingGroup)
        );
        assert_eq!(
            classify_group_recovery(&[range(10, 12), range(13, 14)], evidence),
            Err(RecoveryClassificationError::NonContiguousPendingGroup)
        );
        assert_eq!(
            classify_group_recovery(&[range(11, 12)], evidence),
            Err(RecoveryClassificationError::CursorBeforePendingRange)
        );
        assert_eq!(
            classify_group_recovery(&[range(10, 10)], evidence),
            Err(RecoveryClassificationError::InvalidPendingRange)
        );
    }

    #[test]
    fn grouped_worker_confirms_recovered_prefix_and_retries_only_the_suffix() {
        let mut worker = GroupedPersistenceWorker::new(Sink { fail: true }, 8, 3).unwrap();
        let first = worker.submit_record(envelope_at(0)).unwrap();
        let second = worker.submit_record(envelope_at(1)).unwrap();
        let third = worker.submit_record(envelope_at(2)).unwrap();

        assert_eq!(worker.process_one_group(), Ok(3));
        assert!(worker.is_faulted());
        assert_eq!(worker.failed_count(), 3);
        let failure = worker.poll_completion().unwrap();
        assert_eq!(failure.id, first);
        assert_eq!(failure.result, Err(SinkError));

        let decision = worker
            .replace_recovered_sink(
                Sink { fail: false },
                RecoveredCursorEvidence {
                    hardware_id: HardwareId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0]),
                    boot_epoch: BootEpoch::new(1),
                    durable: SequenceCursor::new(1),
                },
            )
            .unwrap();
        assert_eq!(decision.confirmed_prefix_records, 1);
        assert_eq!(worker.failed_count(), 0);
        assert_eq!(worker.pending_count(), 2);
        let recovered = worker.poll_completion().unwrap();
        assert_eq!(recovered.id, first);
        assert_eq!(recovered.result.unwrap().end, SequenceCursor::new(1));

        assert_eq!(worker.process_one_group(), Ok(2));
        let second_completion = worker.poll_completion().unwrap();
        let third_completion = worker.poll_completion().unwrap();
        assert_eq!(second_completion.id, second);
        assert_eq!(third_completion.id, third);
        assert_eq!(
            second_completion.result.unwrap().end,
            SequenceCursor::new(2)
        );
        assert_eq!(third_completion.result.unwrap().end, SequenceCursor::new(3));
    }

    #[test]
    fn grouped_worker_rejects_unbounded_configuration() {
        assert!(matches!(
            GroupedPersistenceWorker::new(Sink { fail: false }, 0, 1),
            Err(GroupedWriterError::ZeroCapacity)
        ));
        assert!(matches!(
            GroupedPersistenceWorker::new(Sink { fail: false }, 1, 0),
            Err(GroupedWriterError::ZeroGroupLimit)
        ));
    }

    #[test]
    fn threaded_grouped_executor_bounds_and_recovers_the_complete_group() {
        let policy = GroupCommitPolicy::new(3, Duration::from_millis(10)).unwrap();
        let mut executor = GroupedPersistenceExecutor::start(
            Sink { fail: true },
            3,
            policy,
            "test-threaded-grouped",
        )
        .unwrap();
        let metrics = executor.metrics_reader();
        let first = executor.submit_record(envelope_at(0)).unwrap();
        let second = executor.submit_record(envelope_at(1)).unwrap();
        let third = executor.submit_record(envelope_at(2)).unwrap();
        assert_eq!(
            executor.submit_record(envelope_at(3)).unwrap_err().kind(),
            SubmitErrorKind::QueueFull
        );

        let failed = wait_grouped_completion(&executor);
        assert_eq!(failed.id, first);
        assert_eq!(failed.result, Err(SinkError));
        let decision = executor
            .replace_recovered_sink(
                Sink { fail: false },
                RecoveredCursorEvidence {
                    hardware_id: HardwareId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0]),
                    boot_epoch: BootEpoch::new(1),
                    durable: SequenceCursor::new(1),
                },
            )
            .unwrap();
        assert_eq!(decision.confirmed_prefix_records, 1);

        let completions = [
            wait_grouped_completion(&executor),
            wait_grouped_completion(&executor),
            wait_grouped_completion(&executor),
        ];
        assert_eq!(completions[0].id, first);
        assert_eq!(completions[1].id, second);
        assert_eq!(completions[2].id, third);
        assert!(completions.iter().all(|value| value.result.is_ok()));
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.failed_groups, 1);
        assert_eq!(snapshot.committed_groups, 1);
        assert_eq!(snapshot.committed_records, 2);
        assert_eq!(snapshot.maximum_group_records, 2);
        assert_eq!(snapshot.queue_high_water, 3);
        executor.shutdown().unwrap();
    }

    #[test]
    fn zero_capacity_is_rejected_before_spawning_a_thread() {
        assert!(matches!(
            PersistenceExecutor::start(Sink { fail: false }, 0, "never-started"),
            Err(PersistenceExecutorError::ZeroCapacity)
        ));
    }

    #[test]
    fn real_capture_repository_runs_exclusively_on_the_storage_thread() {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "sensor-host-threaded-capture-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let repository = CaptureRepository::open([CaptureStreamSpec {
            directory: root.join("node-1"),
            config: CaptureWalConfig {
                hardware_id: HardwareId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0]),
                boot_epoch: BootEpoch::new(1),
                recovered_cursor: sensor_core::SequenceCursor::ORIGIN,
                first_window_ordinal: 0,
                window_policy: CaptureWindowPolicy::new(600_000_000).unwrap(),
            },
        }])
        .unwrap();
        let mut executor = PersistenceExecutor::start(repository, 2, "test-real-capture").unwrap();
        let job_id = executor.submit_record(envelope()).unwrap();
        let completion = wait_repository_completion(&executor);
        assert_eq!(completion.id, job_id);
        assert_eq!(completion.result.unwrap().end.get(), 1);

        let shutdown = executor.shutdown().unwrap();
        assert!(shutdown.completions.is_empty());
        assert_eq!(shutdown.sink.durable_cursors().next().unwrap().2.get(), 1);
        drop(shutdown);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn grouped_worker_commits_real_capture_records_with_one_repository_batch() {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "sensor-host-grouped-capture-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let repository = CaptureRepository::open([CaptureStreamSpec {
            directory: root.join("node-1"),
            config: CaptureWalConfig {
                hardware_id: HardwareId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0]),
                boot_epoch: BootEpoch::new(1),
                recovered_cursor: SequenceCursor::ORIGIN,
                first_window_ordinal: 0,
                window_policy: CaptureWindowPolicy::new(600_000_000).unwrap(),
            },
        }])
        .unwrap();
        let mut worker = GroupedPersistenceWorker::new(repository, 8, 3).unwrap();
        let ids = [
            worker.submit_record(envelope_at(0)).unwrap(),
            worker.submit_record(envelope_at(1)).unwrap(),
            worker.submit_record(envelope_at(2)).unwrap(),
        ];

        assert_eq!(worker.process_one_group(), Ok(3));
        for expected in ids {
            let completion = worker.poll_completion().unwrap();
            assert_eq!(completion.id, expected);
            assert!(completion.result.is_ok());
        }
        let repository = worker.into_sink();
        assert_eq!(
            repository.durable_cursors().next().unwrap().2,
            SequenceCursor::new(3)
        );
        drop(repository);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn real_wal_barrier_failure_recovers_group_without_duplicate_append() {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "sensor-host-grouped-fault-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let stream = CaptureStreamSpec {
            directory: root.join("node-1"),
            config: CaptureWalConfig {
                hardware_id: HardwareId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0]),
                boot_epoch: BootEpoch::new(1),
                recovered_cursor: SequenceCursor::ORIGIN,
                first_window_ordinal: 0,
                window_policy: CaptureWindowPolicy::new(600_000_000).unwrap(),
            },
        };
        let mut repository = CaptureRepository::open([stream.clone()]).unwrap();
        repository
            .inject_failure_before_next_sync(stream.config.hardware_id, stream.config.boot_epoch)
            .unwrap();
        let policy = GroupCommitPolicy::new(3, Duration::from_millis(10)).unwrap();
        let mut executor =
            GroupedPersistenceExecutor::start(repository, 3, policy, "test-real-wal-fault")
                .unwrap();
        let ids = [
            executor.submit_record(envelope_at(0)).unwrap(),
            executor.submit_record(envelope_at(1)).unwrap(),
            executor.submit_record(envelope_at(2)).unwrap(),
        ];

        let failure = wait_grouped_repository_completion(&executor);
        assert_eq!(failure.id, ids[0]);
        assert!(failure.result.is_err());

        let recovered = CaptureRepository::open([stream]).unwrap();
        let evidence = RecoveredCursorEvidence {
            hardware_id: HardwareId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0]),
            boot_epoch: BootEpoch::new(1),
            durable: recovered.durable_cursors().next().unwrap().2,
        };
        assert_eq!(evidence.durable, SequenceCursor::new(3));
        let decision = executor
            .replace_recovered_sink(recovered, evidence)
            .unwrap();
        assert_eq!(decision.confirmed_prefix_records, 3);
        for expected in ids {
            let completion = wait_grouped_repository_completion(&executor);
            assert_eq!(completion.id, expected);
            assert!(completion.result.is_ok());
        }
        let shutdown = executor.shutdown().unwrap();
        assert_eq!(
            shutdown.sink.durable_cursors().next().unwrap().2,
            SequenceCursor::new(3)
        );
        drop(shutdown);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn wait_repository_completion(
        executor: &PersistenceExecutor<CaptureRepository>,
    ) -> WriteCompletion<crate::capture_repository::CaptureRepositoryError> {
        for _ in 0..100 {
            if let Some(completion) = executor.try_completion().unwrap() {
                return completion;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("capture repository completion did not arrive");
    }

    fn wait_grouped_repository_completion(
        executor: &GroupedPersistenceExecutor<CaptureRepository>,
    ) -> WriteCompletion<crate::capture_repository::CaptureRepositoryError> {
        for _ in 0..200 {
            if let Some(completion) = executor.try_completion().unwrap() {
                return completion;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("grouped capture repository completion did not arrive");
    }
}
