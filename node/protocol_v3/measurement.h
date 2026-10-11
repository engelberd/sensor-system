#pragma once

#include <cstddef>
#include <cstdint>

#include "protocol_v3/application.h"
#include "protocol_v3/framing.h"

namespace ProtocolV3 {

constexpr uint8_t kMeasurementServiceId = static_cast<uint8_t>(ServiceId::Measurement);
constexpr uint8_t kGetStreamStateMessageId = 1;
constexpr uint8_t kPullSamplesMessageId = 2;
constexpr uint8_t kReleaseSamplesMessageId = 3;
constexpr size_t kStreamStateFieldsBytes = 44;
constexpr size_t kStreamStateSuccessBytes = 45;
constexpr size_t kPullRequestBytes = 26;
constexpr size_t kReleaseRequestBytes = 16;
constexpr size_t kChunkFixedBytes = 66;
constexpr size_t kGapResponseBytes = 43;
constexpr size_t kContinuityBreakResponseBytes = 35;
constexpr size_t kXyz24SampleBytes = 9;
constexpr size_t kMaxWireChunkSamples =
    (kMaxPayloadBytes - kChunkFixedBytes) / kXyz24SampleBytes;

constexpr uint16_t kTimingValid = 1u << 0u;
constexpr uint16_t kTimingEstimated = 1u << 1u;
constexpr uint16_t kTimingSensorLossNearby = 1u << 2u;
constexpr uint16_t kTimingDrdyRingOverflow = 1u << 3u;
constexpr uint16_t kTimingBindingMismatch = 1u << 4u;
constexpr uint16_t kKnownTimingFlags =
    kTimingValid | kTimingEstimated | kTimingSensorLossNearby |
    kTimingDrdyRingOverflow | kTimingBindingMismatch;

enum class PullResultCode : uint8_t {
    SampleChunk = 0,
    NoData = 1,
    RangeUnavailable = 2,
    StreamChanged = 3,
    InvalidRelease = 4,
    Gap = 5,
    ContinuityBreak = 6,
};

enum class GapReasonCode : uint8_t {
    UnknownDiscontinuity = 0,
    SourceLoss = 1,
    IngressOverflow = 2,
};

enum class ContinuityBreakReasonCode : uint8_t {
    Unknown = 0,
    SensorOverflow = 1,
    SensorReadUncertain = 2,
    InvalidTiming = 3,
    ProcessingReset = 4,
};

struct StreamState {
    uint64_t boot_epoch = 0;
    uint64_t retained_from = 0;
    uint64_t produced = 0;
    uint64_t released = 0;
    uint32_t capacity_samples = 0;
    uint64_t overwritten_samples = 0;
};

struct PullSamplesRequest {
    uint64_t boot_epoch = 0;
    uint64_t from = 0;
    uint64_t release_before = 0;
    uint16_t max_samples = 0;
};

struct ReleaseSamplesRequest {
    uint64_t boot_epoch = 0;
    uint64_t release_before = 0;
};

struct RawSample {
    int32_t x = 0;
    int32_t y = 0;
    int32_t z = 0;
};

struct TimingEvidence {
    uint32_t segment_id = 0;
    uint64_t first_device_time_us = 0;
    uint64_t last_device_time_us = 0;
    uint32_t max_fit_residual_us = 0;
    uint16_t quality_flags = 0;
};

struct SampleChunk {
    uint64_t boot_epoch = 0;
    uint64_t retained_from = 0;
    uint64_t produced = 0;
    uint64_t first = 0;
    uint16_t sample_count = 0;
    uint32_t config_revision = 0;
    TimingEvidence timing{};
    RawSample samples[kMaxWireChunkSamples]{};
};

struct GapEvidence {
    uint64_t boot_epoch = 0;
    uint64_t retained_from = 0;
    uint64_t produced = 0;
    uint64_t first_missing = 0;
    uint64_t missing_count = 0;
    GapReasonCode reason = GapReasonCode::UnknownDiscontinuity;
};

struct ContinuityBreakEvidence {
    uint64_t boot_epoch = 0;
    uint64_t retained_from = 0;
    uint64_t produced = 0;
    uint64_t at = 0;
    ContinuityBreakReasonCode reason = ContinuityBreakReasonCode::Unknown;
};

using RawSampleReader = bool (*)(const void* context, size_t index, RawSample& output);

// Encoding-only view used by firmware to serialize directly from bounded
// storage without first copying a complete chunk into SampleChunk::samples.
struct SampleChunkView {
    uint64_t boot_epoch = 0;
    uint64_t retained_from = 0;
    uint64_t produced = 0;
    uint64_t first = 0;
    uint16_t sample_count = 0;
    uint32_t config_revision = 0;
    TimingEvidence timing{};
    const void* sample_context = nullptr;
    RawSampleReader read_sample = nullptr;
};

struct PullResponse {
    PullResultCode result = PullResultCode::NoData;
    StreamState state{};
    uint64_t actual_boot_epoch = 0;
    SampleChunk chunk{};
    GapEvidence gap{};
    ContinuityBreakEvidence continuity_break{};
};

enum class MeasurementCodecStatus : uint8_t {
    Ok = 0,
    OutputTooSmall,
    InvalidPayloadLength,
    UnknownResponseStatus,
    RemoteError,
    UnknownResult,
    ZeroBootEpoch,
    ZeroJournalCapacity,
    InvalidCursorOrder,
    ZeroSampleLimit,
    InvalidRelease,
    EmptySampleChunk,
    ChunkAheadOfProduced,
    ChunkBeforeRetained,
    ChunkTooLarge,
    InvalidAxis,
    ZeroConfigRevision,
    ZeroTimingSegment,
    UnknownTimingFlags,
    ReversedTimingAnchors,
    InvalidTimingAnchors,
    MissingSampleReader,
    SampleReadFailed,
    EmptyGap,
    GapBeforeRetained,
    GapAheadOfProduced,
    UnknownGapReason,
    BreakBeforeRetained,
    BreakAheadOfProduced,
    UnknownContinuityBreakReason,
};

MeasurementCodecStatus validate(const StreamState& state);
MeasurementCodecStatus validate(const PullSamplesRequest& request);
MeasurementCodecStatus validate(const ReleaseSamplesRequest& request);
MeasurementCodecStatus validate(const SampleChunk& chunk);
MeasurementCodecStatus validate(const SampleChunkView& chunk);
MeasurementCodecStatus validate(const GapEvidence& gap);
MeasurementCodecStatus validate(const ContinuityBreakEvidence& value);

MeasurementCodecStatus encode_pull_request(
    const PullSamplesRequest& request, uint8_t* output, size_t output_capacity);
MeasurementCodecStatus decode_pull_request(
    const uint8_t* payload, size_t payload_size, PullSamplesRequest& output);

MeasurementCodecStatus encode_release_request(
    const ReleaseSamplesRequest& request, uint8_t* output, size_t output_capacity);
MeasurementCodecStatus decode_release_request(
    const uint8_t* payload, size_t payload_size, ReleaseSamplesRequest& output);

MeasurementCodecStatus encode_stream_state_success(
    const StreamState& state, uint8_t* output, size_t output_capacity);
MeasurementCodecStatus decode_stream_state_success(
    const uint8_t* payload, size_t payload_size, StreamState& output,
    ResponseStatus& remote_status);

MeasurementCodecStatus encode_pull_response(
    const PullResponse& response, uint8_t* output, size_t output_capacity,
    size_t& output_size);
MeasurementCodecStatus encode_sample_chunk_view(
    const SampleChunkView& chunk, uint8_t* output, size_t output_capacity,
    size_t& output_size);
MeasurementCodecStatus decode_pull_response(
    const uint8_t* payload, size_t payload_size, PullResponse& output,
    ResponseStatus& remote_status);

}  // namespace ProtocolV3
