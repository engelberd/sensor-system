use core::fmt;

use sensor_core::{
    BootEpoch, ContinuityBreak, ContinuityBreakError, CursorError, DeliveryCursors, Gap, GapError,
    GapReason, HardwareId, SampleAxes, SequenceCursor,
};
use sensor_protocol::{
    ContinuityBreakEvidence, GapEvidence, MeasurementContractError, MeasurementWireError,
    PullSamplesRequest, SampleChunk, SampleChunkHeader, StreamState,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DurableReceipt {
    pub hardware_id: HardwareId,
    pub boot_epoch: BootEpoch,
    pub end: SequenceCursor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPhase {
    Ready,
    AwaitingPull,
    PersistingChunk,
    PersistingGap,
    PersistingContinuityBreak,
    RecoveryRequired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PendingChunk {
    Legacy {
        header: SampleChunkHeader,
        samples: Vec<SampleAxes>,
    },
    Wire(SampleChunk),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PendingPersistence {
    Chunk {
        hardware_id: HardwareId,
        chunk: SampleChunk,
    },
    Gap(Gap),
    ContinuityBreak(ContinuityBreak),
}

impl PendingPersistence {
    pub fn receipt(&self) -> DurableReceipt {
        match self {
            Self::Chunk { hardware_id, chunk } => DurableReceipt {
                hardware_id: *hardware_id,
                boot_epoch: chunk.header.boot_epoch,
                end: SequenceCursor::new(
                    chunk.header.first.get() + u64::from(chunk.header.sample_count),
                ),
            },
            Self::Gap(gap) => DurableReceipt {
                hardware_id: gap.hardware_id,
                boot_epoch: gap.boot_epoch,
                end: SequenceCursor::new(gap.first_missing.get() + gap.missing_count),
            },
            Self::ContinuityBreak(observed) => DurableReceipt {
                hardware_id: observed.hardware_id,
                boot_epoch: observed.boot_epoch,
                end: observed.end(),
            },
        }
    }
}

impl PendingChunk {
    fn samples(&self) -> &[SampleAxes] {
        match self {
            Self::Legacy { samples, .. } => samples,
            Self::Wire(chunk) => &chunk.samples,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeSession {
    hardware_id: HardwareId,
    boot_epoch: BootEpoch,
    cursors: DeliveryCursors,
    phase: SessionPhase,
    active_pull: Option<PullSamplesRequest>,
    pending_chunk: Option<PendingChunk>,
    pending_gap: Option<Gap>,
    pending_continuity_break: Option<ContinuityBreak>,
    pending_receipt: Option<DurableReceipt>,
}

impl NodeSession {
    pub fn recovered(
        hardware_id: HardwareId,
        boot_epoch: BootEpoch,
        durable: SequenceCursor,
        released: SequenceCursor,
    ) -> Result<Self, SessionError> {
        let cursors = DeliveryCursors::new(durable, durable, released)?;
        Ok(Self {
            hardware_id,
            boot_epoch,
            cursors,
            phase: SessionPhase::Ready,
            active_pull: None,
            pending_chunk: None,
            pending_gap: None,
            pending_continuity_break: None,
            pending_receipt: None,
        })
    }

    pub const fn phase(&self) -> SessionPhase {
        self.phase
    }

    pub const fn hardware_id(&self) -> HardwareId {
        self.hardware_id
    }

    pub const fn boot_epoch(&self) -> BootEpoch {
        self.boot_epoch
    }

    pub const fn cursors(&self) -> DeliveryCursors {
        self.cursors
    }

    pub fn begin_pull(&mut self, max_samples: u16) -> Result<PullSamplesRequest, SessionError> {
        self.require_phase(SessionPhase::Ready)?;
        let request = PullSamplesRequest::new(
            self.boot_epoch,
            self.cursors.received(),
            self.cursors.durable(),
            max_samples,
        )?;
        self.active_pull = Some(request);
        self.phase = SessionPhase::AwaitingPull;
        Ok(request)
    }

    pub fn abandon_pull_for_retry(&mut self) -> Result<(), SessionError> {
        self.require_phase(SessionPhase::AwaitingPull)?;
        self.active_pull = None;
        self.phase = SessionPhase::Ready;
        Ok(())
    }

    pub fn accept_chunk(
        &mut self,
        header: SampleChunkHeader,
        samples: Vec<SampleAxes>,
    ) -> Result<(), SessionError> {
        let request = self.active_pull()?;
        let header = match header.validate_for(request) {
            Ok(header) => header,
            Err(error) => return self.enter_recovery(SessionError::Contract(error)),
        };
        if samples.len() != usize::from(header.sample_count) {
            return self.enter_recovery(SessionError::SampleCountMismatch);
        }

        self.active_pull = None;
        self.cursors.mark_released(request.release_before)?;
        self.cursors
            .accept_contiguous(header.first.next_sample(), header.sample_count)?;
        self.pending_chunk = Some(PendingChunk::Legacy { header, samples });
        self.pending_receipt = Some(DurableReceipt {
            hardware_id: self.hardware_id,
            boot_epoch: self.boot_epoch,
            end: header.end()?,
        });
        self.phase = SessionPhase::PersistingChunk;
        Ok(())
    }

    pub fn accept_wire_chunk(&mut self, chunk: SampleChunk) -> Result<(), SessionError> {
        chunk.validate()?;
        let request = self.active_pull()?;
        let header = match chunk.header.validate_for(request) {
            Ok(header) => header,
            Err(error) => return self.enter_recovery(SessionError::Contract(error)),
        };
        if chunk.samples.len() != usize::from(header.sample_count) {
            return self.enter_recovery(SessionError::SampleCountMismatch);
        }

        self.active_pull = None;
        self.cursors.mark_released(request.release_before)?;
        self.cursors
            .accept_contiguous(header.first.next_sample(), header.sample_count)?;
        self.pending_receipt = Some(DurableReceipt {
            hardware_id: self.hardware_id,
            boot_epoch: self.boot_epoch,
            end: header.end()?,
        });
        self.pending_chunk = Some(PendingChunk::Wire(chunk));
        self.phase = SessionPhase::PersistingChunk;
        Ok(())
    }

    pub fn accept_no_data(&mut self, state: StreamState) -> Result<(), SessionError> {
        let request = self.active_pull()?;
        let state = self.validate_state_for_active_stream(state)?;
        if request.from != state.produced {
            return self.enter_recovery(SessionError::UnexpectedNoData);
        }
        self.active_pull = None;
        self.cursors.mark_released(request.release_before)?;
        self.phase = SessionPhase::Ready;
        Ok(())
    }

    pub fn accept_range_unavailable(&mut self, state: StreamState) -> Result<Gap, SessionError> {
        let request = self.active_pull()?;
        let state = self.validate_state_for_active_stream(state)?;
        if request.from >= state.retained_from {
            return self.enter_recovery(SessionError::UnexpectedRangeUnavailable);
        }
        self.active_pull = None;
        self.cursors.mark_released(request.release_before)?;
        let missing_count = state.retained_from.get() - request.from.get();
        let gap = Gap::new(
            self.hardware_id,
            self.boot_epoch,
            request.from.next_sample(),
            missing_count,
            GapReason::JournalOverflow,
        )?;
        self.cursors
            .account_gap(gap.first_missing, gap.missing_count)?;
        self.pending_gap = Some(gap);
        self.pending_receipt = Some(DurableReceipt {
            hardware_id: gap.hardware_id,
            boot_epoch: gap.boot_epoch,
            end: SequenceCursor::new(gap.first_missing.get() + gap.missing_count),
        });
        self.phase = SessionPhase::PersistingGap;
        Ok(gap)
    }

    pub fn accept_gap(&mut self, evidence: GapEvidence) -> Result<Gap, SessionError> {
        let request = self.active_pull()?;
        let evidence = match evidence.validate_for(request) {
            Ok(evidence) => evidence,
            Err(error) => return self.enter_recovery(SessionError::Contract(error)),
        };
        self.active_pull = None;
        self.cursors.mark_released(request.release_before)?;
        let gap = Gap::new(
            self.hardware_id,
            self.boot_epoch,
            evidence.first_missing.next_sample(),
            evidence.missing_count,
            evidence.reason,
        )?;
        self.cursors
            .account_gap(gap.first_missing, gap.missing_count)?;
        self.pending_gap = Some(gap);
        self.pending_receipt = Some(DurableReceipt {
            hardware_id: gap.hardware_id,
            boot_epoch: gap.boot_epoch,
            end: SequenceCursor::new(gap.first_missing.get() + gap.missing_count),
        });
        self.phase = SessionPhase::PersistingGap;
        Ok(gap)
    }

    pub fn accept_continuity_break(
        &mut self,
        evidence: ContinuityBreakEvidence,
    ) -> Result<ContinuityBreak, SessionError> {
        let request = self.active_pull()?;
        let evidence = match evidence.validate_for(request) {
            Ok(evidence) => evidence,
            Err(error) => return self.enter_recovery(SessionError::Contract(error)),
        };
        self.active_pull = None;
        self.cursors.mark_released(request.release_before)?;
        let observed = ContinuityBreak::new(
            self.hardware_id,
            self.boot_epoch,
            evidence.at,
            evidence.reason,
        )?;
        self.cursors.account_continuity_break(observed.at)?;
        self.pending_continuity_break = Some(observed);
        self.pending_receipt = Some(DurableReceipt {
            hardware_id: observed.hardware_id,
            boot_epoch: observed.boot_epoch,
            end: observed.end(),
        });
        self.phase = SessionPhase::PersistingContinuityBreak;
        Ok(observed)
    }

    pub fn stream_changed(&mut self) -> Result<(), SessionError> {
        self.require_phase(SessionPhase::AwaitingPull)?;
        self.active_pull = None;
        self.phase = SessionPhase::RecoveryRequired;
        Ok(())
    }

    pub fn invalid_release(&mut self, state: StreamState) -> Result<(), SessionError> {
        self.active_pull()?;
        self.validate_state_for_active_stream(state)?;
        self.active_pull = None;
        self.phase = SessionPhase::RecoveryRequired;
        Ok(())
    }

    pub fn fail_pull_for_recovery(&mut self) -> Result<(), SessionError> {
        self.require_phase(SessionPhase::AwaitingPull)?;
        self.active_pull = None;
        self.phase = SessionPhase::RecoveryRequired;
        Ok(())
    }

    pub fn pending_samples(&self) -> Option<&[SampleAxes]> {
        self.pending_chunk.as_ref().map(PendingChunk::samples)
    }

    pub fn pending_wire_chunk(&self) -> Option<&SampleChunk> {
        match self.pending_chunk.as_ref() {
            Some(PendingChunk::Wire(chunk)) => Some(chunk),
            _ => None,
        }
    }

    pub const fn pending_gap(&self) -> Option<Gap> {
        self.pending_gap
    }

    pub const fn pending_continuity_break(&self) -> Option<ContinuityBreak> {
        self.pending_continuity_break
    }

    pub const fn pending_receipt(&self) -> Option<DurableReceipt> {
        self.pending_receipt
    }

    /// Moves the pending wire record to the storage owner without copying its
    /// samples. The session retains only the expected durable receipt.
    pub fn take_pending_persistence(&mut self) -> Result<PendingPersistence, SessionError> {
        let record = match self.phase {
            SessionPhase::PersistingChunk => match self.pending_chunk.take() {
                Some(PendingChunk::Wire(chunk)) => PendingPersistence::Chunk {
                    hardware_id: self.hardware_id,
                    chunk,
                },
                Some(legacy @ PendingChunk::Legacy { .. }) => {
                    self.pending_chunk = Some(legacy);
                    return Err(SessionError::NonWirePending);
                }
                None => return Err(SessionError::PendingAlreadyHandedOff),
            },
            SessionPhase::PersistingGap => PendingPersistence::Gap(
                self.pending_gap
                    .take()
                    .ok_or(SessionError::PendingAlreadyHandedOff)?,
            ),
            SessionPhase::PersistingContinuityBreak => PendingPersistence::ContinuityBreak(
                self.pending_continuity_break
                    .take()
                    .ok_or(SessionError::PendingAlreadyHandedOff)?,
            ),
            _ => return Err(SessionError::UnexpectedPhase),
        };
        if Some(record.receipt()) != self.pending_receipt {
            return Err(SessionError::ReceiptCursorMismatch);
        }
        Ok(record)
    }

    pub fn confirm_durable(&mut self, receipt: DurableReceipt) -> Result<(), SessionError> {
        if receipt.hardware_id != self.hardware_id || receipt.boot_epoch != self.boot_epoch {
            return Err(SessionError::ReceiptIdentityMismatch);
        }
        match self.phase {
            SessionPhase::PersistingChunk
            | SessionPhase::PersistingGap
            | SessionPhase::PersistingContinuityBreak => {
                if self.pending_receipt != Some(receipt) {
                    return Err(SessionError::ReceiptCursorMismatch);
                }
                self.cursors.mark_durable(receipt.end)?;
                self.pending_chunk = None;
                self.pending_gap = None;
                self.pending_continuity_break = None;
                self.pending_receipt = None;
            }
            _ => return Err(SessionError::UnexpectedPhase),
        }
        self.phase = SessionPhase::Ready;
        Ok(())
    }

    fn active_pull(&self) -> Result<PullSamplesRequest, SessionError> {
        self.require_phase(SessionPhase::AwaitingPull)?;
        self.active_pull.ok_or(SessionError::MissingPending)
    }

    fn validate_state_for_active_stream(
        &mut self,
        state: StreamState,
    ) -> Result<StreamState, SessionError> {
        let state = state.validate()?;
        if state.boot_epoch != self.boot_epoch {
            self.phase = SessionPhase::RecoveryRequired;
            return Err(SessionError::StreamChanged);
        }
        Ok(state)
    }

    fn require_phase(&self, expected: SessionPhase) -> Result<(), SessionError> {
        if self.phase == expected {
            Ok(())
        } else {
            Err(SessionError::UnexpectedPhase)
        }
    }

    fn enter_recovery<T>(&mut self, error: SessionError) -> Result<T, SessionError> {
        self.active_pull = None;
        self.phase = SessionPhase::RecoveryRequired;
        Err(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionError {
    UnexpectedPhase,
    MissingPending,
    SampleCountMismatch,
    UnexpectedNoData,
    UnexpectedRangeUnavailable,
    StreamChanged,
    ReceiptIdentityMismatch,
    ReceiptCursorMismatch,
    NonWirePending,
    PendingAlreadyHandedOff,
    Cursor(CursorError),
    Contract(MeasurementContractError),
    Wire(MeasurementWireError),
    Gap(GapError),
    ContinuityBreak(ContinuityBreakError),
}

impl From<CursorError> for SessionError {
    fn from(value: CursorError) -> Self {
        Self::Cursor(value)
    }
}

impl From<MeasurementContractError> for SessionError {
    fn from(value: MeasurementContractError) -> Self {
        Self::Contract(value)
    }
}

impl From<MeasurementWireError> for SessionError {
    fn from(value: MeasurementWireError) -> Self {
        Self::Wire(value)
    }
}

impl From<GapError> for SessionError {
    fn from(value: GapError) -> Self {
        Self::Gap(value)
    }
}

impl From<ContinuityBreakError> for SessionError {
    fn from(value: ContinuityBreakError) -> Self {
        Self::ContinuityBreak(value)
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedPhase => {
                formatter.write_str("operation is invalid in this session phase")
            }
            Self::MissingPending => formatter.write_str("session has no pending operation"),
            Self::SampleCountMismatch => {
                formatter.write_str("chunk payload count differs from its header")
            }
            Self::UnexpectedNoData => {
                formatter.write_str("NO_DATA does not match the requested cursor")
            }
            Self::UnexpectedRangeUnavailable => {
                formatter.write_str("RANGE_UNAVAILABLE does not prove a missing range")
            }
            Self::StreamChanged => formatter.write_str("node boot epoch changed"),
            Self::ReceiptIdentityMismatch => {
                formatter.write_str("durable receipt belongs to another stream")
            }
            Self::ReceiptCursorMismatch => {
                formatter.write_str("durable receipt does not cover the pending range")
            }
            Self::NonWirePending => {
                formatter.write_str("legacy measurement cannot enter recorder v2")
            }
            Self::PendingAlreadyHandedOff => {
                formatter.write_str("pending measurement is already owned by storage")
            }
            Self::Cursor(error) => write!(formatter, "cursor invariant failed: {error}"),
            Self::Contract(error) => write!(formatter, "measurement contract failed: {error}"),
            Self::Wire(error) => write!(formatter, "measurement wire payload failed: {error}"),
            Self::Gap(error) => write!(formatter, "gap invariant failed: {error}"),
            Self::ContinuityBreak(error) => {
                write!(formatter, "continuity break invariant failed: {error}")
            }
        }
    }
}

impl std::error::Error for SessionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::{SimulatedNodeLink, SimulatedPull};
    use sensor_core::SampleAxis;

    const HARDWARE_ID: HardwareId = HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);

    fn axes(value: i32) -> SampleAxes {
        let axis = SampleAxis::new(value).unwrap();
        SampleAxes::new(axis, axis, axis)
    }

    fn session(epoch: u64) -> NodeSession {
        NodeSession::recovered(
            HARDWARE_ID,
            BootEpoch::new(epoch),
            SequenceCursor::ORIGIN,
            SequenceCursor::ORIGIN,
        )
        .unwrap()
    }

    fn receipt(epoch: u64, end: u64) -> DurableReceipt {
        DurableReceipt {
            hardware_id: HARDWARE_ID,
            boot_epoch: BootEpoch::new(epoch),
            end: SequenceCursor::new(end),
        }
    }

    fn deliver(session: &mut NodeSession, outcome: SimulatedPull) {
        match outcome {
            SimulatedPull::Chunk { header, samples } => {
                session.accept_chunk(header, samples).unwrap();
            }
            SimulatedPull::NoData(state) => session.accept_no_data(state).unwrap(),
            SimulatedPull::RangeUnavailable(state) => {
                session.accept_range_unavailable(state).unwrap();
            }
            SimulatedPull::StreamChanged => session.stream_changed().unwrap(),
            SimulatedPull::InvalidRelease => panic!("simulation rejected a valid release"),
        }
    }

    #[test]
    fn releases_only_the_previously_durable_prefix() {
        let mut node = SimulatedNodeLink::new(BootEpoch::new(7), 16);
        node.produce([axes(1), axes(2), axes(3), axes(4)]);
        let mut session = session(7);

        let first = session.begin_pull(2).unwrap();
        assert_eq!(first.release_before, SequenceCursor::ORIGIN);
        deliver(&mut session, node.pull(first));
        assert_eq!(session.cursors().received(), SequenceCursor::new(2));
        assert_eq!(session.cursors().durable(), SequenceCursor::ORIGIN);
        assert_eq!(session.cursors().released(), SequenceCursor::ORIGIN);

        session.confirm_durable(receipt(7, 2)).unwrap();
        let second = session.begin_pull(2).unwrap();
        assert_eq!(second.from, SequenceCursor::new(2));
        assert_eq!(second.release_before, SequenceCursor::new(2));
        deliver(&mut session, node.pull(second));
        assert_eq!(session.cursors().released(), SequenceCursor::new(2));
    }

    #[test]
    fn timeout_retries_the_same_cursor_and_duplicate_delivery_is_rejected() {
        let mut node = SimulatedNodeLink::new(BootEpoch::new(7), 8);
        node.produce([axes(1), axes(2)]);
        let mut session = session(7);

        let lost = session.begin_pull(2).unwrap();
        let lost_response = node.pull(lost);
        session.abandon_pull_for_retry().unwrap();
        let retry = session.begin_pull(2).unwrap();
        assert_eq!(retry.from, lost.from);
        deliver(&mut session, node.pull(retry));
        assert_eq!(session.phase(), SessionPhase::PersistingChunk);
        assert_eq!(
            match lost_response {
                SimulatedPull::Chunk { header, samples } => session.accept_chunk(header, samples),
                _ => panic!("expected chunk"),
            },
            Err(SessionError::UnexpectedPhase)
        );
        assert_eq!(session.cursors().received(), SequenceCursor::new(2));
    }

    #[test]
    fn storage_failure_cannot_advance_durable_or_release() {
        let mut node = SimulatedNodeLink::new(BootEpoch::new(7), 8);
        node.produce([axes(1), axes(2)]);
        let mut session = session(7);

        let request = session.begin_pull(2).unwrap();
        deliver(&mut session, node.pull(request));
        assert_eq!(session.phase(), SessionPhase::PersistingChunk);
        assert_eq!(session.pending_samples().unwrap().len(), 2);
        assert_eq!(session.cursors().durable(), SequenceCursor::ORIGIN);
        assert_eq!(session.cursors().released(), SequenceCursor::ORIGIN);
        assert_eq!(session.begin_pull(2), Err(SessionError::UnexpectedPhase));
    }

    #[test]
    fn overflow_becomes_a_durable_gap_before_transfer_continues() {
        let mut node = SimulatedNodeLink::new(BootEpoch::new(7), 3);
        node.produce([axes(0), axes(1), axes(2), axes(3), axes(4)]);
        let mut session = session(7);

        let request = session.begin_pull(3).unwrap();
        deliver(&mut session, node.pull(request));
        let gap = session.pending_gap().unwrap();
        assert_eq!(gap.first_missing.get(), 0);
        assert_eq!(gap.missing_count, 2);
        assert_eq!(session.cursors().received(), SequenceCursor::new(2));

        session.confirm_durable(receipt(7, 2)).unwrap();
        assert_eq!(session.cursors().durable(), SequenceCursor::new(2));
        let request = session.begin_pull(3).unwrap();
        assert_eq!(request.from, SequenceCursor::new(2));
        deliver(&mut session, node.pull(request));
        assert_eq!(session.pending_samples().unwrap().len(), 3);
    }

    #[test]
    fn reboot_requires_recovery_and_never_relabels_the_new_stream() {
        let mut node = SimulatedNodeLink::new(BootEpoch::new(7), 8);
        let mut session = session(7);
        let request = session.begin_pull(2).unwrap();
        node.reboot(BootEpoch::new(8));
        deliver(&mut session, node.pull(request));
        assert_eq!(session.phase(), SessionPhase::RecoveryRequired);
        assert_eq!(session.cursors(), DeliveryCursors::empty());
    }

    #[test]
    fn exact_gap_advances_received_before_durable_and_rejects_skips() {
        let mut session = session(7);
        session.begin_pull(4).unwrap();
        let gap = GapEvidence {
            boot_epoch: BootEpoch::new(7),
            retained_from: SequenceCursor::ORIGIN,
            produced: SequenceCursor::new(4),
            first_missing: SequenceCursor::ORIGIN,
            missing_count: 2,
            reason: GapReason::IngressOverflow,
        };
        session.accept_gap(gap).unwrap();
        assert_eq!(session.phase(), SessionPhase::PersistingGap);
        assert_eq!(session.cursors().received(), SequenceCursor::new(2));
        assert_eq!(session.cursors().durable(), SequenceCursor::ORIGIN);
        session.confirm_durable(receipt(7, 2)).unwrap();
        assert_eq!(session.cursors().durable(), SequenceCursor::new(2));

        session.begin_pull(4).unwrap();
        let skipped = GapEvidence {
            first_missing: SequenceCursor::new(3),
            produced: SequenceCursor::new(5),
            ..gap
        };
        assert_eq!(
            session.accept_gap(skipped),
            Err(SessionError::Contract(
                MeasurementContractError::UnexpectedFirstSequence
            ))
        );
        assert_eq!(session.phase(), SessionPhase::RecoveryRequired);
    }

    #[test]
    fn continuity_break_requires_durable_evidence_before_next_pull() {
        let mut session = session(7);
        session.begin_pull(4).unwrap();
        let evidence = ContinuityBreakEvidence {
            boot_epoch: BootEpoch::new(7),
            retained_from: SequenceCursor::ORIGIN,
            produced: SequenceCursor::new(2),
            at: SequenceCursor::ORIGIN,
            reason: sensor_core::ContinuityBreakReason::SensorOverflow,
        };

        let observed = session.accept_continuity_break(evidence).unwrap();
        assert_eq!(observed.at, SequenceCursor::ORIGIN);
        assert_eq!(session.phase(), SessionPhase::PersistingContinuityBreak);
        assert_eq!(session.cursors().received(), SequenceCursor::new(1));
        assert_eq!(session.cursors().durable(), SequenceCursor::ORIGIN);
        assert_eq!(session.begin_pull(4), Err(SessionError::UnexpectedPhase));

        session.confirm_durable(receipt(7, 1)).unwrap();
        assert_eq!(session.cursors().durable(), SequenceCursor::new(1));
        assert_eq!(session.phase(), SessionPhase::Ready);
        assert_eq!(session.begin_pull(4).unwrap().from, SequenceCursor::new(1));
    }
}
