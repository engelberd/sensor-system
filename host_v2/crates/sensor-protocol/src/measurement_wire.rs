use core::fmt;

use sensor_core::{
    BootEpoch, ContinuityBreakReason, GapReason, SampleAxes, SampleAxis, SequenceCursor,
};

use crate::{
    ContinuityBreakEvidence, GapEvidence, MeasurementContractError, PullSamplesRequest,
    ResponseStatus, SampleChunkHeader, StreamState, MAX_PAYLOAD_BYTES,
};

pub const GET_STREAM_STATE_MESSAGE_ID: u8 = 1;
pub const PULL_SAMPLES_MESSAGE_ID: u8 = 2;
pub const RELEASE_SAMPLES_MESSAGE_ID: u8 = 3;

pub const STREAM_STATE_FIELDS_BYTES: usize = 44;
pub const STREAM_STATE_SUCCESS_BYTES: usize = 1 + STREAM_STATE_FIELDS_BYTES;
pub const PULL_REQUEST_BYTES: usize = 26;
pub const RELEASE_REQUEST_BYTES: usize = 16;
pub const CHUNK_FIXED_BYTES: usize = 66;
pub const GAP_RESPONSE_BYTES: usize = 43;
pub const CONTINUITY_BREAK_RESPONSE_BYTES: usize = 35;
pub const MAX_WIRE_CHUNK_SAMPLES: usize = (MAX_PAYLOAD_BYTES - CHUNK_FIXED_BYTES) / 9;

pub const TIMING_VALID: u16 = 1 << 0;
pub const TIMING_ESTIMATED: u16 = 1 << 1;
pub const TIMING_SENSOR_LOSS_NEARBY: u16 = 1 << 2;
pub const TIMING_DRDY_RING_OVERFLOW: u16 = 1 << 3;
pub const TIMING_BINDING_MISMATCH: u16 = 1 << 4;
pub const KNOWN_TIMING_FLAGS: u16 = TIMING_VALID
    | TIMING_ESTIMATED
    | TIMING_SENSOR_LOSS_NEARBY
    | TIMING_DRDY_RING_OVERFLOW
    | TIMING_BINDING_MISMATCH;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum PullResultCode {
    SampleChunk = 0,
    NoData = 1,
    RangeUnavailable = 2,
    StreamChanged = 3,
    InvalidRelease = 4,
    Gap = 5,
    ContinuityBreak = 6,
}

