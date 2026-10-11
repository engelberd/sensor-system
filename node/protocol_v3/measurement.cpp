#include "protocol_v3/measurement.h"

#include <climits>

namespace ProtocolV3 {
namespace {

void write_u16(uint16_t value, uint8_t* output) {
    output[0] = static_cast<uint8_t>(value);
    output[1] = static_cast<uint8_t>(value >> 8u);
}

void write_u32(uint32_t value, uint8_t* output) {
    for (size_t i = 0; i < 4; ++i) output[i] = static_cast<uint8_t>(value >> (8u * i));
}

void write_u64(uint64_t value, uint8_t* output) {
    for (size_t i = 0; i < 8; ++i) output[i] = static_cast<uint8_t>(value >> (8u * i));
}

uint16_t read_u16(const uint8_t* input) {
    return static_cast<uint16_t>(input[0]) |
           static_cast<uint16_t>(static_cast<uint16_t>(input[1]) << 8u);
}

uint32_t read_u32(const uint8_t* input) {
    uint32_t value = 0;
    for (size_t i = 0; i < 4; ++i) value |= static_cast<uint32_t>(input[i]) << (8u * i);
    return value;
}

uint64_t read_u64(const uint8_t* input) {
    uint64_t value = 0;
    for (size_t i = 0; i < 8; ++i) value |= static_cast<uint64_t>(input[i]) << (8u * i);
    return value;
}

bool decode_status(uint8_t value, ResponseStatus& output) {
    if (value > static_cast<uint8_t>(ResponseStatus::InternalError)) return false;
    output = static_cast<ResponseStatus>(value);
    return true;
}

bool decode_result(uint8_t value, PullResultCode& output) {
    if (value > static_cast<uint8_t>(PullResultCode::ContinuityBreak)) return false;
    output = static_cast<PullResultCode>(value);
    return true;
}

bool decode_continuity_break_reason(
    uint8_t value, ContinuityBreakReasonCode& output) {
    if (value > static_cast<uint8_t>(ContinuityBreakReasonCode::ProcessingReset)) {
        return false;
    }
    output = static_cast<ContinuityBreakReasonCode>(value);
    return true;
}

bool decode_gap_reason(uint8_t value, GapReasonCode& output) {
    if (value > static_cast<uint8_t>(GapReasonCode::IngressOverflow)) return false;
    output = static_cast<GapReasonCode>(value);
    return true;
}

void encode_state_fields(const StreamState& state, uint8_t* output) {
    write_u64(state.boot_epoch, output + 0);
    write_u64(state.retained_from, output + 8);
    write_u64(state.produced, output + 16);
    write_u64(state.released, output + 24);
    write_u32(state.capacity_samples, output + 32);
    write_u64(state.overwritten_samples, output + 36);
}

void decode_state_fields(const uint8_t* payload, StreamState& output) {
    output.boot_epoch = read_u64(payload + 0);
    output.retained_from = read_u64(payload + 8);
    output.produced = read_u64(payload + 16);
    output.released = read_u64(payload + 24);
    output.capacity_samples = read_u32(payload + 32);
    output.overwritten_samples = read_u64(payload + 36);
}

bool valid_axis(int32_t value) {
    return value >= -0x00800000 && value <= 0x007fffff;
}

MeasurementCodecStatus validate_chunk_fields(
    uint64_t boot_epoch, uint64_t retained_from, uint64_t produced,
    uint64_t first, uint16_t sample_count, uint32_t config_revision,
    const TimingEvidence& timing) {
    if (boot_epoch == 0) return MeasurementCodecStatus::ZeroBootEpoch;
    if (retained_from > produced) return MeasurementCodecStatus::InvalidCursorOrder;
    if (sample_count == 0) return MeasurementCodecStatus::EmptySampleChunk;
    if (sample_count > kMaxWireChunkSamples) return MeasurementCodecStatus::ChunkTooLarge;
    if (first < retained_from) return MeasurementCodecStatus::ChunkBeforeRetained;
    if (first > UINT64_MAX - sample_count || first + sample_count > produced) {
        return MeasurementCodecStatus::ChunkAheadOfProduced;
    }
    if (config_revision == 0) return MeasurementCodecStatus::ZeroConfigRevision;
    if (timing.segment_id == 0) return MeasurementCodecStatus::ZeroTimingSegment;
    if ((timing.quality_flags & static_cast<uint16_t>(~kKnownTimingFlags)) != 0) {
        return MeasurementCodecStatus::UnknownTimingFlags;
    }
    if (timing.last_device_time_us < timing.first_device_time_us) {
        return MeasurementCodecStatus::ReversedTimingAnchors;
    }
    if ((timing.quality_flags & kTimingValid) != 0) {
        if ((sample_count == 1 &&
             timing.first_device_time_us != timing.last_device_time_us) ||
            (sample_count > 1 &&
             timing.first_device_time_us == timing.last_device_time_us)) {
            return MeasurementCodecStatus::InvalidTimingAnchors;
        }
    }
    return MeasurementCodecStatus::Ok;
}

void encode_chunk_header(
    uint64_t boot_epoch, uint64_t retained_from, uint64_t produced,
    uint64_t first, uint16_t sample_count, uint32_t config_revision,
    const TimingEvidence& timing, uint8_t* output) {
    output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
    output[1] = static_cast<uint8_t>(PullResultCode::SampleChunk);
    write_u64(boot_epoch, output + 2);
    write_u64(retained_from, output + 10);
    write_u64(produced, output + 18);
    write_u64(first, output + 26);
    write_u16(sample_count, output + 34);
    write_u32(config_revision, output + 36);
    write_u32(timing.segment_id, output + 40);
    write_u64(timing.first_device_time_us, output + 44);
    write_u64(timing.last_device_time_us, output + 52);
    write_u32(timing.max_fit_residual_us, output + 60);
    write_u16(timing.quality_flags, output + 64);
}

void encode_i24(int32_t value, uint8_t* output) {
    const uint32_t raw = static_cast<uint32_t>(value) & 0x00ffffffu;
    output[0] = static_cast<uint8_t>(raw);
    output[1] = static_cast<uint8_t>(raw >> 8u);
    output[2] = static_cast<uint8_t>(raw >> 16u);
}

int32_t decode_i24(const uint8_t* input) {
    const uint32_t raw = static_cast<uint32_t>(input[0]) |
                         (static_cast<uint32_t>(input[1]) << 8u) |
                         (static_cast<uint32_t>(input[2]) << 16u);
    const int32_t magnitude = static_cast<int32_t>(raw);
    return (raw & 0x00800000u) == 0u ? magnitude : magnitude - 0x01000000;
}

MeasurementCodecStatus encode_state_result(
    PullResultCode result, const StreamState& state, uint8_t* output,
    size_t output_capacity, size_t& output_size) {
    const MeasurementCodecStatus status = validate(state);
    if (status != MeasurementCodecStatus::Ok) return status;
    if (output == nullptr || output_capacity < 46) return MeasurementCodecStatus::OutputTooSmall;
    output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
    output[1] = static_cast<uint8_t>(result);
    encode_state_fields(state, output + 2);
    output_size = 46;
    return MeasurementCodecStatus::Ok;
}

}  // namespace

MeasurementCodecStatus validate(const StreamState& state) {
    if (state.boot_epoch == 0) return MeasurementCodecStatus::ZeroBootEpoch;
    if (state.capacity_samples == 0) return MeasurementCodecStatus::ZeroJournalCapacity;
    if (state.retained_from > state.produced || state.released > state.produced) {
        return MeasurementCodecStatus::InvalidCursorOrder;
    }
    return MeasurementCodecStatus::Ok;
}

MeasurementCodecStatus validate(const PullSamplesRequest& request) {
    if (request.boot_epoch == 0) return MeasurementCodecStatus::ZeroBootEpoch;
    if (request.max_samples == 0) return MeasurementCodecStatus::ZeroSampleLimit;
    if (request.release_before > request.from) return MeasurementCodecStatus::InvalidRelease;
    return MeasurementCodecStatus::Ok;
}

MeasurementCodecStatus validate(const ReleaseSamplesRequest& request) {
    return request.boot_epoch == 0
        ? MeasurementCodecStatus::ZeroBootEpoch
        : MeasurementCodecStatus::Ok;
}

MeasurementCodecStatus validate(const SampleChunk& chunk) {
    const MeasurementCodecStatus fields = validate_chunk_fields(
        chunk.boot_epoch, chunk.retained_from, chunk.produced, chunk.first,
        chunk.sample_count, chunk.config_revision, chunk.timing);
    if (fields != MeasurementCodecStatus::Ok) return fields;
    for (size_t i = 0; i < chunk.sample_count; ++i) {
        if (!valid_axis(chunk.samples[i].x) || !valid_axis(chunk.samples[i].y) ||
            !valid_axis(chunk.samples[i].z)) {
            return MeasurementCodecStatus::InvalidAxis;
        }
    }
    return MeasurementCodecStatus::Ok;
}

MeasurementCodecStatus validate(const SampleChunkView& chunk) {
    const MeasurementCodecStatus fields = validate_chunk_fields(
        chunk.boot_epoch, chunk.retained_from, chunk.produced, chunk.first,
        chunk.sample_count, chunk.config_revision, chunk.timing);
    if (fields != MeasurementCodecStatus::Ok) return fields;
    if (chunk.sample_context == nullptr || chunk.read_sample == nullptr) {
        return MeasurementCodecStatus::MissingSampleReader;
    }
    return MeasurementCodecStatus::Ok;
}

MeasurementCodecStatus validate(const GapEvidence& gap) {
    if (gap.boot_epoch == 0) return MeasurementCodecStatus::ZeroBootEpoch;
    if (gap.retained_from > gap.produced) {
        return MeasurementCodecStatus::InvalidCursorOrder;
    }
    if (gap.missing_count == 0) return MeasurementCodecStatus::EmptyGap;
    if (gap.first_missing < gap.retained_from) {
        return MeasurementCodecStatus::GapBeforeRetained;
    }
    if (gap.first_missing > UINT64_MAX - gap.missing_count ||
        gap.first_missing + gap.missing_count > gap.produced) {
        return MeasurementCodecStatus::GapAheadOfProduced;
    }
    if (static_cast<uint8_t>(gap.reason) >
        static_cast<uint8_t>(GapReasonCode::IngressOverflow)) {
        return MeasurementCodecStatus::UnknownGapReason;
    }
    return MeasurementCodecStatus::Ok;
}

MeasurementCodecStatus validate(const ContinuityBreakEvidence& value) {
    if (value.boot_epoch == 0) return MeasurementCodecStatus::ZeroBootEpoch;
    if (value.retained_from > value.produced) {
        return MeasurementCodecStatus::InvalidCursorOrder;
    }
    if (value.at < value.retained_from) {
        return MeasurementCodecStatus::BreakBeforeRetained;
    }
    if (value.at == UINT64_MAX || value.at + 1u > value.produced) {
        return MeasurementCodecStatus::BreakAheadOfProduced;
    }
    if (static_cast<uint8_t>(value.reason) >
        static_cast<uint8_t>(ContinuityBreakReasonCode::ProcessingReset)) {
        return MeasurementCodecStatus::UnknownContinuityBreakReason;
    }
    return MeasurementCodecStatus::Ok;
}

MeasurementCodecStatus encode_pull_request(
    const PullSamplesRequest& request, uint8_t* output, size_t output_capacity) {
    const MeasurementCodecStatus status = validate(request);
    if (status != MeasurementCodecStatus::Ok) return status;
    if (output == nullptr || output_capacity < kPullRequestBytes) {
        return MeasurementCodecStatus::OutputTooSmall;
    }
    write_u64(request.boot_epoch, output + 0);
    write_u64(request.from, output + 8);
    write_u64(request.release_before, output + 16);
    write_u16(request.max_samples, output + 24);
    return MeasurementCodecStatus::Ok;
}

MeasurementCodecStatus decode_pull_request(
    const uint8_t* payload, size_t payload_size, PullSamplesRequest& output) {
    if (payload == nullptr || payload_size != kPullRequestBytes) {
        return MeasurementCodecStatus::InvalidPayloadLength;
    }
    output.boot_epoch = read_u64(payload + 0);
    output.from = read_u64(payload + 8);
    output.release_before = read_u64(payload + 16);
    output.max_samples = read_u16(payload + 24);
    return validate(output);
}

MeasurementCodecStatus encode_release_request(
    const ReleaseSamplesRequest& request, uint8_t* output, size_t output_capacity) {
    const MeasurementCodecStatus status = validate(request);
    if (status != MeasurementCodecStatus::Ok) return status;
    if (output == nullptr || output_capacity < kReleaseRequestBytes) {
        return MeasurementCodecStatus::OutputTooSmall;
    }
    write_u64(request.boot_epoch, output + 0);
    write_u64(request.release_before, output + 8);
    return MeasurementCodecStatus::Ok;
}

MeasurementCodecStatus decode_release_request(
    const uint8_t* payload, size_t payload_size, ReleaseSamplesRequest& output) {
    if (payload == nullptr || payload_size != kReleaseRequestBytes) {
        return MeasurementCodecStatus::InvalidPayloadLength;
    }
    output.boot_epoch = read_u64(payload + 0);
    output.release_before = read_u64(payload + 8);
    return validate(output);
}

MeasurementCodecStatus encode_stream_state_success(
    const StreamState& state, uint8_t* output, size_t output_capacity) {
    const MeasurementCodecStatus status = validate(state);
    if (status != MeasurementCodecStatus::Ok) return status;
    if (output == nullptr || output_capacity < kStreamStateSuccessBytes) {
        return MeasurementCodecStatus::OutputTooSmall;
    }
    output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
    encode_state_fields(state, output + 1);
    return MeasurementCodecStatus::Ok;
}

MeasurementCodecStatus decode_stream_state_success(
    const uint8_t* payload, size_t payload_size, StreamState& output,
    ResponseStatus& remote_status) {
    if (payload == nullptr || payload_size == 0) return MeasurementCodecStatus::InvalidPayloadLength;
    if (!decode_status(payload[0], remote_status)) return MeasurementCodecStatus::UnknownResponseStatus;
    if (remote_status != ResponseStatus::Ok) return MeasurementCodecStatus::RemoteError;
    if (payload_size != kStreamStateSuccessBytes) return MeasurementCodecStatus::InvalidPayloadLength;
    decode_state_fields(payload + 1, output);
    return validate(output);
}

MeasurementCodecStatus encode_pull_response(
    const PullResponse& response, uint8_t* output, size_t output_capacity,
    size_t& output_size) {
    output_size = 0;
    if (response.result == PullResultCode::NoData ||
        response.result == PullResultCode::RangeUnavailable ||
        response.result == PullResultCode::InvalidRelease) {
        return encode_state_result(response.result, response.state, output, output_capacity, output_size);
    }
    if (response.result == PullResultCode::StreamChanged) {
        if (response.actual_boot_epoch == 0) return MeasurementCodecStatus::ZeroBootEpoch;
        if (output == nullptr || output_capacity < 10) return MeasurementCodecStatus::OutputTooSmall;
        output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
        output[1] = static_cast<uint8_t>(response.result);
        write_u64(response.actual_boot_epoch, output + 2);
        output_size = 10;
        return MeasurementCodecStatus::Ok;
    }
    if (response.result == PullResultCode::Gap) {
        const MeasurementCodecStatus status = validate(response.gap);
        if (status != MeasurementCodecStatus::Ok) return status;
        if (output == nullptr || output_capacity < kGapResponseBytes) {
            return MeasurementCodecStatus::OutputTooSmall;
        }
        output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
        output[1] = static_cast<uint8_t>(PullResultCode::Gap);
        write_u64(response.gap.boot_epoch, output + 2);
        write_u64(response.gap.retained_from, output + 10);
        write_u64(response.gap.produced, output + 18);
        write_u64(response.gap.first_missing, output + 26);
        write_u64(response.gap.missing_count, output + 34);
        output[42] = static_cast<uint8_t>(response.gap.reason);
        output_size = kGapResponseBytes;
        return MeasurementCodecStatus::Ok;
    }
    if (response.result == PullResultCode::ContinuityBreak) {
        const MeasurementCodecStatus status = validate(response.continuity_break);
        if (status != MeasurementCodecStatus::Ok) return status;
        if (output == nullptr || output_capacity < kContinuityBreakResponseBytes) {
            return MeasurementCodecStatus::OutputTooSmall;
        }
        output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
        output[1] = static_cast<uint8_t>(PullResultCode::ContinuityBreak);
        write_u64(response.continuity_break.boot_epoch, output + 2);
        write_u64(response.continuity_break.retained_from, output + 10);
        write_u64(response.continuity_break.produced, output + 18);
        write_u64(response.continuity_break.at, output + 26);
        output[34] = static_cast<uint8_t>(response.continuity_break.reason);
        output_size = kContinuityBreakResponseBytes;
        return MeasurementCodecStatus::Ok;
    }

    const MeasurementCodecStatus status = validate(response.chunk);
    if (status != MeasurementCodecStatus::Ok) return status;
    const size_t required = kChunkFixedBytes +
                            static_cast<size_t>(response.chunk.sample_count) * kXyz24SampleBytes;
    if (output == nullptr || output_capacity < required) return MeasurementCodecStatus::OutputTooSmall;
    encode_chunk_header(
        response.chunk.boot_epoch, response.chunk.retained_from,
        response.chunk.produced, response.chunk.first, response.chunk.sample_count,
        response.chunk.config_revision, response.chunk.timing, output);
    for (size_t i = 0; i < response.chunk.sample_count; ++i) {
        const size_t offset = kChunkFixedBytes + i * kXyz24SampleBytes;
        encode_i24(response.chunk.samples[i].x, output + offset);
        encode_i24(response.chunk.samples[i].y, output + offset + 3);
        encode_i24(response.chunk.samples[i].z, output + offset + 6);
    }
    output_size = required;
    return MeasurementCodecStatus::Ok;
}

MeasurementCodecStatus encode_sample_chunk_view(
    const SampleChunkView& chunk, uint8_t* output, size_t output_capacity,
    size_t& output_size) {
    output_size = 0;
    const MeasurementCodecStatus status = validate(chunk);
    if (status != MeasurementCodecStatus::Ok) return status;
    const size_t required = kChunkFixedBytes +
                            static_cast<size_t>(chunk.sample_count) * kXyz24SampleBytes;
    if (output == nullptr || output_capacity < required) {
        return MeasurementCodecStatus::OutputTooSmall;
    }

    encode_chunk_header(
        chunk.boot_epoch, chunk.retained_from, chunk.produced, chunk.first,
        chunk.sample_count, chunk.config_revision, chunk.timing, output);
    for (size_t i = 0; i < chunk.sample_count; ++i) {
        RawSample sample{};
        if (!chunk.read_sample(chunk.sample_context, i, sample)) {
            output_size = 0;
            return MeasurementCodecStatus::SampleReadFailed;
        }
        if (!valid_axis(sample.x) || !valid_axis(sample.y) || !valid_axis(sample.z)) {
            output_size = 0;
            return MeasurementCodecStatus::InvalidAxis;
        }
        const size_t offset = kChunkFixedBytes + i * kXyz24SampleBytes;
        encode_i24(sample.x, output + offset);
        encode_i24(sample.y, output + offset + 3);
        encode_i24(sample.z, output + offset + 6);
    }
    output_size = required;
    return MeasurementCodecStatus::Ok;
}

MeasurementCodecStatus decode_pull_response(
    const uint8_t* payload, size_t payload_size, PullResponse& output,
    ResponseStatus& remote_status) {
    if (payload == nullptr || payload_size == 0) return MeasurementCodecStatus::InvalidPayloadLength;
    if (!decode_status(payload[0], remote_status)) return MeasurementCodecStatus::UnknownResponseStatus;
    if (remote_status != ResponseStatus::Ok) return MeasurementCodecStatus::RemoteError;
    if (payload_size < 2 || !decode_result(payload[1], output.result)) {
        return payload_size < 2 ? MeasurementCodecStatus::InvalidPayloadLength
                                : MeasurementCodecStatus::UnknownResult;
    }
    if (output.result == PullResultCode::NoData ||
        output.result == PullResultCode::RangeUnavailable ||
        output.result == PullResultCode::InvalidRelease) {
        if (payload_size != 46) return MeasurementCodecStatus::InvalidPayloadLength;
        decode_state_fields(payload + 2, output.state);
        return validate(output.state);
    }
    if (output.result == PullResultCode::StreamChanged) {
        if (payload_size != 10) return MeasurementCodecStatus::InvalidPayloadLength;
        output.actual_boot_epoch = read_u64(payload + 2);
        return output.actual_boot_epoch == 0 ? MeasurementCodecStatus::ZeroBootEpoch
                                             : MeasurementCodecStatus::Ok;
    }
    if (output.result == PullResultCode::Gap) {
        if (payload_size != kGapResponseBytes) {
            return MeasurementCodecStatus::InvalidPayloadLength;
        }
        GapEvidence& gap = output.gap;
        gap.boot_epoch = read_u64(payload + 2);
        gap.retained_from = read_u64(payload + 10);
        gap.produced = read_u64(payload + 18);
        gap.first_missing = read_u64(payload + 26);
        gap.missing_count = read_u64(payload + 34);
        if (!decode_gap_reason(payload[42], gap.reason)) {
            return MeasurementCodecStatus::UnknownGapReason;
        }
        return validate(gap);
    }
    if (output.result == PullResultCode::ContinuityBreak) {
        if (payload_size != kContinuityBreakResponseBytes) {
            return MeasurementCodecStatus::InvalidPayloadLength;
        }
        ContinuityBreakEvidence& value = output.continuity_break;
        value.boot_epoch = read_u64(payload + 2);
        value.retained_from = read_u64(payload + 10);
        value.produced = read_u64(payload + 18);
        value.at = read_u64(payload + 26);
        if (!decode_continuity_break_reason(payload[34], value.reason)) {
            return MeasurementCodecStatus::UnknownContinuityBreakReason;
        }
        return validate(value);
    }

    if (payload_size < kChunkFixedBytes) return MeasurementCodecStatus::InvalidPayloadLength;
    SampleChunk& chunk = output.chunk;
    chunk.boot_epoch = read_u64(payload + 2);
    chunk.retained_from = read_u64(payload + 10);
    chunk.produced = read_u64(payload + 18);
    chunk.first = read_u64(payload + 26);
    chunk.sample_count = read_u16(payload + 34);
    if (chunk.sample_count > kMaxWireChunkSamples) return MeasurementCodecStatus::ChunkTooLarge;
    const size_t expected = kChunkFixedBytes +
                            static_cast<size_t>(chunk.sample_count) * kXyz24SampleBytes;
    if (payload_size != expected) return MeasurementCodecStatus::InvalidPayloadLength;
    chunk.config_revision = read_u32(payload + 36);
    chunk.timing.segment_id = read_u32(payload + 40);
    chunk.timing.first_device_time_us = read_u64(payload + 44);
    chunk.timing.last_device_time_us = read_u64(payload + 52);
    chunk.timing.max_fit_residual_us = read_u32(payload + 60);
    chunk.timing.quality_flags = read_u16(payload + 64);
    for (size_t i = 0; i < chunk.sample_count; ++i) {
        const size_t offset = kChunkFixedBytes + i * kXyz24SampleBytes;
        chunk.samples[i].x = decode_i24(payload + offset);
        chunk.samples[i].y = decode_i24(payload + offset + 3);
        chunk.samples[i].z = decode_i24(payload + offset + 6);
    }
    return validate(chunk);
}

}  // namespace ProtocolV3
