use core::fmt;
use std::collections::VecDeque;

use sensor_core::{ContinuityBreak, Gap, HardwareId};
use sensor_protocol::SampleChunk;

use crate::measurement_persistence::DurableMeasurementSink;
use crate::node_session::{DurableReceipt, PendingPersistence};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WriteJobId(u64);

impl WriteJobId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PersistenceRecord {
    Chunk {
        hardware_id: HardwareId,
        chunk: SampleChunk,
    },
    Gap(Gap),
    ContinuityBreak(ContinuityBreak),
}

/// Owned record plus its prevalidated durable receipt.
///
/// Validation happens before enqueue, so a successful storage write can never
/// be followed by a local cursor-computation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistenceEnvelope {
    record: PersistenceRecord,
    receipt: DurableReceipt,
}

impl PersistenceEnvelope {
    pub fn from_pending(record: PendingPersistence) -> Result<Self, PersistenceEnvelopeError> {
        match record {
            PendingPersistence::Chunk { hardware_id, chunk } => Self::chunk(hardware_id, chunk),
            PendingPersistence::Gap(gap) => Ok(Self::gap(gap)),
            PendingPersistence::ContinuityBreak(observed) => Ok(Self::continuity_break(observed)),
        }
    }

    pub fn chunk(
        hardware_id: HardwareId,
        chunk: SampleChunk,
    ) -> Result<Self, PersistenceEnvelopeError> {
        chunk
            .validate()
            .map_err(PersistenceEnvelopeError::InvalidChunk)?;
        let end = chunk
            .header
            .end()
            .map_err(PersistenceEnvelopeError::InvalidContract)?;
        let receipt = DurableReceipt {
            hardware_id,
            boot_epoch: chunk.header.boot_epoch,
            end,
        };
        Ok(Self {
            record: PersistenceRecord::Chunk { hardware_id, chunk },
            receipt,
        })
    }

    pub fn gap(gap: Gap) -> Self {
        let end = gap
            .first_missing
            .checked_add(gap.missing_count)
            .expect("Gap constructor guarantees an exclusive end");
        Self {
            record: PersistenceRecord::Gap(gap),
            receipt: DurableReceipt {
                hardware_id: gap.hardware_id,
                boot_epoch: gap.boot_epoch,
                end: sensor_core::SequenceCursor::new(end.get()),
            },
        }
    }

    pub fn continuity_break(observed: ContinuityBreak) -> Self {
        Self {
            record: PersistenceRecord::ContinuityBreak(observed),
            receipt: DurableReceipt {
                hardware_id: observed.hardware_id,
                boot_epoch: observed.boot_epoch,
                end: observed.end(),
            },
        }
    }

    pub const fn receipt(&self) -> DurableReceipt {
        self.receipt
    }

    pub const fn hardware_id(&self) -> HardwareId {
        self.receipt.hardware_id
    }

    pub const fn boot_epoch(&self) -> sensor_core::BootEpoch {
        self.receipt.boot_epoch
    }

    pub const fn start_cursor(&self) -> sensor_core::SequenceCursor {
        match &self.record {
            PersistenceRecord::Chunk { chunk, .. } => chunk.header.first,
            PersistenceRecord::Gap(gap) => {
                sensor_core::SequenceCursor::new(gap.first_missing.get())
            }
            PersistenceRecord::ContinuityBreak(observed) => observed.at,
        }
    }

    pub const fn end_cursor(&self) -> sensor_core::SequenceCursor {
        self.receipt.end
    }

    pub(crate) const fn record(&self) -> &PersistenceRecord {
        &self.record
    }

