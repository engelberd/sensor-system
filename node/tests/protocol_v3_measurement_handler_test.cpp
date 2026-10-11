#include <cassert>
#include <cstddef>
#include <cstdint>

#include "measurement/delivery_session.h"
#include "protocol_v3/measurement_handler.h"

namespace {
using namespace measurement;
using namespace ProtocolV3;

constexpr uint64_t kEpoch = 0x1122334455667788ull;

SampleRecord sample(int32_t value) {
    return {
        1000u + static_cast<uint64_t>(value) * 4000u,
        value,
        -value,
        value + 1,
        3,
        4,
        kTimingValidFlag,
        0,
    };
}

struct Fixture {
    MeasurementJournal<8, 8> journal{};
    DeliverySession<8, 4> session{kEpoch, journal};
    MeasurementServiceHandler<8, 4> handler{session};

    Fixture() {
        for (int32_t value = 0; value < 6; ++value) {
            assert(journal.append_sample(
                       static_cast<uint64_t>(value), sample(value)).status ==
                   StreamAppendStatus::Ok);
        }
    }
};

void test_get_stream_state_and_malformed_request() {
    Fixture fixture;
    uint8_t output[kMaxPayloadBytes]{};
    MeasurementHandlerResult result = fixture.handler.handle(
        kGetStreamStateMessageId, nullptr, 0, output, sizeof(output));
    assert(result.payload_size == kStreamStateSuccessBytes);
    assert(!result.completion_required);

    StreamState state{};
    ResponseStatus remote = ResponseStatus::InternalError;
    assert(decode_stream_state_success(
               output, result.payload_size, state, remote) ==
           MeasurementCodecStatus::Ok);
    assert(state.boot_epoch == kEpoch && state.produced == 6);
    assert(state.capacity_samples == 8);

    const uint8_t unexpected = 1;
    result = fixture.handler.handle(
        kGetStreamStateMessageId, &unexpected, 1, output, sizeof(output));
    assert(result.payload_size == 1);
    assert(output[0] == static_cast<uint8_t>(ResponseStatus::MalformedRequest));
}

void test_pull_encodes_directly_and_completes_explicitly() {
    Fixture fixture;
    const PullSamplesRequest request{kEpoch, 0, 0, 4};
    uint8_t request_bytes[kPullRequestBytes]{};
    assert(encode_pull_request(request, request_bytes, sizeof(request_bytes)) ==
           MeasurementCodecStatus::Ok);

    uint8_t output[kMaxPayloadBytes]{};
    const MeasurementHandlerResult result = fixture.handler.handle(
        kPullSamplesMessageId, request_bytes, sizeof(request_bytes),
        output, sizeof(output));
    assert(result.completion_required && result.delivery_token != 0);
    assert(fixture.journal.state().delivered == 0);

    PullResponse decoded{};
    ResponseStatus remote = ResponseStatus::InternalError;
    assert(decode_pull_response(output, result.payload_size, decoded, remote) ==
           MeasurementCodecStatus::Ok);
    assert(decoded.result == PullResultCode::SampleChunk);
    assert(decoded.chunk.first == 0 && decoded.chunk.sample_count == 4);
    assert(decoded.chunk.samples[0].x == 0 && decoded.chunk.samples[3].z == 4);

    assert(fixture.handler.complete(result.delivery_token) == CompletionStatus::Ok);
    assert(fixture.journal.state().delivered == 4);
}

void test_encoding_failure_aborts_preparation() {
    Fixture fixture;
    const PullSamplesRequest request{kEpoch, 0, 0, 4};
    uint8_t request_bytes[kPullRequestBytes]{};
    encode_pull_request(request, request_bytes, sizeof(request_bytes));

    uint8_t too_small[1]{};
    const MeasurementHandlerResult result = fixture.handler.handle(
        kPullSamplesMessageId, request_bytes, sizeof(request_bytes),
        too_small, sizeof(too_small));
    assert(!result.completion_required && result.payload_size == 1);
    assert(too_small[0] == static_cast<uint8_t>(ResponseStatus::InternalError));
    assert(!fixture.session.has_prepared_chunk());
    assert(fixture.journal.state().delivered == 0);
}

void test_pull_results_and_release_mapping() {
    Fixture fixture;
    uint8_t request_bytes[kPullRequestBytes]{};
    uint8_t output[kMaxPayloadBytes]{};
    ResponseStatus remote = ResponseStatus::InternalError;
    PullResponse decoded{};

    encode_pull_request({kEpoch, 6, 0, 4}, request_bytes, sizeof(request_bytes));
    MeasurementHandlerResult result = fixture.handler.handle(
        kPullSamplesMessageId, request_bytes, sizeof(request_bytes),
        output, sizeof(output));
    assert(decode_pull_response(output, result.payload_size, decoded, remote) ==
           MeasurementCodecStatus::Ok);
    assert(decoded.result == PullResultCode::NoData);

    encode_pull_request({kEpoch + 1, 0, 0, 4}, request_bytes, sizeof(request_bytes));
    result = fixture.handler.handle(
        kPullSamplesMessageId, request_bytes, sizeof(request_bytes),
        output, sizeof(output));
    assert(decode_pull_response(output, result.payload_size, decoded, remote) ==
           MeasurementCodecStatus::Ok);
    assert(decoded.result == PullResultCode::StreamChanged);
    assert(decoded.actual_boot_epoch == kEpoch);

    // encode_pull_request correctly rejects this semantic error, so construct
    // the wire bytes from a valid request and change release_before in place.
    encode_pull_request({kEpoch, 1, 1, 4}, request_bytes, sizeof(request_bytes));
    for (size_t index = 8; index < 16; ++index) request_bytes[index] = 0;
    result = fixture.handler.handle(
        kPullSamplesMessageId, request_bytes, sizeof(request_bytes),
        output, sizeof(output));
    assert(decode_pull_response(output, result.payload_size, decoded, remote) ==
           MeasurementCodecStatus::Ok);
    assert(decoded.result == PullResultCode::InvalidRelease);

    PullSamplesRequest first_pull{kEpoch, 0, 0, 2};
    encode_pull_request(first_pull, request_bytes, sizeof(request_bytes));
    result = fixture.handler.handle(
        kPullSamplesMessageId, request_bytes, sizeof(request_bytes),
        output, sizeof(output));
    assert(fixture.handler.complete(result.delivery_token) == CompletionStatus::Ok);

    uint8_t release_bytes[kReleaseRequestBytes]{};
    encode_release_request({kEpoch, 2}, release_bytes, sizeof(release_bytes));
    result = fixture.handler.handle(
        kReleaseSamplesMessageId, release_bytes, sizeof(release_bytes),
        output, sizeof(output));
    StreamState released{};
    assert(decode_stream_state_success(
               output, result.payload_size, released, remote) ==
           MeasurementCodecStatus::Ok);
    assert(released.released == 2 && released.retained_from == 2);
}

void test_exact_gap_is_encoded_and_completed_explicitly() {
    MeasurementJournal<8, 8> journal;
    assert(journal.append_gap({0, 3, GapReason::SourceLoss}).status ==
           StreamAppendStatus::Ok);
    DeliverySession<8, 4> session{kEpoch, journal};
    MeasurementServiceHandler<8, 4> handler{session};

    const PullSamplesRequest request{kEpoch, 0, 0, 4};
    uint8_t request_bytes[kPullRequestBytes]{};
    assert(encode_pull_request(request, request_bytes, sizeof(request_bytes)) ==
           MeasurementCodecStatus::Ok);
    uint8_t output[kMaxPayloadBytes]{};
    const MeasurementHandlerResult result = handler.handle(
        kPullSamplesMessageId, request_bytes, sizeof(request_bytes),
        output, sizeof(output));
    assert(result.completion_required && result.delivery_token != 0);
    assert(journal.state().delivered == 0);

    PullResponse decoded{};
    ResponseStatus remote = ResponseStatus::InternalError;
    assert(decode_pull_response(output, result.payload_size, decoded, remote) ==
           MeasurementCodecStatus::Ok);
    assert(decoded.result == PullResultCode::Gap);
    assert(decoded.gap.first_missing == 0 && decoded.gap.missing_count == 3);
    assert(decoded.gap.reason == GapReasonCode::SourceLoss);

    assert(handler.complete(result.delivery_token) == CompletionStatus::Ok);
    assert(journal.state().delivered == 3);
}

void test_continuity_break_is_encoded_and_completed_explicitly() {
    MeasurementJournal<8, 8> journal;
    assert(journal.append_continuity_break(
               {0, ContinuityBreakReason::SensorOverflow}).status ==
           StreamAppendStatus::Ok);
    DeliverySession<8, 4> session{kEpoch, journal};
    MeasurementServiceHandler<8, 4> handler{session};

    uint8_t request_bytes[kPullRequestBytes]{};
    assert(encode_pull_request(
               {kEpoch, 0, 0, 4}, request_bytes, sizeof(request_bytes)) ==
           MeasurementCodecStatus::Ok);
    uint8_t output[kMaxPayloadBytes]{};
    const MeasurementHandlerResult result = handler.handle(
        kPullSamplesMessageId, request_bytes, sizeof(request_bytes),
        output, sizeof(output));
    assert(result.completion_required && result.delivery_token != 0);
    assert(journal.state().delivered == 0);

    PullResponse decoded{};
    ResponseStatus remote = ResponseStatus::InternalError;
    assert(decode_pull_response(output, result.payload_size, decoded, remote) ==
           MeasurementCodecStatus::Ok);
    assert(decoded.result == PullResultCode::ContinuityBreak);
    assert(decoded.continuity_break.at == 0);
    assert(decoded.continuity_break.reason ==
           ContinuityBreakReasonCode::SensorOverflow);

    assert(handler.complete(result.delivery_token) == CompletionStatus::Ok);
    assert(journal.state().delivered == 1);
}

void test_common_errors() {
    Fixture fixture;
    uint8_t output[kMaxPayloadBytes]{};
    uint8_t malformed[1]{0};
    MeasurementHandlerResult result = fixture.handler.handle(
        kPullSamplesMessageId, malformed, sizeof(malformed), output, sizeof(output));
    assert(result.payload_size == 1);
    assert(output[0] == static_cast<uint8_t>(ResponseStatus::MalformedRequest));

    result = fixture.handler.handle(99, nullptr, 0, output, sizeof(output));
    assert(result.payload_size == 1);
    assert(output[0] == static_cast<uint8_t>(ResponseStatus::UnsupportedMessage));
}
}  // namespace

int main() {
    test_get_stream_state_and_malformed_request();
    test_pull_encodes_directly_and_completes_explicitly();
    test_encoding_failure_aborts_preparation();
    test_pull_results_and_release_mapping();
    test_exact_gap_is_encoded_and_completed_explicitly();
    test_continuity_break_is_encoded_and_completed_explicitly();
    test_common_errors();
    return 0;
}
