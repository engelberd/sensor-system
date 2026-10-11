#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <fstream>
#include <string>
#include <vector>

#include "protocol_v3/measurement.h"

namespace {
using namespace ProtocolV3;

constexpr uint64_t kEpoch = 0x1122334455667788ull;

std::vector<uint8_t> read_vector(const char* name) {
    const std::string path = std::string("../host_v2/contracts/golden/") + name;
    std::ifstream input(path, std::ios::binary);
    assert(input.good());
    return std::vector<uint8_t>(std::istreambuf_iterator<char>(input), {});
}

void assert_vector(const uint8_t* bytes, size_t size, const char* name) {
    const auto golden = read_vector(name);
    assert(golden.size() == size);
    assert(std::memcmp(golden.data(), bytes, size) == 0);
}

StreamState state() {
    return {kEpoch, 10, 100, 8, 4096, 10};
}

PullResponse chunk_response() {
    PullResponse response{};
    response.result = PullResultCode::SampleChunk;
    response.chunk.boot_epoch = kEpoch;
    response.chunk.retained_from = 10;
    response.chunk.produced = 100;
    response.chunk.first = 20;
    response.chunk.sample_count = 2;
    response.chunk.config_revision = 3;
    response.chunk.timing = {4, 1000, 5000, 2, kTimingValid};
    response.chunk.samples[0] = {-0x00800000, -1, 0};
    response.chunk.samples[1] = {1, 0x00123456, 0x007fffff};
    return response;
}

PullResponse gap_response() {
    PullResponse response{};
    response.result = PullResultCode::Gap;
    response.gap = {kEpoch, 10, 100, 20, 3, GapReasonCode::SourceLoss};
    return response;
}

PullResponse continuity_break_response() {
    PullResponse response{};
    response.result = PullResultCode::ContinuityBreak;
    response.continuity_break = {
        kEpoch,
        10,
        100,
        20,
        ContinuityBreakReasonCode::SensorReadUncertain,
    };
    return response;
}

bool read_chunk_sample(const void* context, size_t index, RawSample& output) {
    const SampleChunk* chunk = static_cast<const SampleChunk*>(context);
    if (index >= chunk->sample_count) return false;
    output = chunk->samples[index];
    return true;
}

void test_requests_match_golden_vectors() {
    const PullSamplesRequest pull{kEpoch, 20, 16, 48};
    uint8_t pull_bytes[kPullRequestBytes]{};
    assert(encode_pull_request(pull, pull_bytes, sizeof(pull_bytes)) ==
           MeasurementCodecStatus::Ok);
    assert_vector(pull_bytes, sizeof(pull_bytes), "measurement_v5_pull_request.bin");
    PullSamplesRequest decoded_pull{};
    assert(decode_pull_request(pull_bytes, sizeof(pull_bytes), decoded_pull) ==
           MeasurementCodecStatus::Ok);
    assert(decoded_pull.boot_epoch == pull.boot_epoch && decoded_pull.from == pull.from);

    const ReleaseSamplesRequest release{kEpoch, 16};
    uint8_t release_bytes[kReleaseRequestBytes]{};
    assert(encode_release_request(release, release_bytes, sizeof(release_bytes)) ==
           MeasurementCodecStatus::Ok);
    assert_vector(release_bytes, sizeof(release_bytes), "measurement_v5_release_request.bin");
}

void test_stream_state_matches_golden_vector() {
    uint8_t encoded[kStreamStateSuccessBytes]{};
    assert(encode_stream_state_success(state(), encoded, sizeof(encoded)) ==
           MeasurementCodecStatus::Ok);
    assert_vector(encoded, sizeof(encoded), "measurement_v5_stream_state_response.bin");
    StreamState decoded{};
    ResponseStatus remote = ResponseStatus::InternalError;
    assert(decode_stream_state_success(encoded, sizeof(encoded), decoded, remote) ==
           MeasurementCodecStatus::Ok);
    assert(decoded.boot_epoch == kEpoch && decoded.produced == 100);
}

void test_chunk_matches_golden_and_i24_boundaries() {
    const PullResponse response = chunk_response();
    uint8_t encoded[kMaxPayloadBytes]{};
    size_t size = 0;
    assert(encode_pull_response(response, encoded, sizeof(encoded), size) ==
           MeasurementCodecStatus::Ok);
    assert(size == 84);
    assert_vector(encoded, size, "measurement_v5_chunk_response.bin");

    PullResponse decoded{};
    ResponseStatus remote = ResponseStatus::InternalError;
    assert(decode_pull_response(encoded, size, decoded, remote) ==
           MeasurementCodecStatus::Ok);
    assert(decoded.result == PullResultCode::SampleChunk);
    assert(decoded.chunk.samples[0].x == -0x00800000);
    assert(decoded.chunk.samples[0].y == -1);
    assert(decoded.chunk.samples[1].y == 0x00123456);
    assert(decoded.chunk.samples[1].z == 0x007fffff);

    const SampleChunk& owned = response.chunk;
    const SampleChunkView view{
        owned.boot_epoch,
        owned.retained_from,
        owned.produced,
        owned.first,
        owned.sample_count,
        owned.config_revision,
        owned.timing,
        &owned,
        read_chunk_sample,
    };
    uint8_t view_encoded[kMaxPayloadBytes]{};
    size_t view_size = 0;
    assert(encode_sample_chunk_view(
               view, view_encoded, sizeof(view_encoded), view_size) ==
           MeasurementCodecStatus::Ok);
    assert(view_size == size);
    assert(std::memcmp(view_encoded, encoded, size) == 0);
}

void assert_state_result(PullResultCode result, const char* vector_name) {
    PullResponse response{};
    response.result = result;
    response.state = state();
    uint8_t encoded[kMaxPayloadBytes]{};
    size_t size = 0;
    assert(encode_pull_response(response, encoded, sizeof(encoded), size) ==
           MeasurementCodecStatus::Ok);
    assert(size == 46);
    assert_vector(encoded, size, vector_name);
    PullResponse decoded{};
    ResponseStatus remote = ResponseStatus::InternalError;
    assert(decode_pull_response(encoded, size, decoded, remote) ==
           MeasurementCodecStatus::Ok);
    assert(decoded.result == result && decoded.state.produced == 100);
}

void test_all_pull_results_match_golden_vectors() {
    assert_state_result(PullResultCode::NoData, "measurement_v5_no_data_response.bin");
    assert_state_result(
        PullResultCode::RangeUnavailable,
        "measurement_v5_range_unavailable_response.bin");
    assert_state_result(
        PullResultCode::InvalidRelease,
        "measurement_v5_invalid_release_response.bin");

    PullResponse changed{};
    changed.result = PullResultCode::StreamChanged;
    changed.actual_boot_epoch = 9;
    uint8_t encoded[kMaxPayloadBytes]{};
    size_t size = 0;
    assert(encode_pull_response(changed, encoded, sizeof(encoded), size) ==
           MeasurementCodecStatus::Ok);
    assert_vector(encoded, size, "measurement_v5_stream_changed_response.bin");

    const PullResponse gap = gap_response();
    assert(encode_pull_response(gap, encoded, sizeof(encoded), size) ==
           MeasurementCodecStatus::Ok);
    assert(size == kGapResponseBytes);
    assert_vector(encoded, size, "measurement_v5_gap_response.bin");
    PullResponse decoded{};
    ResponseStatus remote = ResponseStatus::InternalError;
    assert(decode_pull_response(encoded, size, decoded, remote) ==
           MeasurementCodecStatus::Ok);
    assert(decoded.result == PullResultCode::Gap);
    assert(decoded.gap.first_missing == 20 && decoded.gap.missing_count == 3);
    assert(decoded.gap.reason == GapReasonCode::SourceLoss);

    const PullResponse continuity = continuity_break_response();
    assert(encode_pull_response(continuity, encoded, sizeof(encoded), size) ==
           MeasurementCodecStatus::Ok);
    assert(size == kContinuityBreakResponseBytes);
    assert_vector(
        encoded, size, "measurement_v5_continuity_break_response.bin");
    assert(decode_pull_response(encoded, size, decoded, remote) ==
           MeasurementCodecStatus::Ok);
    assert(decoded.result == PullResultCode::ContinuityBreak);
    assert(decoded.continuity_break.at == 20);
    assert(decoded.continuity_break.reason ==
           ContinuityBreakReasonCode::SensorReadUncertain);
}

void test_fail_closed_validation() {
    PullResponse invalid = chunk_response();
    invalid.chunk.timing.quality_flags = 0x8000;
    uint8_t output[kMaxPayloadBytes]{};
    size_t size = 0;
    assert(encode_pull_response(invalid, output, sizeof(output), size) ==
           MeasurementCodecStatus::UnknownTimingFlags);

    invalid = chunk_response();
    invalid.chunk.samples[0].x = 0x00800000;
    assert(encode_pull_response(invalid, output, sizeof(output), size) ==
           MeasurementCodecStatus::InvalidAxis);

    PullResponse decoded{};
    ResponseStatus remote = ResponseStatus::Ok;
    const uint8_t busy[] = {static_cast<uint8_t>(ResponseStatus::Busy)};
    assert(decode_pull_response(busy, sizeof(busy), decoded, remote) ==
           MeasurementCodecStatus::RemoteError);
    assert(remote == ResponseStatus::Busy);

    SampleChunkView missing_reader{};
    missing_reader.boot_epoch = kEpoch;
    missing_reader.retained_from = 0;
    missing_reader.produced = 1;
    missing_reader.first = 0;
    missing_reader.sample_count = 1;
    missing_reader.config_revision = 1;
    missing_reader.timing = {1, 1, 1, 0, kTimingValid};
    assert(encode_sample_chunk_view(
               missing_reader, output, sizeof(output), size) ==
           MeasurementCodecStatus::MissingSampleReader);

    invalid = gap_response();
    invalid.gap.missing_count = 0;
    assert(encode_pull_response(invalid, output, sizeof(output), size) ==
           MeasurementCodecStatus::EmptyGap);

    uint8_t bad_gap[kGapResponseBytes]{};
    PullResponse valid_gap = gap_response();
    assert(encode_pull_response(valid_gap, bad_gap, sizeof(bad_gap), size) ==
           MeasurementCodecStatus::Ok);
    bad_gap[42] = 0xff;
    assert(decode_pull_response(bad_gap, sizeof(bad_gap), decoded, remote) ==
           MeasurementCodecStatus::UnknownGapReason);

    uint8_t bad_break[kContinuityBreakResponseBytes]{};
    PullResponse valid_break = continuity_break_response();
    assert(encode_pull_response(
               valid_break, bad_break, sizeof(bad_break), size) ==
           MeasurementCodecStatus::Ok);
    bad_break[34] = 0xff;
    assert(decode_pull_response(
               bad_break, sizeof(bad_break), decoded, remote) ==
           MeasurementCodecStatus::UnknownContinuityBreakReason);
}
}  // namespace

int main() {
    test_requests_match_golden_vectors();
    test_stream_state_matches_golden_vector();
    test_chunk_matches_golden_and_i24_boundaries();
    test_all_pull_results_match_golden_vectors();
    test_fail_closed_validation();
    return 0;
}
