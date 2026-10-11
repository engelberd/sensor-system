#pragma once

#include <cstddef>
#include <cstdint>

#include "measurement/delivery_session.h"
#include "protocol_v3/measurement.h"

namespace ProtocolV3 {

struct MeasurementHandlerResult {
    size_t payload_size = 0;
    bool completion_required = false;
    uint64_t delivery_token = 0;
};

class IMeasurementServiceHandler {
public:
    virtual ~IMeasurementServiceHandler() = default;
    virtual MeasurementHandlerResult handle(
        uint8_t message, const uint8_t* request_payload,
        size_t request_payload_size, uint8_t* response_payload,
        size_t response_capacity) = 0;
    virtual measurement::CompletionStatus complete(uint64_t token) = 0;
    virtual measurement::CompletionStatus abort(uint64_t token) = 0;
};

// Adapts Measurement Service payloads to the domain DeliverySession. Framing,
// addressing and physical transmission remain outside this component.
template <size_t JournalCapacity, size_t MaxChunkSamples, size_t GapCapacity = 8>
class MeasurementServiceHandler final : public IMeasurementServiceHandler {
    static_assert(MaxChunkSamples <= kMaxWireChunkSamples,
                  "delivery chunk does not fit Measurement wire payload");
    static_assert(measurement::kKnownTimingQualityFlags == kKnownTimingFlags,
                  "domain and wire timing flags differ");

public:
    explicit MeasurementServiceHandler(
        measurement::DeliverySession<
            JournalCapacity, MaxChunkSamples, GapCapacity>& session)
        : session_(session) {}

    MeasurementServiceHandler(const MeasurementServiceHandler&) = delete;
    MeasurementServiceHandler& operator=(const MeasurementServiceHandler&) = delete;

    MeasurementHandlerResult handle(
        uint8_t message, const uint8_t* request_payload,
        size_t request_payload_size, uint8_t* response_payload,
        size_t response_capacity) override {
        if (message == kGetStreamStateMessageId) {
            return handle_get_state(
                request_payload_size, response_payload, response_capacity);
        }
        if (message == kPullSamplesMessageId) {
            return handle_pull(
                request_payload, request_payload_size,
                response_payload, response_capacity);
        }
        if (message == kReleaseSamplesMessageId) {
            return handle_release(
                request_payload, request_payload_size,
                response_payload, response_capacity);
        }
        return error(ResponseStatus::UnsupportedMessage, response_payload, response_capacity);
    }

    measurement::CompletionStatus complete(uint64_t token) override {
        return session_.complete(token);
    }

    measurement::CompletionStatus abort(uint64_t token) override {
        return session_.abort(token);
    }

private:
    static MeasurementHandlerResult error(
        ResponseStatus status, uint8_t* output, size_t capacity) {
        MeasurementHandlerResult result{};
        if (output != nullptr && capacity > 0) {
            output[0] = static_cast<uint8_t>(status);
            result.payload_size = 1;
        }
        return result;
    }

    StreamState wire_state(
        const measurement::MeasurementJournalState& state) const {
        return {
            session_.boot_epoch(),
            state.retained_from,
            state.produced,
            state.released,
            static_cast<uint32_t>(state.capacity_samples),
            state.discarded_samples,
        };
    }

    MeasurementHandlerResult handle_get_state(
        size_t request_payload_size, uint8_t* output, size_t capacity) const {
        if (request_payload_size != 0) {
            return error(ResponseStatus::MalformedRequest, output, capacity);
        }
        const StreamState state = wire_state(session_.state());
        if (encode_stream_state_success(state, output, capacity) !=
            MeasurementCodecStatus::Ok) {
            return error(ResponseStatus::InternalError, output, capacity);
        }
        MeasurementHandlerResult result{};
        result.payload_size = kStreamStateSuccessBytes;
        return result;
    }

