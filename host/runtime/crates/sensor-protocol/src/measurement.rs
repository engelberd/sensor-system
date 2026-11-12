use core::fmt;

use sensor_core::{BootEpoch, ContinuityBreakReason, GapReason, SequenceCursor};

/// Semantic payload size before framing and COBS overhead.
pub const PULL_SAMPLES_REQUEST_BYTES: usize = 8 + 8 + 8 + 2;
pub const SAMPLE_CHUNK_FIELDS_BYTES: usize = 8 + 8 + 8 + 8 + 2;
pub const SAMPLE_CHUNK_SUCCESS_HEADER_BYTES: usize = 1 + SAMPLE_CHUNK_FIELDS_BYTES;
pub const XYZ_I24_WIRE_SAMPLE_BYTES: usize = 9;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PullSamplesRequest {
    pub boot_epoch: BootEpoch,
    pub from: SequenceCursor,
    pub release_before: SequenceCursor,
    pub max_samples: u16,
}

impl PullSamplesRequest {
    pub fn new(
        boot_epoch: BootEpoch,
        from: SequenceCursor,
        release_before: SequenceCursor,
        max_samples: u16,
    ) -> Result<Self, MeasurementContractError> {
        if boot_epoch.get() == 0 {
            return Err(MeasurementContractError::ZeroBootEpoch);
        }
        if max_samples == 0 {
            return Err(MeasurementContractError::ZeroSampleLimit);
        }
        if release_before > from {
            return Err(MeasurementContractError::ReleaseAheadOfRequest);
        }
        Ok(Self {
            boot_epoch,
            from,
            release_before,
            max_samples,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamState {
    pub boot_epoch: BootEpoch,
    pub retained_from: SequenceCursor,
    pub produced: SequenceCursor,
    pub released: SequenceCursor,
    pub capacity_samples: u32,
    pub overwritten_samples: u64,
}

impl StreamState {
    pub fn validate(self) -> Result<Self, MeasurementContractError> {
        if self.boot_epoch.get() == 0 {
            return Err(MeasurementContractError::ZeroBootEpoch);
        }
        if self.capacity_samples == 0 {
            return Err(MeasurementContractError::ZeroJournalCapacity);
        }
        if self.retained_from > self.produced {
            return Err(MeasurementContractError::RetainedAheadOfProduced);
        }
        if self.released > self.produced {
            return Err(MeasurementContractError::ReleasedAheadOfProduced);
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SampleChunkHeader {
    pub boot_epoch: BootEpoch,
    pub retained_from: SequenceCursor,
    pub produced: SequenceCursor,
    pub first: SequenceCursor,
    pub sample_count: u16,
}

impl SampleChunkHeader {
    pub fn validate_structure(self) -> Result<Self, MeasurementContractError> {
        if self.boot_epoch.get() == 0 {
            return Err(MeasurementContractError::ZeroBootEpoch);
        }
        if self.retained_from > self.produced {
            return Err(MeasurementContractError::RetainedAheadOfProduced);
        }
        if self.sample_count == 0 {
            return Err(MeasurementContractError::EmptySampleChunk);
        }
        if self.first < self.retained_from {
            return Err(MeasurementContractError::FirstBeforeRetained);
        }
        let end = self
            .first
            .checked_advance(u64::from(self.sample_count))
            .ok_or(MeasurementContractError::SequenceOverflow)?;
        if end > self.produced {
            return Err(MeasurementContractError::ChunkAheadOfProduced);
        }
        Ok(self)
    }

    pub fn validate_for(
        self,
        request: PullSamplesRequest,
    ) -> Result<Self, MeasurementContractError> {
        if self.boot_epoch != request.boot_epoch {
            return Err(MeasurementContractError::StreamChanged);
        }
        self.validate_structure()?;
        if request.from < self.retained_from {
            return Err(MeasurementContractError::RequestedRangeUnavailable);
        }
        if self.first != request.from {
            return Err(MeasurementContractError::UnexpectedFirstSequence);
        }
        if self.sample_count > request.max_samples {
            return Err(MeasurementContractError::SampleLimitExceeded);
        }
        Ok(self)
    }

    pub fn end(self) -> Result<SequenceCursor, MeasurementContractError> {
        self.first
            .checked_advance(u64::from(self.sample_count))
            .ok_or(MeasurementContractError::SequenceOverflow)
    }

    pub fn more_available(self) -> Result<bool, MeasurementContractError> {
        Ok(self.end()? < self.produced)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GapEvidence {
    pub boot_epoch: BootEpoch,
    pub retained_from: SequenceCursor,
    pub produced: SequenceCursor,
    pub first_missing: SequenceCursor,
    pub missing_count: u64,
    pub reason: GapReason,
}

impl GapEvidence {
    pub fn validate_structure(self) -> Result<Self, MeasurementContractError> {
        if self.boot_epoch.get() == 0 {
            return Err(MeasurementContractError::ZeroBootEpoch);
        }
        if self.retained_from > self.produced {
            return Err(MeasurementContractError::RetainedAheadOfProduced);
        }
        if self.missing_count == 0 {
            return Err(MeasurementContractError::EmptyGap);
        }
        if self.reason == GapReason::JournalOverflow {
            return Err(MeasurementContractError::InvalidExactGapReason);
        }
        if self.first_missing < self.retained_from {
            return Err(MeasurementContractError::GapBeforeRetained);
        }
        if self.end()? > self.produced {
            return Err(MeasurementContractError::GapAheadOfProduced);
        }
        Ok(self)
    }

    pub fn validate_for(
        self,
        request: PullSamplesRequest,
    ) -> Result<Self, MeasurementContractError> {
        if self.boot_epoch != request.boot_epoch {
            return Err(MeasurementContractError::StreamChanged);
        }
        self.validate_structure()?;
        if request.from < self.retained_from {
            return Err(MeasurementContractError::RequestedRangeUnavailable);
        }
        if self.first_missing != request.from {
            return Err(MeasurementContractError::UnexpectedFirstSequence);
        }
        Ok(self)
    }

    pub fn end(self) -> Result<SequenceCursor, MeasurementContractError> {
        self.first_missing
            .checked_advance(self.missing_count)
            .ok_or(MeasurementContractError::SequenceOverflow)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContinuityBreakEvidence {
    pub boot_epoch: BootEpoch,
    pub retained_from: SequenceCursor,
    pub produced: SequenceCursor,
    pub at: SequenceCursor,
    pub reason: ContinuityBreakReason,
}

impl ContinuityBreakEvidence {
    pub fn validate_structure(self) -> Result<Self, MeasurementContractError> {
        if self.boot_epoch.get() == 0 {
            return Err(MeasurementContractError::ZeroBootEpoch);
        }
        if self.retained_from > self.produced {
            return Err(MeasurementContractError::RetainedAheadOfProduced);
        }
        if self.at < self.retained_from {
            return Err(MeasurementContractError::BreakBeforeRetained);
        }
        if self.end()? > self.produced {
            return Err(MeasurementContractError::BreakAheadOfProduced);
        }
        Ok(self)
    }

    pub fn validate_for(
        self,
        request: PullSamplesRequest,
    ) -> Result<Self, MeasurementContractError> {
        if self.boot_epoch != request.boot_epoch {
            return Err(MeasurementContractError::StreamChanged);
        }
        self.validate_structure()?;
        if request.from < self.retained_from {
            return Err(MeasurementContractError::RequestedRangeUnavailable);
        }
        if self.at != request.from {
            return Err(MeasurementContractError::UnexpectedFirstSequence);
        }
        Ok(self)
    }

    pub fn end(self) -> Result<SequenceCursor, MeasurementContractError> {
        self.at
            .checked_advance(1)
            .ok_or(MeasurementContractError::SequenceOverflow)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReleaseSamplesRequest {
    pub boot_epoch: BootEpoch,
    pub release_before: SequenceCursor,
}

impl ReleaseSamplesRequest {
    pub fn new(
        boot_epoch: BootEpoch,
        release_before: SequenceCursor,
    ) -> Result<Self, MeasurementContractError> {
        if boot_epoch.get() == 0 {
            return Err(MeasurementContractError::ZeroBootEpoch);
        }
        Ok(Self {
            boot_epoch,
            release_before,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeasurementContractError {
    ZeroBootEpoch,
    ZeroJournalCapacity,
    ZeroSampleLimit,
    ReleaseAheadOfRequest,
    StreamChanged,
    RequestedRangeUnavailable,
    UnexpectedFirstSequence,
    EmptySampleChunk,
    SampleLimitExceeded,
    SequenceOverflow,
    RetainedAheadOfProduced,
    ReleasedAheadOfProduced,
    ChunkAheadOfProduced,
    FirstBeforeRetained,
    EmptyGap,
    GapBeforeRetained,
    GapAheadOfProduced,
    InvalidExactGapReason,
    BreakBeforeRetained,
    BreakAheadOfProduced,
}

impl fmt::Display for MeasurementContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ZeroBootEpoch => "measurement boot epoch must be non-zero",
            Self::ZeroJournalCapacity => "measurement journal capacity must be non-zero",
            Self::ZeroSampleLimit => "pull sample limit must be greater than zero",
            Self::ReleaseAheadOfRequest => "release cursor cannot pass requested start",
            Self::StreamChanged => "response belongs to a different node boot epoch",
            Self::RequestedRangeUnavailable => "requested range is no longer retained",
            Self::UnexpectedFirstSequence => "response does not start at the requested cursor",
            Self::EmptySampleChunk => "sample chunk must contain at least one sample",
            Self::SampleLimitExceeded => "response exceeds the requested sample limit",
            Self::SequenceOverflow => "sample range overflows u64",
            Self::RetainedAheadOfProduced => "retained cursor cannot pass produced cursor",
            Self::ReleasedAheadOfProduced => "released cursor cannot pass produced cursor",
            Self::ChunkAheadOfProduced => "sample chunk extends beyond produced cursor",
            Self::FirstBeforeRetained => "sample chunk starts before retained cursor",
            Self::EmptyGap => "gap must contain at least one missing sample",
            Self::GapBeforeRetained => "gap starts before retained cursor",
            Self::GapAheadOfProduced => "gap extends beyond produced cursor",
            Self::InvalidExactGapReason => {
                "journal overflow must be represented by RANGE_UNAVAILABLE"
            }
            Self::BreakBeforeRetained => "continuity break starts before retained cursor",
            Self::BreakAheadOfProduced => "continuity break extends beyond produced cursor",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for MeasurementContractError {}

#[cfg(test)]
mod tests {
    use super::{
        ContinuityBreakEvidence, GapEvidence, MeasurementContractError, PullSamplesRequest,
        SampleChunkHeader, PULL_SAMPLES_REQUEST_BYTES, SAMPLE_CHUNK_FIELDS_BYTES,
        SAMPLE_CHUNK_SUCCESS_HEADER_BYTES, XYZ_I24_WIRE_SAMPLE_BYTES,
    };
    use sensor_core::{BootEpoch, ContinuityBreakReason, GapReason, SequenceCursor};

    #[test]
    fn continuity_break_is_one_cursor_marker_with_unknown_sample_extent() {
        let request = PullSamplesRequest::new(
            BootEpoch::new(7),
            SequenceCursor::new(32),
            SequenceCursor::new(32),
            8,
        )
        .unwrap();
        let evidence = ContinuityBreakEvidence {
            boot_epoch: BootEpoch::new(7),
            retained_from: SequenceCursor::new(0),
            produced: SequenceCursor::new(40),
            at: SequenceCursor::new(32),
            reason: ContinuityBreakReason::SensorReadUncertain,
        };
        assert_eq!(evidence.validate_for(request), Ok(evidence));
        assert_eq!(evidence.end(), Ok(SequenceCursor::new(33)));
    }

    fn request() -> PullSamplesRequest {
        PullSamplesRequest::new(
            BootEpoch::new(7),
            SequenceCursor::new(32),
            SequenceCursor::new(32),
            32,
        )
        .unwrap()
    }

    #[test]
    fn payload_sizes_include_the_full_width_boot_epoch() {
        assert_eq!(PULL_SAMPLES_REQUEST_BYTES, 26);
        assert_eq!(SAMPLE_CHUNK_FIELDS_BYTES, 34);
        assert_eq!(SAMPLE_CHUNK_SUCCESS_HEADER_BYTES, 35);
        assert_eq!(XYZ_I24_WIRE_SAMPLE_BYTES, 9);
    }

    #[test]
    fn accepts_an_exact_bounded_contiguous_response() {
        let header = SampleChunkHeader {
            boot_epoch: BootEpoch::new(7),
            retained_from: SequenceCursor::new(0),
            produced: SequenceCursor::new(96),
            first: SequenceCursor::new(32),
            sample_count: 32,
        }
        .validate_for(request())
        .unwrap();

        assert_eq!(header.end().unwrap(), SequenceCursor::new(64));
        assert!(header.more_available().unwrap());
    }

    #[test]
    fn rejects_a_response_that_skips_the_requested_hole() {
        let result = SampleChunkHeader {
            boot_epoch: BootEpoch::new(7),
            retained_from: SequenceCursor::new(0),
            produced: SequenceCursor::new(96),
            first: SequenceCursor::new(64),
            sample_count: 32,
        }
        .validate_for(request());

        assert_eq!(
            result,
            Err(MeasurementContractError::UnexpectedFirstSequence)
        );
    }

    #[test]
    fn distinguishes_overwrite_from_a_transient_missing_response() {
        let result = SampleChunkHeader {
            boot_epoch: BootEpoch::new(7),
            retained_from: SequenceCursor::new(64),
            produced: SequenceCursor::new(96),
            first: SequenceCursor::new(64),
            sample_count: 32,
        }
        .validate_for(request());

        assert_eq!(
            result,
            Err(MeasurementContractError::RequestedRangeUnavailable)
        );
    }

    #[test]
    fn rejects_release_past_the_requested_start() {
        assert_eq!(
            PullSamplesRequest::new(
                BootEpoch::new(7),
                SequenceCursor::new(32),
                SequenceCursor::new(33),
                32,
            ),
            Err(MeasurementContractError::ReleaseAheadOfRequest)
        );
    }

    #[test]
    fn exact_gap_must_start_at_the_requested_cursor() {
        let evidence = GapEvidence {
            boot_epoch: BootEpoch::new(7),
            retained_from: SequenceCursor::new(0),
            produced: SequenceCursor::new(40),
            first_missing: SequenceCursor::new(32),
            missing_count: 3,
            reason: GapReason::IngressOverflow,
        };
        assert_eq!(evidence.validate_for(request()), Ok(evidence));
        assert_eq!(evidence.end().unwrap(), SequenceCursor::new(35));

        let skipped = GapEvidence {
            first_missing: SequenceCursor::new(33),
            ..evidence
        };
        assert_eq!(
            skipped.validate_for(request()),
            Err(MeasurementContractError::UnexpectedFirstSequence)
        );
    }

    #[test]
    fn requires_an_explicit_no_data_result_instead_of_an_empty_chunk() {
        let result = SampleChunkHeader {
            boot_epoch: BootEpoch::new(7),
            retained_from: SequenceCursor::new(0),
            produced: SequenceCursor::new(32),
            first: SequenceCursor::new(32),
            sample_count: 0,
        }
        .validate_for(request());

        assert_eq!(result, Err(MeasurementContractError::EmptySampleChunk));
    }
}