    pub(crate) fn persist<S>(&self, sink: &mut S) -> Result<(), S::Error>
    where
        S: DurableMeasurementSink,
    {
        match &self.record {
            PersistenceRecord::Chunk { hardware_id, chunk } => {
                sink.persist_chunk(*hardware_id, chunk)
            }
            PersistenceRecord::Gap(gap) => sink.persist_gap(*gap),
            PersistenceRecord::ContinuityBreak(observed) => {
                sink.persist_continuity_break(*observed)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GroupPersistSuccess {
    pub persisted_records: usize,
}

#[derive(Debug)]
pub enum GroupPersistError<E> {
    /// No record reached storage. The first supplied record cannot start a
    /// valid group; later preparation failures must be deferred by committing
    /// the valid prefix first.
    Preparation(E),
    /// All `attempted_records` may be durable and require recovery evidence.
    Commit { attempted_records: usize, error: E },
}

/// Optional optimized persistence boundary for one durability group.
///
/// Implementations may persist a strict prefix when a physical file or window
/// boundary is reached. A success count is always at least one for non-empty
/// input. A commit error makes exactly the reported prefix uncertain.
pub trait GroupDurableMeasurementSink: DurableMeasurementSink {
    fn persist_group(
        &mut self,
        envelopes: &[&PersistenceEnvelope],
    ) -> Result<GroupPersistSuccess, GroupPersistError<Self::Error>>;
}

pub trait PersistenceSubmitter {
    fn submit_record(&mut self, envelope: PersistenceEnvelope) -> Result<WriteJobId, SubmitError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistenceEnvelopeError {
    InvalidChunk(sensor_protocol::MeasurementWireError),
    InvalidContract(sensor_protocol::MeasurementContractError),
}

impl fmt::Display for PersistenceEnvelopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "persistence envelope error: {self:?}")
    }
}

impl std::error::Error for PersistenceEnvelopeError {}

struct WriteJob {
    id: WriteJobId,
    envelope: PersistenceEnvelope,
}

#[derive(Debug)]
pub struct WriteCompletion<E> {
    pub id: WriteJobId,
    pub result: Result<DurableReceipt, E>,
}

/// Bounded storage-side worker core.
///
/// The channel side only submits and polls. `process_ready` belongs on the
/// storage executor/thread. The number of accepted records never exceeds
/// `capacity`, keeping sample memory bounded even if channels or storage stall.
/// The first sink failure faults the worker and retains the uncertain job;
/// installing a recovered sink retries it before any later queued job.
pub struct BoundedPersistenceWorker<S>
where
    S: DurableMeasurementSink,
{
    sink: S,
    capacity: usize,
    next_job_id: u64,
    pending: VecDeque<WriteJob>,
    failed_job: Option<WriteJob>,
    completions: VecDeque<WriteCompletion<S::Error>>,
    faulted: bool,
}

impl<S> BoundedPersistenceWorker<S>
where
    S: DurableMeasurementSink,
{
    pub fn new(sink: S, capacity: usize) -> Result<Self, BoundedWriterError> {
        if capacity == 0 {
            return Err(BoundedWriterError::ZeroCapacity);
        }
        Ok(Self {
            sink,
            capacity,
            next_job_id: 1,
            pending: VecDeque::with_capacity(capacity),
            failed_job: None,
            completions: VecDeque::with_capacity(capacity),
            faulted: false,
        })
    }

    pub fn submit(&mut self, envelope: PersistenceEnvelope) -> Result<WriteJobId, SubmitError> {
        if self.faulted {
            return Err(SubmitError::new(SubmitErrorKind::WorkerFaulted, envelope));
        }
        if self.accepted_record_count() + self.completions.len() >= self.capacity {
            return Err(SubmitError::new(SubmitErrorKind::QueueFull, envelope));
        }
        let id = WriteJobId(self.next_job_id);
        let Some(next_job_id) = self.next_job_id.checked_add(1) else {
            return Err(SubmitError::new(SubmitErrorKind::JobIdExhausted, envelope));
        };
        self.next_job_id = next_job_id;
        self.pending.push_back(WriteJob { id, envelope });
        Ok(id)
    }

    /// Processes at most `limit` records. Call only from the storage executor.
    pub fn process_ready(&mut self, limit: usize) -> Result<usize, BoundedWriterError> {
        if limit == 0 {
            return Err(BoundedWriterError::ZeroProcessLimit);
        }
        if self.faulted {
            return Ok(0);
        }
        let mut processed = 0;
        while processed < limit {
            let Some(job) = self.pending.pop_front() else {
                break;
            };
            let job_id = job.id;
            let result = match job.envelope.persist(&mut self.sink) {
                Ok(()) => Ok(job.envelope.receipt()),
                Err(error) => {
                    self.faulted = true;
                    self.failed_job = Some(job);
                    Err(error)
                }
            };
            let failed = result.is_err();
            self.completions
                .push_back(WriteCompletion { id: job_id, result });
            processed += 1;
            if failed {
                break;
            }
        }
        Ok(processed)
    }

    pub fn poll_completion(&mut self) -> Option<WriteCompletion<S::Error>> {
        self.completions.pop_front()
    }

    pub fn replace_recovered_sink(&mut self, sink: S) {
        self.sink = sink;
        if let Some(job) = self.failed_job.take() {
            self.pending.push_front(job);
        }
        self.faulted = false;
    }

    pub const fn is_faulted(&self) -> bool {
        self.faulted
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub const fn has_failed_job(&self) -> bool {
        self.failed_job.is_some()
    }

    pub fn completion_count(&self) -> usize {
        self.completions.len()
    }

    pub fn available_capacity(&self) -> usize {
        self.capacity
            .saturating_sub(self.accepted_record_count() + self.completions.len())
    }

    pub fn into_sink(self) -> S {
        self.sink
    }

    fn accepted_record_count(&self) -> usize {
        self.pending.len() + usize::from(self.failed_job.is_some())
    }
}

impl<S> PersistenceSubmitter for BoundedPersistenceWorker<S>
where
    S: DurableMeasurementSink,
{
    fn submit_record(&mut self, envelope: PersistenceEnvelope) -> Result<WriteJobId, SubmitError> {
        self.submit(envelope)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitErrorKind {
    QueueFull,
    WorkerFaulted,
    JobIdExhausted,
}

#[derive(Debug)]
pub struct SubmitError {
    kind: SubmitErrorKind,
    envelope: Box<PersistenceEnvelope>,
}

impl SubmitError {
    pub(crate) fn new(kind: SubmitErrorKind, envelope: PersistenceEnvelope) -> Self {
        Self {
            kind,
            envelope: Box::new(envelope),
        }
    }

    pub const fn kind(&self) -> SubmitErrorKind {
        self.kind
    }

    pub fn into_envelope(self) -> PersistenceEnvelope {
        *self.envelope
    }
}

impl fmt::Display for SubmitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            SubmitErrorKind::QueueFull => formatter.write_str("bounded persistence queue is full"),
            SubmitErrorKind::WorkerFaulted => formatter.write_str("persistence worker is faulted"),
            SubmitErrorKind::JobIdExhausted => {
                formatter.write_str("persistence job ID is exhausted")
            }
        }
    }
}

impl std::error::Error for SubmitError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundedWriterError {
    ZeroCapacity,
    ZeroProcessLimit,
}

impl fmt::Display for BoundedWriterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "bounded persistence writer error: {self:?}")
    }
}