impl TryFrom<u8> for PullResultCode {
    type Error = MeasurementWireError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::SampleChunk),
            1 => Ok(Self::NoData),
            2 => Ok(Self::RangeUnavailable),
            3 => Ok(Self::StreamChanged),
            4 => Ok(Self::InvalidRelease),
            5 => Ok(Self::Gap),
            6 => Ok(Self::ContinuityBreak),
            _ => Err(MeasurementWireError::UnknownResult(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimingEvidence {
    pub segment_id: u32,
    pub first_device_time_us: u64,
    pub last_device_time_us: u64,
    pub max_fit_residual_us: u32,
    pub quality_flags: u16,
}

impl TimingEvidence {
    pub fn validate(self, sample_count: u16) -> Result<Self, MeasurementWireError> {
        if self.segment_id == 0 {
            return Err(MeasurementWireError::ZeroTimingSegment);
        }
        if self.quality_flags & !KNOWN_TIMING_FLAGS != 0 {
            return Err(MeasurementWireError::UnknownTimingFlags(
                self.quality_flags & !KNOWN_TIMING_FLAGS,
            ));
        }
        if self.last_device_time_us < self.first_device_time_us {
            return Err(MeasurementWireError::ReversedTimingAnchors);
        }
        if self.quality_flags & TIMING_VALID != 0 {
            if sample_count == 1 && self.last_device_time_us != self.first_device_time_us {
                return Err(MeasurementWireError::InvalidSingleSampleTiming);
            }
            if sample_count > 1 && self.last_device_time_us == self.first_device_time_us {
                return Err(MeasurementWireError::CollapsedTimingAnchors);
            }
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SampleChunk {
    pub header: SampleChunkHeader,
    pub config_revision: u32,
    pub timing: TimingEvidence,
    pub samples: Vec<SampleAxes>,
}

impl SampleChunk {
    pub fn validate(&self) -> Result<(), MeasurementWireError> {
        self.header.validate_structure()?;
        if self.config_revision == 0 {
            return Err(MeasurementWireError::ZeroConfigRevision);
        }
        if self.samples.len() != usize::from(self.header.sample_count) {
            return Err(MeasurementWireError::SampleCountMismatch);
        }
        if self.samples.len() > MAX_WIRE_CHUNK_SAMPLES {
            return Err(MeasurementWireError::ChunkTooLarge);
        }
        self.timing.validate(self.header.sample_count)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PullResponse {
    SampleChunk(SampleChunk),
    NoData(StreamState),
    RangeUnavailable(StreamState),
    StreamChanged(BootEpoch),
    InvalidRelease(StreamState),
    Gap(GapEvidence),
    ContinuityBreak(ContinuityBreakEvidence),
}

pub fn encode_pull_request(request: PullSamplesRequest) -> [u8; PULL_REQUEST_BYTES] {
    let mut output = [0; PULL_REQUEST_BYTES];
    output[0..8].copy_from_slice(&request.boot_epoch.get().to_le_bytes());
    output[8..16].copy_from_slice(&request.from.get().to_le_bytes());
    output[16..24].copy_from_slice(&request.release_before.get().to_le_bytes());
    output[24..26].copy_from_slice(&request.max_samples.to_le_bytes());
    output
}

pub fn decode_pull_request(payload: &[u8]) -> Result<PullSamplesRequest, MeasurementWireError> {
    require_length(payload, PULL_REQUEST_BYTES)?;
    PullSamplesRequest::new(
        BootEpoch::new(read_u64(payload, 0)),
        SequenceCursor::new(read_u64(payload, 8)),
        SequenceCursor::new(read_u64(payload, 16)),
        read_u16(payload, 24),
    )
    .map_err(Into::into)
}

pub fn encode_release_request(
    request: crate::ReleaseSamplesRequest,
) -> [u8; RELEASE_REQUEST_BYTES] {
    let mut output = [0; RELEASE_REQUEST_BYTES];
    output[0..8].copy_from_slice(&request.boot_epoch.get().to_le_bytes());
    output[8..16].copy_from_slice(&request.release_before.get().to_le_bytes());
    output
}

pub fn decode_release_request(
    payload: &[u8],
) -> Result<crate::ReleaseSamplesRequest, MeasurementWireError> {
    require_length(payload, RELEASE_REQUEST_BYTES)?;
    crate::ReleaseSamplesRequest::new(
        BootEpoch::new(read_u64(payload, 0)),
        SequenceCursor::new(read_u64(payload, 8)),
    )
    .map_err(Into::into)
}

pub fn encode_stream_state_success(
    state: StreamState,
) -> Result<[u8; STREAM_STATE_SUCCESS_BYTES], MeasurementWireError> {
    state.validate()?;
    let mut output = [0; STREAM_STATE_SUCCESS_BYTES];
    output[0] = ResponseStatus::Ok as u8;
    encode_stream_state_fields(state, &mut output[1..]);
    Ok(output)
}

pub fn decode_stream_state_success(payload: &[u8]) -> Result<StreamState, MeasurementWireError> {
    decode_ok_status(payload)?;
    require_length(payload, STREAM_STATE_SUCCESS_BYTES)?;
    decode_stream_state_fields(&payload[1..])
}

pub fn encode_pull_response(
    response: &PullResponse,
    output: &mut [u8; MAX_PAYLOAD_BYTES],
) -> Result<usize, MeasurementWireError> {
    output[0] = ResponseStatus::Ok as u8;
    match response {
        PullResponse::SampleChunk(chunk) => {
            chunk.validate()?;
            output[1] = PullResultCode::SampleChunk as u8;
            encode_chunk(chunk, output);
            Ok(CHUNK_FIXED_BYTES + chunk.samples.len() * 9)
        }
        PullResponse::NoData(state) => encode_state_result(PullResultCode::NoData, *state, output),
        PullResponse::RangeUnavailable(state) => {
            encode_state_result(PullResultCode::RangeUnavailable, *state, output)
        }
        PullResponse::StreamChanged(epoch) => {
            if epoch.get() == 0 {
                return Err(MeasurementWireError::Contract(
                    MeasurementContractError::ZeroBootEpoch,
                ));
            }
            output[1] = PullResultCode::StreamChanged as u8;
            output[2..10].copy_from_slice(&epoch.get().to_le_bytes());
            Ok(10)
        }
        PullResponse::InvalidRelease(state) => {
            encode_state_result(PullResultCode::InvalidRelease, *state, output)
        }
        PullResponse::Gap(gap) => encode_gap(*gap, output),
        PullResponse::ContinuityBreak(value) => encode_continuity_break(*value, output),
    }
}

pub fn decode_pull_response(payload: &[u8]) -> Result<PullResponse, MeasurementWireError> {
    decode_ok_status(payload)?;
    if payload.len() < 2 {
        return Err(MeasurementWireError::InvalidLength);
    }
    match PullResultCode::try_from(payload[1])? {
        PullResultCode::SampleChunk => decode_chunk(payload).map(PullResponse::SampleChunk),
        PullResultCode::NoData => decode_state_result(payload).map(PullResponse::NoData),
        PullResultCode::RangeUnavailable => {
            decode_state_result(payload).map(PullResponse::RangeUnavailable)
        }
        PullResultCode::StreamChanged => {
            require_length(payload, 10)?;
            let epoch = BootEpoch::new(read_u64(payload, 2));
            if epoch.get() == 0 {
                return Err(MeasurementContractError::ZeroBootEpoch.into());
            }
            Ok(PullResponse::StreamChanged(epoch))
        }
        PullResultCode::InvalidRelease => {
            decode_state_result(payload).map(PullResponse::InvalidRelease)
        }
        PullResultCode::Gap => decode_gap(payload).map(PullResponse::Gap),
        PullResultCode::ContinuityBreak => {
            decode_continuity_break(payload).map(PullResponse::ContinuityBreak)
        }
    }
}

fn encode_chunk(chunk: &SampleChunk, output: &mut [u8; MAX_PAYLOAD_BYTES]) {
    output[2..10].copy_from_slice(&chunk.header.boot_epoch.get().to_le_bytes());
    output[10..18].copy_from_slice(&chunk.header.retained_from.get().to_le_bytes());
    output[18..26].copy_from_slice(&chunk.header.produced.get().to_le_bytes());
    output[26..34].copy_from_slice(&chunk.header.first.get().to_le_bytes());
    output[34..36].copy_from_slice(&chunk.header.sample_count.to_le_bytes());
    output[36..40].copy_from_slice(&chunk.config_revision.to_le_bytes());
    output[40..44].copy_from_slice(&chunk.timing.segment_id.to_le_bytes());
    output[44..52].copy_from_slice(&chunk.timing.first_device_time_us.to_le_bytes());
    output[52..60].copy_from_slice(&chunk.timing.last_device_time_us.to_le_bytes());
    output[60..64].copy_from_slice(&chunk.timing.max_fit_residual_us.to_le_bytes());
    output[64..66].copy_from_slice(&chunk.timing.quality_flags.to_le_bytes());
    for (index, sample) in chunk.samples.iter().enumerate() {
        let offset = CHUNK_FIXED_BYTES + index * 9;
        encode_i24(sample.x, &mut output[offset..offset + 3]);
        encode_i24(sample.y, &mut output[offset + 3..offset + 6]);
        encode_i24(sample.z, &mut output[offset + 6..offset + 9]);
    }
}

fn decode_chunk(payload: &[u8]) -> Result<SampleChunk, MeasurementWireError> {
    if payload.len() < CHUNK_FIXED_BYTES {
        return Err(MeasurementWireError::InvalidLength);
    }
    let count = read_u16(payload, 34);
    let expected = CHUNK_FIXED_BYTES + usize::from(count) * 9;
    require_length(payload, expected)?;
    let mut samples = Vec::with_capacity(usize::from(count));
    for index in 0..usize::from(count) {
        let offset = CHUNK_FIXED_BYTES + index * 9;
        samples.push(SampleAxes::new(
            decode_i24(&payload[offset..offset + 3])?,
            decode_i24(&payload[offset + 3..offset + 6])?,
            decode_i24(&payload[offset + 6..offset + 9])?,
        ));
    }
    let chunk = SampleChunk {
        header: SampleChunkHeader {
            boot_epoch: BootEpoch::new(read_u64(payload, 2)),
            retained_from: SequenceCursor::new(read_u64(payload, 10)),
            produced: SequenceCursor::new(read_u64(payload, 18)),
            first: SequenceCursor::new(read_u64(payload, 26)),
            sample_count: count,
        },
        config_revision: read_u32(payload, 36),
        timing: TimingEvidence {
            segment_id: read_u32(payload, 40),
            first_device_time_us: read_u64(payload, 44),
            last_device_time_us: read_u64(payload, 52),
            max_fit_residual_us: read_u32(payload, 60),
            quality_flags: read_u16(payload, 64),
        },
        samples,
    };
    chunk.validate()?;
    Ok(chunk)
}

fn encode_gap(
    gap: GapEvidence,
    output: &mut [u8; MAX_PAYLOAD_BYTES],
) -> Result<usize, MeasurementWireError> {
    gap.validate_structure()?;
    output[1] = PullResultCode::Gap as u8;
    output[2..10].copy_from_slice(&gap.boot_epoch.get().to_le_bytes());
    output[10..18].copy_from_slice(&gap.retained_from.get().to_le_bytes());
    output[18..26].copy_from_slice(&gap.produced.get().to_le_bytes());
    output[26..34].copy_from_slice(&gap.first_missing.get().to_le_bytes());
    output[34..42].copy_from_slice(&gap.missing_count.to_le_bytes());
    output[42] = match gap.reason {
        GapReason::UnknownDiscontinuity => 0,
        GapReason::SourceLoss => 1,
        GapReason::IngressOverflow => 2,
        GapReason::JournalOverflow => return Err(MeasurementWireError::InvalidGapReason),
    };
    Ok(GAP_RESPONSE_BYTES)
}

fn decode_gap(payload: &[u8]) -> Result<GapEvidence, MeasurementWireError> {
    require_length(payload, GAP_RESPONSE_BYTES)?;
    let reason = match payload[42] {
        0 => GapReason::UnknownDiscontinuity,
        1 => GapReason::SourceLoss,
        2 => GapReason::IngressOverflow,
        value => return Err(MeasurementWireError::UnknownGapReason(value)),
    };
    GapEvidence {
        boot_epoch: BootEpoch::new(read_u64(payload, 2)),
        retained_from: SequenceCursor::new(read_u64(payload, 10)),
        produced: SequenceCursor::new(read_u64(payload, 18)),
        first_missing: SequenceCursor::new(read_u64(payload, 26)),
        missing_count: read_u64(payload, 34),
        reason,
    }
    .validate_structure()
    .map_err(Into::into)
}

fn encode_continuity_break(
    value: ContinuityBreakEvidence,
    output: &mut [u8; MAX_PAYLOAD_BYTES],
) -> Result<usize, MeasurementWireError> {
    value.validate_structure()?;
    output[1] = PullResultCode::ContinuityBreak as u8;
    output[2..10].copy_from_slice(&value.boot_epoch.get().to_le_bytes());
    output[10..18].copy_from_slice(&value.retained_from.get().to_le_bytes());
    output[18..26].copy_from_slice(&value.produced.get().to_le_bytes());
    output[26..34].copy_from_slice(&value.at.get().to_le_bytes());
    output[34] = match value.reason {
        ContinuityBreakReason::Unknown => 0,
        ContinuityBreakReason::SensorOverflow => 1,
        ContinuityBreakReason::SensorReadUncertain => 2,
        ContinuityBreakReason::InvalidTiming => 3,
        ContinuityBreakReason::ProcessingReset => 4,
    };
    Ok(CONTINUITY_BREAK_RESPONSE_BYTES)
}

fn decode_continuity_break(
    payload: &[u8],
) -> Result<ContinuityBreakEvidence, MeasurementWireError> {
    require_length(payload, CONTINUITY_BREAK_RESPONSE_BYTES)?;
    let reason = match payload[34] {
        0 => ContinuityBreakReason::Unknown,
        1 => ContinuityBreakReason::SensorOverflow,
        2 => ContinuityBreakReason::SensorReadUncertain,
        3 => ContinuityBreakReason::InvalidTiming,
        4 => ContinuityBreakReason::ProcessingReset,
        value => return Err(MeasurementWireError::UnknownContinuityBreakReason(value)),
    };
    ContinuityBreakEvidence {
        boot_epoch: BootEpoch::new(read_u64(payload, 2)),
        retained_from: SequenceCursor::new(read_u64(payload, 10)),
        produced: SequenceCursor::new(read_u64(payload, 18)),
        at: SequenceCursor::new(read_u64(payload, 26)),
        reason,
    }
    .validate_structure()
    .map_err(Into::into)
}

fn encode_state_result(
    code: PullResultCode,
    state: StreamState,
    output: &mut [u8; MAX_PAYLOAD_BYTES],
) -> Result<usize, MeasurementWireError> {
    state.validate()?;
    output[1] = code as u8;
    encode_stream_state_fields(state, &mut output[2..46]);
    Ok(46)
}

fn decode_state_result(payload: &[u8]) -> Result<StreamState, MeasurementWireError> {
    require_length(payload, 46)?;
    decode_stream_state_fields(&payload[2..])
}

fn encode_stream_state_fields(state: StreamState, output: &mut [u8]) {
    output[0..8].copy_from_slice(&state.boot_epoch.get().to_le_bytes());
    output[8..16].copy_from_slice(&state.retained_from.get().to_le_bytes());
    output[16..24].copy_from_slice(&state.produced.get().to_le_bytes());
    output[24..32].copy_from_slice(&state.released.get().to_le_bytes());
    output[32..36].copy_from_slice(&state.capacity_samples.to_le_bytes());
    output[36..44].copy_from_slice(&state.overwritten_samples.to_le_bytes());
}

fn decode_stream_state_fields(payload: &[u8]) -> Result<StreamState, MeasurementWireError> {
    require_length(payload, STREAM_STATE_FIELDS_BYTES)?;
    StreamState {
        boot_epoch: BootEpoch::new(read_u64(payload, 0)),
        retained_from: SequenceCursor::new(read_u64(payload, 8)),
        produced: SequenceCursor::new(read_u64(payload, 16)),
        released: SequenceCursor::new(read_u64(payload, 24)),
        capacity_samples: read_u32(payload, 32),
        overwritten_samples: read_u64(payload, 36),
    }
    .validate()
    .map_err(Into::into)
}

fn decode_ok_status(payload: &[u8]) -> Result<(), MeasurementWireError> {
    let raw = *payload.first().ok_or(MeasurementWireError::InvalidLength)?;
    let status = ResponseStatus::try_from(raw)
        .map_err(|_| MeasurementWireError::UnknownResponseStatus(raw))?;
    if status == ResponseStatus::Ok {
        Ok(())
    } else {
        Err(MeasurementWireError::RemoteError(status))
    }
}

fn require_length(payload: &[u8], expected: usize) -> Result<(), MeasurementWireError> {
    if payload.len() == expected {
        Ok(())
    } else {
        Err(MeasurementWireError::InvalidLength)
    }
}

fn read_u16(input: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(input[offset..offset + 2].try_into().unwrap())
}

fn read_u32(input: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(input[offset..offset + 4].try_into().unwrap())
}

fn read_u64(input: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(input[offset..offset + 8].try_into().unwrap())
}

fn encode_i24(axis: SampleAxis, output: &mut [u8]) {
    output.copy_from_slice(&axis.get().to_le_bytes()[..3]);
}

fn decode_i24(input: &[u8]) -> Result<SampleAxis, MeasurementWireError> {
    let sign = if input[2] & 0x80 == 0 { 0 } else { 0xff };
    SampleAxis::new(i32::from_le_bytes([input[0], input[1], input[2], sign]))
        .map_err(|_| MeasurementWireError::InvalidAxis)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeasurementWireError {
    InvalidLength,
    UnknownResponseStatus(u8),
    RemoteError(ResponseStatus),
    UnknownResult(u8),
    UnknownTimingFlags(u16),
    ZeroTimingSegment,
    ReversedTimingAnchors,
    InvalidSingleSampleTiming,
    CollapsedTimingAnchors,
    ZeroConfigRevision,
    SampleCountMismatch,
    ChunkTooLarge,
    InvalidAxis,
    InvalidGapReason,
    UnknownGapReason(u8),
    UnknownContinuityBreakReason(u8),
    Contract(MeasurementContractError),
}

impl From<MeasurementContractError> for MeasurementWireError {
    fn from(value: MeasurementContractError) -> Self {
        Self::Contract(value)
    }
}

impl fmt::Display for MeasurementWireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength => formatter.write_str("measurement payload has an invalid length"),
            Self::UnknownResponseStatus(value) => {
                write!(formatter, "unknown response status {value}")
            }
            Self::RemoteError(status) => write!(formatter, "node returned {status:?}"),
            Self::UnknownResult(value) => write!(formatter, "unknown pull result {value}"),
            Self::UnknownTimingFlags(bits) => {
                write!(formatter, "unknown timing flags 0x{bits:04x}")
            }
            Self::ZeroTimingSegment => formatter.write_str("timing segment must be non-zero"),
            Self::ReversedTimingAnchors => formatter.write_str("timing anchors are reversed"),
            Self::InvalidSingleSampleTiming => {
                formatter.write_str("one sample must have equal timing anchors")
            }
            Self::CollapsedTimingAnchors => {
                formatter.write_str("multiple valid samples need distinct timing anchors")
            }
            Self::ZeroConfigRevision => formatter.write_str("config revision must be non-zero"),
            Self::SampleCountMismatch => formatter.write_str("sample count differs from payload"),
            Self::ChunkTooLarge => formatter.write_str("sample chunk exceeds framing payload"),
            Self::InvalidAxis => formatter.write_str("sample axis is not signed 24-bit"),
            Self::InvalidGapReason => {
                formatter.write_str("gap reason is not valid for exact gap wire evidence")
            }
            Self::UnknownGapReason(value) => write!(formatter, "unknown gap reason {value}"),
            Self::UnknownContinuityBreakReason(value) => {
                write!(formatter, "unknown continuity break reason {value}")
            }
            Self::Contract(error) => write!(formatter, "measurement contract failed: {error}"),
        }
    }
}

impl std::error::Error for MeasurementWireError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn axis(value: i32) -> SampleAxis {
        SampleAxis::new(value).unwrap()
    }

    fn state() -> StreamState {
        StreamState {
            boot_epoch: BootEpoch::new(0x1122_3344_5566_7788),
            retained_from: SequenceCursor::new(10),
            produced: SequenceCursor::new(100),
            released: SequenceCursor::new(8),
            capacity_samples: 4096,
            overwritten_samples: 10,
        }
    }

    fn chunk() -> SampleChunk {
        SampleChunk {
            header: SampleChunkHeader {
                boot_epoch: state().boot_epoch,
                retained_from: SequenceCursor::new(10),
                produced: SequenceCursor::new(100),
                first: SequenceCursor::new(20),
                sample_count: 2,
            },
            config_revision: 3,
            timing: TimingEvidence {
                segment_id: 4,
                first_device_time_us: 1_000,
                last_device_time_us: 5_000,
                max_fit_residual_us: 2,
                quality_flags: TIMING_VALID,
            },
            samples: vec![
                SampleAxes::new(axis(SampleAxis::MIN), axis(-1), axis(0)),
                SampleAxes::new(axis(1), axis(0x12_3456), axis(SampleAxis::MAX)),
            ],
        }
    }

    fn gap() -> GapEvidence {
        GapEvidence {
            boot_epoch: state().boot_epoch,
            retained_from: SequenceCursor::new(10),
            produced: SequenceCursor::new(100),
            first_missing: SequenceCursor::new(20),
            missing_count: 3,
            reason: GapReason::SourceLoss,
        }
    }

    fn continuity_break() -> ContinuityBreakEvidence {
        ContinuityBreakEvidence {
            boot_epoch: state().boot_epoch,
            retained_from: SequenceCursor::new(10),
            produced: SequenceCursor::new(100),
            at: SequenceCursor::new(20),
            reason: ContinuityBreakReason::SensorReadUncertain,
        }
    }

    #[test]
    fn fixed_sizes_leave_a_bounded_sample_budget() {
        assert_eq!(STREAM_STATE_SUCCESS_BYTES, 45);
        assert_eq!(PULL_REQUEST_BYTES, 26);
        assert_eq!(CHUNK_FIXED_BYTES, 66);
        assert_eq!(MAX_WIRE_CHUNK_SAMPLES, 49);
        assert_eq!(CHUNK_FIXED_BYTES + 48 * 9, 498);
    }

    #[test]
    fn request_and_stream_state_round_trip_exactly() {
        let request = PullSamplesRequest::new(
            state().boot_epoch,
            SequenceCursor::new(20),
            SequenceCursor::new(16),
            48,
        )
        .unwrap();
        let encoded_request = encode_pull_request(request);
        assert_eq!(
            &encoded_request,
            include_bytes!("../../../contracts/golden/measurement_v5_pull_request.bin")
        );
        assert_eq!(decode_pull_request(&encoded_request), Ok(request));
        let release =
            crate::ReleaseSamplesRequest::new(state().boot_epoch, SequenceCursor::new(16)).unwrap();
        let encoded_release = encode_release_request(release);
        assert_eq!(
            &encoded_release,
            include_bytes!("../../../contracts/golden/measurement_v5_release_request.bin")
        );
        assert_eq!(decode_release_request(&encoded_release), Ok(release));
        let encoded = encode_stream_state_success(state()).unwrap();
        assert_eq!(
            &encoded,
            include_bytes!("../../../contracts/golden/measurement_v5_stream_state_response.bin")
        );
        assert_eq!(decode_stream_state_success(&encoded), Ok(state()));
    }

    #[test]
    fn chunk_round_trips_signed_24_bit_boundaries() {
        let expected = PullResponse::SampleChunk(chunk());
        let mut encoded = [0; MAX_PAYLOAD_BYTES];
        let length = encode_pull_response(&expected, &mut encoded).unwrap();
        assert_eq!(length, CHUNK_FIXED_BYTES + 18);
        assert_eq!(
            &encoded[..length],
            include_bytes!("../../../contracts/golden/measurement_v5_chunk_response.bin")
        );
        assert_eq!(decode_pull_response(&encoded[..length]), Ok(expected));
    }

    #[test]
    fn state_results_and_stream_change_have_unambiguous_lengths() {
        let variants = [
            (
                PullResponse::NoData(state()),
                include_bytes!("../../../contracts/golden/measurement_v5_no_data_response.bin")
                    .as_slice(),
            ),
            (
                PullResponse::RangeUnavailable(state()),
                include_bytes!(
                    "../../../contracts/golden/measurement_v5_range_unavailable_response.bin"
                )
                .as_slice(),
            ),
            (
                PullResponse::InvalidRelease(state()),
                include_bytes!(
                    "../../../contracts/golden/measurement_v5_invalid_release_response.bin"
                )
                .as_slice(),
            ),
        ];
        for (expected, golden) in variants {
            let mut encoded = [0; MAX_PAYLOAD_BYTES];
            let length = encode_pull_response(&expected, &mut encoded).unwrap();
            assert_eq!(length, 46);
            assert_eq!(&encoded[..length], golden);
            assert_eq!(decode_pull_response(&encoded[..length]), Ok(expected));
        }
        let expected = PullResponse::StreamChanged(BootEpoch::new(9));
        let mut encoded = [0; MAX_PAYLOAD_BYTES];
        let length = encode_pull_response(&expected, &mut encoded).unwrap();
        assert_eq!(length, 10);
        assert_eq!(
            &encoded[..length],
            include_bytes!("../../../contracts/golden/measurement_v5_stream_changed_response.bin")
        );
        assert_eq!(decode_pull_response(&encoded[..length]), Ok(expected));
    }

    #[test]
    fn exact_gap_matches_the_shared_golden_vector() {
        let expected = PullResponse::Gap(gap());
        let mut encoded = [0; MAX_PAYLOAD_BYTES];
        let length = encode_pull_response(&expected, &mut encoded).unwrap();
        assert_eq!(length, GAP_RESPONSE_BYTES);
        assert_eq!(
            &encoded[..length],
            include_bytes!("../../../contracts/golden/measurement_v5_gap_response.bin")
        );
        assert_eq!(decode_pull_response(&encoded[..length]), Ok(expected));

        encoded[42] = 0xff;
        assert_eq!(
            decode_pull_response(&encoded[..length]),
            Err(MeasurementWireError::UnknownGapReason(0xff))
        );
    }

    #[test]
    fn continuity_break_matches_the_shared_golden_vector() {
        let expected = PullResponse::ContinuityBreak(continuity_break());
        let mut encoded = [0; MAX_PAYLOAD_BYTES];
        let length = encode_pull_response(&expected, &mut encoded).unwrap();
        assert_eq!(length, CONTINUITY_BREAK_RESPONSE_BYTES);
        assert_eq!(
            &encoded[..length],
            include_bytes!(
                "../../../contracts/golden/measurement_v5_continuity_break_response.bin"
            )
        );
        assert_eq!(decode_pull_response(&encoded[..length]), Ok(expected));

        encoded[34] = 0xff;
        assert_eq!(
            decode_pull_response(&encoded[..length]),
            Err(MeasurementWireError::UnknownContinuityBreakReason(0xff))
        );
    }

    #[test]
    fn rejects_unknown_flags_bad_lengths_and_remote_errors() {
        let mut invalid = chunk();
        invalid.timing.quality_flags = 0x8000;
        let mut encoded = [0; MAX_PAYLOAD_BYTES];
        assert_eq!(
            encode_pull_response(&PullResponse::SampleChunk(invalid), &mut encoded),
            Err(MeasurementWireError::UnknownTimingFlags(0x8000))
        );
        assert_eq!(
            decode_pull_response(&[ResponseStatus::Busy as u8]),
            Err(MeasurementWireError::RemoteError(ResponseStatus::Busy))
        );
        assert_eq!(
            decode_pull_response(&[0, PullResultCode::StreamChanged as u8, 1]),
            Err(MeasurementWireError::InvalidLength)
        );
    }
}