    MeasurementHandlerResult handle_pull(
        const uint8_t* payload, size_t payload_size,
        uint8_t* output, size_t capacity) {
        PullSamplesRequest request{};
        const MeasurementCodecStatus decoded =
            decode_pull_request(payload, payload_size, request);
        if (decoded != MeasurementCodecStatus::Ok &&
            decoded != MeasurementCodecStatus::InvalidRelease) {
            return error(ResponseStatus::MalformedRequest, output, capacity);
        }

        const measurement::PullPlan plan = session_.prepare_pull({
            request.boot_epoch,
            request.from,
            request.release_before,
            request.max_samples,
        });
        if (plan.decision == measurement::PullDecision::SampleChunk) {
            return encode_chunk(plan, output, capacity);
        }
        if (plan.decision == measurement::PullDecision::Gap) {
            return encode_gap(plan, output, capacity);
        }
        if (plan.decision == measurement::PullDecision::ContinuityBreak) {
            return encode_continuity_break(plan, output, capacity);
        }
        if (plan.decision == measurement::PullDecision::CursorAhead) {
            return error(ResponseStatus::InvalidState, output, capacity);
        }
        if (plan.decision == measurement::PullDecision::InvalidRequest) {
            return error(ResponseStatus::MalformedRequest, output, capacity);
        }
        if (plan.decision == measurement::PullDecision::Busy) {
            return error(ResponseStatus::Busy, output, capacity);
        }
        if (plan.decision == measurement::PullDecision::InternalError) {
            return error(ResponseStatus::InternalError, output, capacity);
        }

        PullResponse response{};
        if (plan.decision == measurement::PullDecision::StreamChanged) {
            response.result = PullResultCode::StreamChanged;
            response.actual_boot_epoch = plan.actual_boot_epoch;
        } else {
            response.state = wire_state(plan.state);
            response.result = plan.decision == measurement::PullDecision::NoData
                ? PullResultCode::NoData
                : plan.decision == measurement::PullDecision::RangeUnavailable
                    ? PullResultCode::RangeUnavailable
                    : PullResultCode::InvalidRelease;
        }

        MeasurementHandlerResult result{};
        if (encode_pull_response(
                response, output, capacity, result.payload_size) !=
            MeasurementCodecStatus::Ok) {
            return error(ResponseStatus::InternalError, output, capacity);
        }
        return result;
    }

    static bool read_sample(
        const void* context, size_t index, RawSample& output) {
        const measurement::MeasurementReadView* view =
            static_cast<const measurement::MeasurementReadView*>(context);
        if (view == nullptr || index >= view->sample_count()) return false;
        const measurement::SampleRecord& source = measurement::sample_at(*view, index);
        output = {source.x, source.y, source.z};
        return true;
    }

    MeasurementHandlerResult encode_gap(
        const measurement::PullPlan& plan,
        uint8_t* output, size_t capacity) {
        const measurement::PreparedGap& prepared = plan.gap;
        PullResponse response{};
        response.result = PullResultCode::Gap;
        response.gap = {
            session_.boot_epoch(),
            prepared.retained_from,
            prepared.produced,
            prepared.gap.first_missing,
            prepared.gap.missing_count,
            prepared.gap.reason == measurement::GapReason::SourceLoss
                ? GapReasonCode::SourceLoss
                : prepared.gap.reason == measurement::GapReason::IngressOverflow
                    ? GapReasonCode::IngressOverflow
                    : GapReasonCode::UnknownDiscontinuity,
        };

        MeasurementHandlerResult result{};
        if (encode_pull_response(
                response, output, capacity, result.payload_size) !=
            MeasurementCodecStatus::Ok) {
            session_.abort(prepared.token);
            return error(ResponseStatus::InternalError, output, capacity);
        }
        result.completion_required = true;
        result.delivery_token = prepared.token;
        return result;
    }