impl std::error::Error for BoundedWriterError {}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_core::{
        BootEpoch, ContinuityBreakReason, GapReason, SampleAxes, SampleAxis, SampleSequence,
        SequenceCursor,
    };
    use sensor_protocol::{SampleChunkHeader, TimingEvidence, TIMING_VALID};

    const HARDWARE: HardwareId = HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);
    const BOOT: BootEpoch = BootEpoch::new(9);

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct SinkError;

    impl fmt::Display for SinkError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("injected sink failure")
        }
    }

    impl std::error::Error for SinkError {}

    struct Sink {
        fail_next: bool,
        persisted_ends: Vec<SequenceCursor>,
    }

    impl Sink {
        fn healthy() -> Self {
            Self {
                fail_next: false,
                persisted_ends: Vec::new(),
            }
        }
    }

    impl DurableMeasurementSink for Sink {
        type Error = SinkError;

        fn persist_chunk(
            &mut self,
            _hardware_id: HardwareId,
            chunk: &SampleChunk,
        ) -> Result<(), Self::Error> {
            if self.fail_next {
                self.fail_next = false;
                return Err(SinkError);
            }
            self.persisted_ends.push(chunk.header.end().unwrap());
            Ok(())
        }

        fn persist_gap(&mut self, gap: Gap) -> Result<(), Self::Error> {
            if self.fail_next {
                self.fail_next = false;
                return Err(SinkError);
            }
            self.persisted_ends.push(SequenceCursor::new(
                gap.first_missing.get() + gap.missing_count,
            ));
            Ok(())
        }

        fn persist_continuity_break(
            &mut self,
            observed: ContinuityBreak,
        ) -> Result<(), Self::Error> {
            if self.fail_next {
                self.fail_next = false;
                return Err(SinkError);
            }
            self.persisted_ends.push(observed.end());
            Ok(())
        }
    }

    fn chunk(first: u64) -> SampleChunk {
        let axis = SampleAxis::new(i32::try_from(first).unwrap()).unwrap();
        SampleChunk {
            header: SampleChunkHeader {
                boot_epoch: BOOT,
                retained_from: SequenceCursor::new(first),
                produced: SequenceCursor::new(first + 1),
                first: SequenceCursor::new(first),
                sample_count: 1,
            },
            config_revision: 1,
            timing: TimingEvidence {
                segment_id: 1,
                first_device_time_us: first * 4_000,
                last_device_time_us: first * 4_000,
                max_fit_residual_us: 1,
                quality_flags: TIMING_VALID,
            },
            samples: vec![SampleAxes::new(axis, axis, axis)],
        }
    }

    #[test]
    fn capacity_counts_pending_and_unconsumed_completions() {
        let mut worker = BoundedPersistenceWorker::new(Sink::healthy(), 2).unwrap();
        let first = worker
            .submit(PersistenceEnvelope::chunk(HARDWARE, chunk(0)).unwrap())
            .unwrap();
        let second = worker
            .submit(PersistenceEnvelope::chunk(HARDWARE, chunk(1)).unwrap())
            .unwrap();
        assert_eq!(worker.available_capacity(), 0);
        let rejected = worker
            .submit(PersistenceEnvelope::chunk(HARDWARE, chunk(2)).unwrap())
            .unwrap_err();
        assert_eq!(rejected.kind(), SubmitErrorKind::QueueFull);

        assert_eq!(worker.process_ready(1), Ok(1));
        assert_eq!(worker.available_capacity(), 0);
        let completion = worker.poll_completion().unwrap();
        assert_eq!(completion.id, first);
        assert_eq!(completion.result.unwrap().end, SequenceCursor::new(1));
        assert_eq!(worker.available_capacity(), 1);
        assert_eq!(worker.pending_count(), 1);
        assert_eq!(second.get(), first.get() + 1);
    }

    #[test]
    fn sink_failure_faults_worker_and_does_not_consume_following_job() {
        let mut worker = BoundedPersistenceWorker::new(
            Sink {
                fail_next: true,
                persisted_ends: Vec::new(),
            },
            3,
        )
        .unwrap();
        let failed = worker
            .submit(PersistenceEnvelope::chunk(HARDWARE, chunk(0)).unwrap())
            .unwrap();
        let queued = worker
            .submit(PersistenceEnvelope::chunk(HARDWARE, chunk(1)).unwrap())
            .unwrap();

        assert_eq!(worker.process_ready(3), Ok(1));
        assert!(worker.is_faulted());
        assert_eq!(worker.pending_count(), 1);
        assert!(worker.has_failed_job());
        let completion = worker.poll_completion().unwrap();
        assert_eq!(completion.id, failed);
        assert_eq!(completion.result, Err(SinkError));

        worker.replace_recovered_sink(Sink::healthy());
        assert_eq!(worker.process_ready(1), Ok(1));
        let completion = worker.poll_completion().unwrap();
        assert_eq!(completion.id, failed);
        assert_eq!(completion.result.unwrap().end, SequenceCursor::new(1));
        assert_eq!(worker.process_ready(1), Ok(1));
        let completion = worker.poll_completion().unwrap();
        assert_eq!(completion.id, queued);
        assert_eq!(completion.result.unwrap().end, SequenceCursor::new(2));
    }

    #[test]
    fn every_record_kind_produces_the_exact_receipt_after_persistence() {
        let gap = Gap::new(
            HARDWARE,
            BOOT,
            SampleSequence::new(4),
            3,
            GapReason::SourceLoss,
        )
        .unwrap();
        let observed = ContinuityBreak::new(
            HARDWARE,
            BOOT,
            SequenceCursor::new(7),
            ContinuityBreakReason::InvalidTiming,
        )
        .unwrap();
        let mut worker = BoundedPersistenceWorker::new(Sink::healthy(), 3).unwrap();
        worker
            .submit(PersistenceEnvelope::chunk(HARDWARE, chunk(3)).unwrap())
            .unwrap();
        worker.submit(PersistenceEnvelope::gap(gap)).unwrap();
        worker
            .submit(PersistenceEnvelope::continuity_break(observed))
            .unwrap();
        assert_eq!(worker.process_ready(3), Ok(3));

        let ends = (0..3)
            .map(|_| worker.poll_completion().unwrap().result.unwrap().end)
            .collect::<Vec<_>>();
        assert_eq!(
            ends,
            vec![
                SequenceCursor::new(4),
                SequenceCursor::new(7),
                SequenceCursor::new(8)
            ]
        );
    }

    #[test]
    fn invalid_configuration_and_process_limit_are_rejected() {
        assert!(matches!(
            BoundedPersistenceWorker::new(Sink::healthy(), 0),
            Err(BoundedWriterError::ZeroCapacity)
        ));
        let mut worker = BoundedPersistenceWorker::new(Sink::healthy(), 1).unwrap();
        assert_eq!(
            worker.process_ready(0),
            Err(BoundedWriterError::ZeroProcessLimit)
        );
    }
}
