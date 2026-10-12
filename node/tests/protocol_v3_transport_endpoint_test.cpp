#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstring>

#include "measurement/delivery_session.h"
#include "protocol_v3/dispatcher.h"
#include "protocol_v3/framing.h"
#include "protocol_v3/lifecycle.h"
#include "protocol_v3/lifecycle_handler.h"
#include "protocol_v3/measurement.h"
#include "protocol_v3/measurement_handler.h"
#include "protocol_v3/time_service.h"
#include "protocol_v3/transport_endpoint.h"

namespace {
using namespace ProtocolV3;

constexpr uint64_t kEpoch = 17;

class FakePort {
public:
    void poll() {}

    bool read_byte(uint8_t& output) {
        if (rx_offset_ == rx_size_) return false;
        output = rx_[rx_offset_++];
        return true;
    }

    bool start_tx(const uint8_t* data, size_t size) {
        if (fail_next_start_) {
            fail_next_start_ = false;
            return false;
        }
        if (busy_ || data == nullptr || size > sizeof(tx_)) return false;
        std::memcpy(tx_, data, size);
        tx_size_ = size;
        busy_ = true;
        completed_ = false;
        return true;
    }

    bool tx_busy() const { return busy_; }
    bool tx_completed() const { return completed_; }
    void clear_tx_completed() { completed_ = false; }

    void abort_tx() {
        busy_ = false;
        completed_ = false;
        ++abort_count;
    }

    void push_rx(const uint8_t* data, size_t size) {
        assert(data != nullptr);
        assert(rx_size_ + size <= sizeof(rx_));
        std::memcpy(rx_ + rx_size_, data, size);
        rx_size_ += size;
    }

    void complete_tx() {
        assert(busy_);
        busy_ = false;
        completed_ = true;
    }

    void fail_next_start() { fail_next_start_ = true; }
    size_t unread_rx() const { return rx_size_ - rx_offset_; }
    const uint8_t* tx_data() const { return tx_; }
    size_t tx_size() const { return tx_size_; }

    uint32_t abort_count = 0;

private:
    uint8_t rx_[4096]{};
    size_t rx_size_ = 0;
    size_t rx_offset_ = 0;
    uint8_t tx_[kMaxWireFrameBytes]{};
    size_t tx_size_ = 0;
    bool busy_ = false;
    bool completed_ = false;
    bool fail_next_start_ = false;
};

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

size_t encode_pull(uint32_t transaction, uint8_t* output) {
    Frame request{};
    request.role = Role::Request;
    request.destination = 7;
    request.source = kHostAddress;
    request.service = kMeasurementServiceId;
    request.message = kPullSamplesMessageId;
    request.transaction_id = transaction;
    assert(encode_pull_request(
               {kEpoch, 0, 0, 2}, request.payload, sizeof(request.payload)) ==
           MeasurementCodecStatus::Ok);
    request.payload_size = kPullRequestBytes;
    size_t size = 0;
    assert(encode(request, output, kMaxWireFrameBytes, size) == Status::Ok);
    return size;
}

Frame decode_frame(const uint8_t* wire, size_t size) {
    StreamDecoder decoder;
    Frame result{};
    Status status = Status::NoFrame;
    for (size_t index = 0; index < size; ++index) {
        status = decoder.push(wire[index], result);
    }
    assert(status == Status::Ok);
    return result;
}

struct Fixture {
    measurement::MeasurementJournal<8, 8> journal{};
    measurement::DeliverySession<8, 4> session{kEpoch, journal};
    MeasurementServiceHandler<8, 4> handler{session};
    ApplicationDispatcher dispatcher{identity(), capabilities(), &handler};
    FakePort port{};

    Fixture() {
        journal.append_sample(0, sample(0));
        journal.append_sample(1, sample(1));
    }
};

class PostTransmitLifecycleHandler final : public ILifecycleServiceHandler {
public:
    LifecycleHandlerResult handle(
        uint8_t,
        const uint8_t*,
        size_t,
        uint8_t* response,
        size_t capacity) override {
        if (response == nullptr || capacity == 0) return {};
        response[0] = static_cast<uint8_t>(ResponseStatus::Ok);
        return {1, true, token};
    }

    bool settle_transmission(uint64_t received_token,
                             bool transmitted) override {
        ++settlement_count;
        last_token = received_token;
        last_transmitted = transmitted;
        return received_token == token;
    }