    MeasurementHandlerResult encode_continuity_break(
        const measurement::PullPlan& plan,
        uint8_t* output, size_t capacity) {
        const measurement::PreparedContinuityBreak& prepared =
            plan.continuity_break;
        PullResponse response{};
        response.result = PullResultCode::ContinuityBreak;
        response.continuity_break = {
            session_.boot_epoch(),
            prepared.retained_from,
            prepared.produced,
            prepared.value.at_sequence,
            prepared.value.reason == measurement::ContinuityBreakReason::SensorOverflow
                ? ContinuityBreakReasonCode::SensorOverflow
                : prepared.value.reason ==
                          measurement::ContinuityBreakReason::SensorReadUncertain
                    ? ContinuityBreakReasonCode::SensorReadUncertain
                    : prepared.value.reason ==
                              measurement::ContinuityBreakReason::InvalidTiming
                        ? ContinuityBreakReasonCode::InvalidTiming
                        : prepared.value.reason ==
                                  measurement::ContinuityBreakReason::ProcessingReset
                            ? ContinuityBreakReasonCode::ProcessingReset
                            : ContinuityBreakReasonCode::Unknown,
        };

        MeasurementHandlerResult result{};
        if (encode_pull_response(
                response, output, capacity, result.payload_size) !=
            MeasurementCodecStatus::Ok) {
            session_.abort(prepared.token);
            return error(ResponseStatus::InternalError, output, capacity);
        }
        result.completion_required = true;
        result.delivery_token = prepared.token;
        return result;
    }

    MeasurementHandlerResult encode_chunk(
        const measurement::PullPlan& plan, uint8_t* output, size_t capacity) {
        const measurement::PreparedChunk& prepared = plan.chunk;
        const SampleChunkView view{
            session_.boot_epoch(),
            prepared.samples.retained_from,
            prepared.samples.produced,
            prepared.first_sequence,
            static_cast<uint16_t>(prepared.samples.sample_count()),
            prepared.config_revision,
            {
                prepared.timing_segment_id,
                prepared.timing.first_device_time_us,
                prepared.timing.last_device_time_us,
                prepared.timing.max_fit_residual_us,
                prepared.timing.quality_flags,
            },
            &prepared.samples,
            read_sample,
        };

        MeasurementHandlerResult result{};
        if (encode_sample_chunk_view(
                view, output, capacity, result.payload_size) !=
            MeasurementCodecStatus::Ok) {
            session_.abort(prepared.token);
            return error(ResponseStatus::InternalError, output, capacity);
        }
        result.completion_required = true;
        result.delivery_token = prepared.token;
        return result;
    }

    MeasurementHandlerResult handle_release(
        const uint8_t* payload, size_t payload_size,
        uint8_t* output, size_t capacity) {
        ReleaseSamplesRequest request{};
        if (decode_release_request(payload, payload_size, request) !=
            MeasurementCodecStatus::Ok) {
            return error(ResponseStatus::MalformedRequest, output, capacity);
        }
        const measurement::ReleaseResult released = session_.release({
            request.boot_epoch,
            request.release_before,
        });
        if (released.decision == measurement::ReleaseDecision::Busy) {
            return error(ResponseStatus::Busy, output, capacity);
        }
        if (released.decision == measurement::ReleaseDecision::InternalError) {
            return error(ResponseStatus::InternalError, output, capacity);
        }
        if (released.decision != measurement::ReleaseDecision::Applied) {
            return error(ResponseStatus::InvalidState, output, capacity);
        }

        const StreamState state = wire_state(released.state);
        if (encode_stream_state_success(state, output, capacity) !=
            MeasurementCodecStatus::Ok) {
            return error(ResponseStatus::InternalError, output, capacity);
        }
        MeasurementHandlerResult result{};
        result.payload_size = kStreamStateSuccessBytes;
        return result;
    }

    measurement::DeliverySession<
        JournalCapacity, MaxChunkSamples, GapCapacity>& session_;
};

}  // namespace ProtocolV3
