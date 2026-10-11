use core::fmt;

use sensor_core::{BootEpoch, ContinuityBreak, Gap, HardwareId, SequenceCursor};
use sensor_protocol::SampleChunk;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureWindowPolicy {
    pub target_duration_us: u64,
}

impl CaptureWindowPolicy {
    pub const fn new(target_duration_us: u64) -> Result<Self, CaptureWindowError> {
        if target_duration_us == 0 {
            Err(CaptureWindowError::ZeroTargetDuration)
        } else {
            Ok(Self { target_duration_us })
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowSealReason {
    TargetDurationReached,
    TimingSegmentChanged,
    ExplicitFinish,
}

/// Durable metadata sufficient to order Capture files without using filenames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureWindowManifest {
    pub hardware_id: HardwareId,
    pub boot_epoch: BootEpoch,
    pub window_ordinal: u64,
    pub first_cursor: SequenceCursor,
    pub end_exclusive: SequenceCursor,
    pub first_sample_cursor: Option<SequenceCursor>,
    pub last_sample_cursor: Option<SequenceCursor>,
    pub sample_count: u64,
    pub missing_sample_count: u64,
    pub continuity_break_count: u64,
    pub seal_reason: WindowSealReason,
}

impl CaptureWindowManifest {
    pub fn relation_to(self, next: Self) -> Result<CaptureWindowRelation, CaptureWindowJoinError> {
        if self.hardware_id != next.hardware_id {
            return Err(CaptureWindowJoinError::DifferentHardware);
        }
        if self.boot_epoch != next.boot_epoch {
            return Ok(CaptureWindowRelation::NewBootEpoch);
        }
        if next.window_ordinal != self.window_ordinal.saturating_add(1) {
            return Err(CaptureWindowJoinError::NonConsecutiveOrdinal {
                previous: self.window_ordinal,
                next: next.window_ordinal,
            });
        }
        if next.first_cursor < self.end_exclusive {
            return Err(CaptureWindowJoinError::Overlap {
                previous_end: self.end_exclusive,
                next_first: next.first_cursor,
            });
        }
        if next.first_cursor > self.end_exclusive {
            return Err(CaptureWindowJoinError::UnrecordedRange {
                expected: self.end_exclusive,
                actual: next.first_cursor,
            });
        }
        Ok(CaptureWindowRelation::Contiguous)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureWindowRelation {
    Contiguous,
    NewBootEpoch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OpenWindow {
    first_cursor: SequenceCursor,
    end_exclusive: SequenceCursor,
    first_sample_cursor: Option<SequenceCursor>,
    last_sample_cursor: Option<SequenceCursor>,
    sample_count: u64,
    missing_sample_count: u64,
    continuity_break_count: u64,
    timing_segment_id: Option<u32>,
    first_device_time_us: Option<u64>,
}

/// Pure ordering and rotation policy for one physical sensor stream.
///
/// The production Capture adapter uses this ledger to validate ordering before
/// durable append and publishes a returned manifest only after the same record
/// is durable. Recovery reconstructs the state from durable records. A wire
/// chunk is never split by rotation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureWindowLedger {
    policy: CaptureWindowPolicy,
    hardware_id: HardwareId,
    boot_epoch: BootEpoch,
    next_cursor: SequenceCursor,
    next_window_ordinal: u64,
    open: Option<OpenWindow>,
}

pub struct CaptureAppendPlan {
    before: CaptureWindowLedger,
    after: CaptureWindowLedger,
    target_window_ordinal: u64,
    sealed_manifest: Option<CaptureWindowManifest>,
}

impl CaptureAppendPlan {
    pub const fn target_window_ordinal(&self) -> u64 {
        self.target_window_ordinal
    }
}

impl CaptureWindowLedger {
    pub fn new(
        policy: CaptureWindowPolicy,
        hardware_id: HardwareId,
        boot_epoch: BootEpoch,
        recovered_cursor: SequenceCursor,
        first_window_ordinal: u64,
    ) -> Self {
        Self {
            policy,
            hardware_id,
            boot_epoch,
            next_cursor: recovered_cursor,
            next_window_ordinal: first_window_ordinal,
            open: None,
        }
    }

    pub const fn next_cursor(&self) -> SequenceCursor {
        self.next_cursor
    }

    pub fn plan_chunk(
        &self,
        hardware_id: HardwareId,
        chunk: &SampleChunk,
    ) -> Result<CaptureAppendPlan, CaptureWindowError> {
        let mut after = self.clone();
        let sealed_manifest = after.apply_chunk(hardware_id, chunk)?;
        Ok(self.plan(after, sealed_manifest))
    }

    pub fn plan_gap(&self, gap: Gap) -> Result<CaptureAppendPlan, CaptureWindowError> {
        let mut after = self.clone();
        let sealed_manifest = after.apply_gap(gap)?;
        Ok(self.plan(after, sealed_manifest))
    }

    pub fn plan_continuity_break(
        &self,
        observed: ContinuityBreak,
    ) -> Result<CaptureAppendPlan, CaptureWindowError> {
        let mut after = self.clone();
        let sealed_manifest = after.apply_continuity_break(observed)?;
        Ok(self.plan(after, sealed_manifest))
    }

    /// Advances the in-memory ledger only after the planned record is durable.
    pub fn commit(
        &mut self,
        plan: CaptureAppendPlan,
    ) -> Result<Option<CaptureWindowManifest>, CaptureWindowError> {
        if *self != plan.before {
            return Err(CaptureWindowError::StaleAppendPlan);
        }
        *self = plan.after;
        Ok(plan.sealed_manifest)
    }

    fn plan(
        &self,
        after: CaptureWindowLedger,
        sealed_manifest: Option<CaptureWindowManifest>,
    ) -> CaptureAppendPlan {
        CaptureAppendPlan {
            before: self.clone(),
            after,
            target_window_ordinal: self.next_window_ordinal,
            sealed_manifest,
        }
    }

    fn apply_chunk(
        &mut self,
        hardware_id: HardwareId,
        chunk: &SampleChunk,
    ) -> Result<Option<CaptureWindowManifest>, CaptureWindowError> {
        self.validate_stream(hardware_id, chunk.header.boot_epoch)?;
        let first = chunk.header.first;
        let end = chunk
            .header
            .end()
            .map_err(|_| CaptureWindowError::SequenceOverflow)?;
        self.validate_next(first)?;

        let mut window = self.open.unwrap_or_else(|| OpenWindow::new(first));
        let timing_segment_changed = window
            .timing_segment_id
            .is_some_and(|segment| segment != chunk.timing.segment_id);
        if window.first_sample_cursor.is_none() {
            window.first_sample_cursor = Some(first);
            window.timing_segment_id = Some(chunk.timing.segment_id);
            window.first_device_time_us = Some(chunk.timing.first_device_time_us);
        }
        window.end_exclusive = end;
        window.last_sample_cursor = Some(SequenceCursor::new(end.get() - 1));
        window.sample_count = window
            .sample_count
            .checked_add(u64::from(chunk.header.sample_count))
            .ok_or(CaptureWindowError::CounterOverflow)?;
        self.next_cursor = end;
        self.open = Some(window);

        let duration_reached = !timing_segment_changed
            && window
                .first_device_time_us
                .and_then(|first_time| chunk.timing.last_device_time_us.checked_sub(first_time))
                .is_some_and(|duration| duration >= self.policy.target_duration_us);
        if timing_segment_changed {
            self.seal(WindowSealReason::TimingSegmentChanged).map(Some)
        } else if duration_reached {
            self.seal(WindowSealReason::TargetDurationReached).map(Some)
        } else {
            Ok(None)
        }
    }

    fn apply_gap(&mut self, gap: Gap) -> Result<Option<CaptureWindowManifest>, CaptureWindowError> {
        self.validate_stream(gap.hardware_id, gap.boot_epoch)?;
        let first = SequenceCursor::new(gap.first_missing.get());
        self.validate_next(first)?;
        let end = first
            .checked_advance(gap.missing_count)
            .ok_or(CaptureWindowError::SequenceOverflow)?;
        let mut window = self.open.unwrap_or_else(|| OpenWindow::new(first));
        window.end_exclusive = end;
        window.missing_sample_count = window
            .missing_sample_count
            .checked_add(gap.missing_count)
            .ok_or(CaptureWindowError::CounterOverflow)?;
        self.next_cursor = end;
        self.open = Some(window);
        Ok(None)
    }

    fn apply_continuity_break(
        &mut self,
        observed: ContinuityBreak,
    ) -> Result<Option<CaptureWindowManifest>, CaptureWindowError> {
        self.validate_stream(observed.hardware_id, observed.boot_epoch)?;
        self.validate_next(observed.at)?;
        let end = observed.end();
        let mut window = self.open.unwrap_or_else(|| OpenWindow::new(observed.at));
        window.end_exclusive = end;
        window.continuity_break_count = window
            .continuity_break_count
            .checked_add(1)
            .ok_or(CaptureWindowError::CounterOverflow)?;
        self.next_cursor = end;
        self.open = Some(window);
        Ok(None)
    }

    pub fn finish(&mut self) -> Result<Option<CaptureWindowManifest>, CaptureWindowError> {
        if self.open.is_none() {
            Ok(None)
        } else {
            self.seal(WindowSealReason::ExplicitFinish).map(Some)
        }
    }

    fn validate_stream(
        &self,
        hardware_id: HardwareId,
        boot_epoch: BootEpoch,
    ) -> Result<(), CaptureWindowError> {
        if hardware_id != self.hardware_id {
            Err(CaptureWindowError::WrongHardware)
        } else if boot_epoch != self.boot_epoch {
            Err(CaptureWindowError::WrongBootEpoch)
        } else {
            Ok(())
        }
    }

    fn validate_next(&self, first: SequenceCursor) -> Result<(), CaptureWindowError> {
        if first < self.next_cursor {
            Err(CaptureWindowError::Overlap {
                expected: self.next_cursor,
                actual: first,
            })
        } else if first > self.next_cursor {
            Err(CaptureWindowError::UnrecordedRange {
                expected: self.next_cursor,
                actual: first,
            })
        } else {
            Ok(())
        }
    }

    fn seal(
        &mut self,
        seal_reason: WindowSealReason,
    ) -> Result<CaptureWindowManifest, CaptureWindowError> {
        let window = self.open.take().ok_or(CaptureWindowError::NoOpenWindow)?;
        let window_ordinal = self.next_window_ordinal;
        self.next_window_ordinal = self
            .next_window_ordinal
            .checked_add(1)
            .ok_or(CaptureWindowError::WindowOrdinalOverflow)?;
        Ok(CaptureWindowManifest {
            hardware_id: self.hardware_id,
            boot_epoch: self.boot_epoch,
            window_ordinal,
            first_cursor: window.first_cursor,
            end_exclusive: window.end_exclusive,
            first_sample_cursor: window.first_sample_cursor,
            last_sample_cursor: window.last_sample_cursor,
            sample_count: window.sample_count,
            missing_sample_count: window.missing_sample_count,
            continuity_break_count: window.continuity_break_count,
            seal_reason,
        })
    }
}

impl OpenWindow {
    const fn new(first_cursor: SequenceCursor) -> Self {
        Self {
            first_cursor,
            end_exclusive: first_cursor,
            first_sample_cursor: None,
            last_sample_cursor: None,
            sample_count: 0,
            missing_sample_count: 0,
            continuity_break_count: 0,
            timing_segment_id: None,
            first_device_time_us: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureWindowError {
    ZeroTargetDuration,
    WrongHardware,
    WrongBootEpoch,
    Overlap {
        expected: SequenceCursor,
        actual: SequenceCursor,
    },
    UnrecordedRange {
        expected: SequenceCursor,
        actual: SequenceCursor,
    },
    SequenceOverflow,
    CounterOverflow,
    WindowOrdinalOverflow,
    NoOpenWindow,
    StaleAppendPlan,
}

impl fmt::Display for CaptureWindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "capture window error: {self:?}")
    }
}

impl std::error::Error for CaptureWindowError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureWindowJoinError {
    DifferentHardware,
    NonConsecutiveOrdinal {
        previous: u64,
        next: u64,
    },
    Overlap {
        previous_end: SequenceCursor,
        next_first: SequenceCursor,
    },
    UnrecordedRange {
        expected: SequenceCursor,
        actual: SequenceCursor,
    },
}

impl fmt::Display for CaptureWindowJoinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "capture window join error: {self:?}")
    }
}

impl std::error::Error for CaptureWindowJoinError {}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_core::{ContinuityBreakReason, GapReason, SampleAxes, SampleAxis, SampleSequence};
    use sensor_protocol::{SampleChunkHeader, TimingEvidence, TIMING_VALID};