    uint64_t token = 0x11223344u;
    uint64_t last_token = 0;
    uint32_t settlement_count = 0;
    bool last_transmitted = false;
};

Frame lifecycle_request(uint32_t transaction) {
    Frame request{};
    request.role = Role::Request;
    request.destination = 7;
    request.source = kHostAddress;
    request.service = kLifecycleServiceId;
    request.message = kGetRuntimeStateMessageId;
    request.transaction_id = transaction;
    return request;
}

size_t encode_frame(const Frame& frame, uint8_t* output) {
    size_t size = 0;
    assert(encode(frame, output, kMaxWireFrameBytes, size) == Status::Ok);
    return size;
}

uint64_t read_u64_le(const uint8_t* input) {
    uint64_t value = 0;
    for (size_t index = 0; index < 8; ++index) {
        value |= static_cast<uint64_t>(input[index]) << (index * 8u);
    }
    return value;
}

void test_fragmented_request_response_and_completion() {
    Fixture fixture;
    TransportEndpoint<FakePort, 1> endpoint(
        fixture.port, fixture.dispatcher, {5, 1000});
    uint8_t request[kMaxWireFrameBytes]{};
    const size_t request_size = encode_pull(42, request);
    fixture.port.push_rx(request, request_size);

    endpoint.update(0);
    assert(endpoint.stats().rx_bytes == 5);
    assert(!endpoint.tx_active());
    for (uint64_t now = 1; !endpoint.tx_active(); ++now) endpoint.update(now);

    assert(endpoint.stats().decoded_frames == 1);
    assert(endpoint.stats().tx_started == 1);
    assert(fixture.journal.state().delivered == 2);
    const Frame response = decode_frame(fixture.port.tx_data(), fixture.port.tx_size());
    assert(response.role == Role::Response && response.transaction_id == 42);
    PullResponse pull{};
    ResponseStatus remote = ResponseStatus::InternalError;
    assert(decode_pull_response(
               response.payload, response.payload_size, pull, remote) ==
           MeasurementCodecStatus::Ok);
    assert(pull.result == PullResultCode::SampleChunk && pull.chunk.sample_count == 2);

    fixture.port.complete_tx();
    endpoint.update(100);
    assert(!endpoint.tx_active());
    assert(endpoint.stats().tx_completed == 1);
}

void test_only_one_response_is_accepted_from_batched_requests() {
    Fixture fixture;
    TransportEndpoint<FakePort, 1> endpoint(
        fixture.port, fixture.dispatcher, {2048, 1000});
    uint8_t first[kMaxWireFrameBytes]{};
    uint8_t second[kMaxWireFrameBytes]{};
    const size_t first_size = encode_pull(1, first);
    const size_t second_size = encode_pull(2, second);
    fixture.port.push_rx(first, first_size);
    fixture.port.push_rx(second, second_size);

    endpoint.update(0);
    assert(endpoint.tx_active());
    assert(endpoint.stats().decoded_frames == 2);
    assert(endpoint.stats().response_queue_full == 1);
    const Frame response = decode_frame(fixture.port.tx_data(), fixture.port.tx_size());
    assert(response.transaction_id == 1);
}

void test_rx_waits_while_half_duplex_tx_is_active() {
    Fixture fixture;
    TransportEndpoint<FakePort, 1> endpoint(
        fixture.port, fixture.dispatcher, {2048, 1000});
    uint8_t first[kMaxWireFrameBytes]{};
    const size_t first_size = encode_pull(1, first);
    fixture.port.push_rx(first, first_size);
    endpoint.update(0);
    assert(endpoint.tx_active());

    uint8_t second[kMaxWireFrameBytes]{};
    const size_t second_size = encode_pull(2, second);
    fixture.port.push_rx(second, second_size);
    const size_t unread = fixture.port.unread_rx();
    endpoint.update(1);
    assert(fixture.port.unread_rx() == unread);

    fixture.port.complete_tx();
    endpoint.update(2);
    assert(endpoint.tx_active());
    const Frame response = decode_frame(fixture.port.tx_data(), fixture.port.tx_size());
    assert(response.transaction_id == 2);
}

void test_start_failure_retries_and_timeout_aborts() {
    Fixture fixture;
    TransportEndpoint<FakePort, 1> endpoint(
        fixture.port, fixture.dispatcher, {2048, 50});
    uint8_t request[kMaxWireFrameBytes]{};
    const size_t size = encode_pull(1, request);
    fixture.port.push_rx(request, size);
    fixture.port.fail_next_start();

    endpoint.update(100);
    assert(!endpoint.tx_active());
    assert(endpoint.queued_responses() == 1);
    assert(endpoint.stats().tx_start_failures == 1);
    endpoint.update(101);
    assert(endpoint.tx_active());
    endpoint.update(151);
    assert(!endpoint.tx_active());
    assert(endpoint.stats().tx_timeouts == 1);
    assert(fixture.port.abort_count == 1);
}

void test_bad_frame_does_not_block_following_valid_frame() {
    Fixture fixture;
    TransportEndpoint<FakePort, 1> endpoint(
        fixture.port, fixture.dispatcher, {2048, 1000});
    uint8_t bad[kMaxWireFrameBytes]{};
    uint8_t good[kMaxWireFrameBytes]{};
    const size_t bad_size = encode_pull(1, bad);
    const size_t good_size = encode_pull(2, good);
    bad[bad_size / 2] ^= 0x40u;
    fixture.port.push_rx(bad, bad_size);
    fixture.port.push_rx(good, good_size);

    endpoint.update(0);
    assert(endpoint.stats().framing_errors == 1);
    assert(endpoint.stats().decoded_frames == 1);
    assert(endpoint.tx_active());
    const Frame response = decode_frame(fixture.port.tx_data(), fixture.port.tx_size());
    assert(response.transaction_id == 2);
}

void test_post_transmit_action_waits_for_physical_tx_completion() {
    PostTransmitLifecycleHandler lifecycle;
    NodeCapabilities caps = capabilities();
    caps.service_flags |= service_capability_bit(ServiceId::Lifecycle);
    ApplicationDispatcher dispatcher(identity(), caps, nullptr, &lifecycle);
    FakePort port;
    TransportEndpoint<FakePort, 1> endpoint(port, dispatcher, {2048, 1000});
    uint8_t request[kMaxWireFrameBytes]{};
    const size_t request_size = encode_frame(lifecycle_request(50), request);
    port.push_rx(request, request_size);

    endpoint.update(10);
    assert(endpoint.tx_active());
    assert(lifecycle.settlement_count == 0);
    port.complete_tx();
    endpoint.update(11);
    assert(!endpoint.tx_active());
    assert(lifecycle.settlement_count == 1);
    assert(lifecycle.last_token == lifecycle.token);
    assert(lifecycle.last_transmitted);
}

void test_post_transmit_action_is_cancelled_on_tx_timeout() {
    PostTransmitLifecycleHandler lifecycle;
    NodeCapabilities caps = capabilities();
    caps.service_flags |= service_capability_bit(ServiceId::Lifecycle);
    ApplicationDispatcher dispatcher(identity(), caps, nullptr, &lifecycle);
    FakePort port;
    TransportEndpoint<FakePort, 1> endpoint(port, dispatcher, {2048, 50});
    uint8_t request[kMaxWireFrameBytes]{};
    const size_t request_size = encode_frame(lifecycle_request(51), request);
    port.push_rx(request, request_size);

    endpoint.update(100);
    assert(endpoint.tx_active());
    endpoint.update(150);
    assert(!endpoint.tx_active());
    assert(lifecycle.settlement_count == 1);
    assert(!lifecycle.last_transmitted);
}

void test_response_turnaround_delays_tx_without_blocking_decode() {
    Fixture fixture;
    TransportEndpoint<FakePort, 1> endpoint(
        fixture.port, fixture.dispatcher, {2048, 1000, 100});
    uint8_t request[kMaxWireFrameBytes]{};
    const size_t size = encode_pull(1, request);
    fixture.port.push_rx(request, size);

    endpoint.update(10);
    assert(endpoint.stats().decoded_frames == 1);
    assert(endpoint.queued_responses() == 1);
    assert(!endpoint.tx_active());
    endpoint.update(109);
    assert(!endpoint.tx_active());
    endpoint.update(110);
    assert(endpoint.tx_active());
    assert(endpoint.stats().tx_started == 1);
}

void test_time_response_captures_receive_and_actual_tx_boundaries() {
    NodeCapabilities caps = capabilities();
    caps.service_flags |= service_capability_bit(ServiceId::Time);
    ApplicationDispatcher dispatcher(identity(), caps);
    FakePort port;
    TransportEndpoint<FakePort, 1> endpoint(
        port, dispatcher, {2048, 1000, 100});
    Frame time_request{};
    time_request.role = Role::Request;
    time_request.destination = 7;
    time_request.source = kHostAddress;
    time_request.service = kTimeServiceId;
    time_request.message = kObserveClockMessageId;
    time_request.transaction_id = 90;
    uint8_t request[kMaxWireFrameBytes]{};
    const size_t request_size = encode_frame(time_request, request);
    port.push_rx(request, request_size);

    endpoint.update(10);
    assert(endpoint.queued_responses() == 1);
    assert(!endpoint.tx_active());
    endpoint.update(109);
    assert(!endpoint.tx_active());
    endpoint.update(110);
    assert(endpoint.tx_active());

    const Frame response = decode_frame(port.tx_data(), port.tx_size());
    assert(response.service == kTimeServiceId);
    assert(response.message == kObserveClockMessageId);
    assert(response.payload_size == kObserveClockResponseBytes);
    assert(response.payload[0] == static_cast<uint8_t>(ResponseStatus::Ok));
    assert(read_u64_le(response.payload + 1) == kEpoch);
    assert(read_u64_le(response.payload + 9) == 10);
    assert(read_u64_le(response.payload + 17) == 110);
}
}  // namespace

int main() {
    test_fragmented_request_response_and_completion();
    test_only_one_response_is_accepted_from_batched_requests();
    test_rx_waits_while_half_duplex_tx_is_active();
    test_start_failure_retries_and_timeout_aborts();
    test_bad_frame_does_not_block_following_valid_frame();
    test_post_transmit_action_waits_for_physical_tx_completion();
    test_post_transmit_action_is_cancelled_on_tx_timeout();
    test_response_turnaround_delays_tx_without_blocking_decode();
    test_time_response_captures_receive_and_actual_tx_boundaries();
    return 0;
}
