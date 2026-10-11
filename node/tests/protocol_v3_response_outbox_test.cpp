#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstring>

#include "measurement/delivery_session.h"
#include "protocol_v3/dispatcher.h"
#include "protocol_v3/measurement.h"
#include "protocol_v3/measurement_handler.h"
#include "protocol_v3/response_outbox.h"

namespace {
using namespace ProtocolV3;

constexpr uint64_t kEpoch = 17;

NodeIdentity identity() {
    NodeIdentity value{};
    const uint8_t id[kHardwareIdBytes] = {1, 2, 3, 4, 5, 6, 7, 8};
    std::memcpy(value.hardware_id, id, sizeof(id));
    value.boot_epoch = kEpoch;
    value.node_address = 7;
    value.board_revision = 2;
    value.firmware = {0, 5, 0};
    return value;
}

NodeCapabilities capabilities() {
    NodeCapabilities value{};
    value.service_flags = service_capability_bit(ServiceId::Identity) |
                          service_capability_bit(ServiceId::Measurement);
    value.feature_flags = kFeatureSampleJournal;
    value.max_payload_bytes = kMaxPayloadBytes;
    value.journal_capacity_samples = 8;
    value.output_rate_mask = kImplementedOutputRateMask;
    value.max_sample_chunk_samples = 4;
    return value;
}

measurement::SampleRecord sample(int32_t value) {
    return {
        1000u + static_cast<uint64_t>(value) * 4000u,
        value,
        -value,
        value + 1,
        1,
        1,
        measurement::kTimingValidFlag,
        0,
    };
}

Frame pull_request(uint32_t transaction) {
    Frame frame{};
    frame.role = Role::Request;
    frame.destination = 7;
    frame.source = kHostAddress;
    frame.service = kMeasurementServiceId;
    frame.message = kPullSamplesMessageId;
    frame.transaction_id = transaction;
    assert(encode_pull_request(
               {kEpoch, 0, 0, 2}, frame.payload, sizeof(frame.payload)) ==
           MeasurementCodecStatus::Ok);
    frame.payload_size = kPullRequestBytes;
    return frame;
}

void test_queue_owns_dispatch_and_completion() {
    measurement::MeasurementJournal<8, 8> journal;
    journal.append_sample(0, sample(0));
    journal.append_sample(1, sample(1));
    measurement::DeliverySession<8, 4> session(kEpoch, journal);
    MeasurementServiceHandler<8, 4> handler(session);
    ApplicationDispatcher dispatcher(identity(), capabilities(), &handler);
    ResponseOutbox<1> outbox;

    assert(outbox.accept_request(dispatcher, pull_request(1)) ==
           RequestAcceptanceStatus::Queued);
    assert(outbox.size() == 1);
    assert(journal.state().delivered == 2);
    assert(outbox.front() != nullptr && outbox.front()->transaction_id == 1);

    assert(outbox.accept_request(dispatcher, pull_request(2)) ==
           RequestAcceptanceStatus::QueueFull);
    assert(journal.state().delivered == 2);
    assert(outbox.pop());
    assert(outbox.empty());

    assert(outbox.accept_request(dispatcher, pull_request(2)) ==
           RequestAcceptanceStatus::Queued);
    assert(outbox.front() != nullptr && outbox.front()->transaction_id == 2);
}

void test_foreign_request_is_ignored_without_queueing() {
    measurement::MeasurementJournal<8, 8> journal;
    measurement::DeliverySession<8, 4> session(kEpoch, journal);
    MeasurementServiceHandler<8, 4> handler(session);
    ApplicationDispatcher dispatcher(identity(), capabilities(), &handler);
    ResponseOutbox<1> outbox;
    Frame request = pull_request(1);
    request.destination = 8;

    assert(outbox.accept_request(dispatcher, request) ==
           RequestAcceptanceStatus::Ignored);
    assert(outbox.empty());
}
}  // namespace

int main() {
    test_queue_owns_dispatch_and_completion();
    test_foreign_request_is_ignored_without_queueing();
    return 0;
}