    const HARDWARE: HardwareId = HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);
    const BOOT: BootEpoch = BootEpoch::new(9);

    fn chunk(first: u64, count: u16, first_time: u64, last_time: u64) -> SampleChunk {
        let axis = SampleAxis::new(1).unwrap();
        SampleChunk {
            header: SampleChunkHeader {
                boot_epoch: BOOT,
                retained_from: SequenceCursor::new(first),
                produced: SequenceCursor::new(first + u64::from(count)),
                first: SequenceCursor::new(first),
                sample_count: count,
            },
            config_revision: 1,
            timing: TimingEvidence {
                segment_id: 1,
                first_device_time_us: first_time,
                last_device_time_us: last_time,
                max_fit_residual_us: 0,
                quality_flags: TIMING_VALID,
            },
            samples: vec![SampleAxes::new(axis, axis, axis); usize::from(count)],
        }
    }

    fn ledger(target_duration_us: u64) -> CaptureWindowLedger {
        CaptureWindowLedger::new(
            CaptureWindowPolicy::new(target_duration_us).unwrap(),
            HARDWARE,
            BOOT,
            SequenceCursor::ORIGIN,
            0,
        )
    }

    fn commit_chunk(
        ledger: &mut CaptureWindowLedger,
        chunk: &SampleChunk,
    ) -> Option<CaptureWindowManifest> {
        let plan = ledger.plan_chunk(HARDWARE, chunk).unwrap();
        ledger.commit(plan).unwrap()
    }

    fn commit_gap(ledger: &mut CaptureWindowLedger, gap: Gap) {
        let plan = ledger.plan_gap(gap).unwrap();
        assert_eq!(ledger.commit(plan), Ok(None));
    }

    fn commit_break(ledger: &mut CaptureWindowLedger, observed: ContinuityBreak) {
        let plan = ledger.plan_continuity_break(observed).unwrap();
        assert_eq!(ledger.commit(plan), Ok(None));
    }

    #[test]
    fn rotation_preserves_exact_adjacent_cursor_ranges() {
        let mut ledger = ledger(1_000);
        assert_eq!(commit_chunk(&mut ledger, &chunk(0, 2, 0, 500)), None);
        let first = commit_chunk(&mut ledger, &chunk(2, 2, 600, 1_100)).unwrap();
        assert_eq!(first.first_cursor, SequenceCursor::new(0));
        assert_eq!(first.end_exclusive, SequenceCursor::new(4));
        assert_eq!(first.sample_count, 4);

        assert_eq!(commit_chunk(&mut ledger, &chunk(4, 2, 1_200, 1_700)), None);
        let second = ledger.finish().unwrap().unwrap();
        assert_eq!(
            first.relation_to(second),
            Ok(CaptureWindowRelation::Contiguous)
        );
        assert_eq!(second.first_cursor, SequenceCursor::new(4));
        assert_eq!(second.end_exclusive, SequenceCursor::new(6));
    }

    #[test]
    fn explicit_gap_is_preserved_in_cursor_space_across_files() {
        let mut ledger = ledger(100);
        let first = commit_chunk(&mut ledger, &chunk(0, 2, 0, 100)).unwrap();
        let gap = Gap::new(
            HARDWARE,
            BOOT,
            SampleSequence::new(2),
            3,
            GapReason::SourceLoss,
        )
        .unwrap();
        commit_gap(&mut ledger, gap);
        let second = commit_chunk(&mut ledger, &chunk(5, 2, 200, 300)).unwrap();

        assert_eq!(
            first.relation_to(second),
            Ok(CaptureWindowRelation::Contiguous)
        );
        assert_eq!(second.first_cursor, SequenceCursor::new(2));
        assert_eq!(second.end_exclusive, SequenceCursor::new(7));
        assert_eq!(second.missing_sample_count, 3);
        assert_eq!(second.sample_count, 2);
    }

    #[test]
    fn hidden_hole_and_overlap_are_rejected() {
        let mut ledger = ledger(1_000_000);
        commit_chunk(&mut ledger, &chunk(0, 2, 0, 10));
        assert_eq!(
            ledger
                .plan_chunk(HARDWARE, &chunk(3, 1, 20, 20))
                .map(|_| ()),
            Err(CaptureWindowError::UnrecordedRange {
                expected: SequenceCursor::new(2),
                actual: SequenceCursor::new(3),
            })
        );
        assert_eq!(
            ledger
                .plan_chunk(HARDWARE, &chunk(1, 1, 20, 20))
                .map(|_| ()),
            Err(CaptureWindowError::Overlap {
                expected: SequenceCursor::new(2),
                actual: SequenceCursor::new(1),
            })
        );
    }

    #[test]
    fn continuity_break_is_ordered_and_consumes_one_cursor_position() {
        let mut ledger = ledger(1_000_000);
        let observed = ContinuityBreak::new(
            HARDWARE,
            BOOT,
            SequenceCursor::ORIGIN,
            ContinuityBreakReason::SensorReadUncertain,
        )
        .unwrap();
        commit_break(&mut ledger, observed);
        commit_chunk(&mut ledger, &chunk(1, 1, 10, 10));
        let manifest = ledger.finish().unwrap().unwrap();
        assert_eq!(manifest.first_cursor, SequenceCursor::ORIGIN);
        assert_eq!(manifest.end_exclusive, SequenceCursor::new(2));
        assert_eq!(manifest.continuity_break_count, 1);
        assert_eq!(manifest.sample_count, 1);
    }

    #[test]
    fn failed_storage_can_discard_plan_without_advancing_ledger() {
        let mut ledger = ledger(1_000_000);
        {
            let discarded = ledger.plan_chunk(HARDWARE, &chunk(0, 2, 0, 10)).unwrap();
            assert_eq!(discarded.target_window_ordinal(), 0);
            assert_eq!(ledger.next_cursor(), SequenceCursor::ORIGIN);
        }
        assert_eq!(ledger.next_cursor(), SequenceCursor::ORIGIN);

        let stale = ledger.plan_chunk(HARDWARE, &chunk(0, 2, 0, 10)).unwrap();
        commit_chunk(&mut ledger, &chunk(0, 1, 0, 0));
        assert_eq!(
            ledger.commit(stale),
            Err(CaptureWindowError::StaleAppendPlan)
        );
    }
}
