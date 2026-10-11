#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstring>

#include "protocol_v3/dispatcher.h"
#include "protocol_v3/lifecycle.h"
#include "protocol_v3/lifecycle_handler.h"
#include "protocol_v3/transport_endpoint.h"
#include "runtime/core1_measurement_runtime.h"
#include "runtime/core1_protocol_runtime.h"
#include "runtime/operation_mailbox.h"

namespace {

constexpr uint64_t kBootEpoch = 0x1234;

using Mailbox = runtime::OperationMailbox<
    runtime::SensorRuntimeCommand,
    runtime::SensorRuntimeResult,
    4,
    4>;
using SystemMailbox = runtime::OperationMailbox<
    runtime::SystemOperationCommand,
    runtime::SensorRuntimeResult,
    4,
    4>;

class FakePort {
public:
    void poll() {}

    bool read_byte(uint8_t& output) {
        if (rx_offset_ == rx_size_) return false;
        output = rx_[rx_offset_++];
        return true;
    }

    bool start_tx(const uint8_t* data, size_t size) {
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
    }

    void push_rx(const uint8_t* data, size_t size) {
        assert(rx_size_ + size <= sizeof(rx_));
        std::memcpy(rx_ + rx_size_, data, size);
        rx_size_ += size;
    }

    void complete_tx() {
        assert(busy_);
        busy_ = false;
        completed_ = true;
    }

    const uint8_t* tx_data() const { return tx_; }
    size_t tx_size() const { return tx_size_; }

private:
    uint8_t rx_[ProtocolV3::kMaxWireFrameBytes * 2]{};
    size_t rx_size_ = 0;
    size_t rx_offset_ = 0;
    uint8_t tx_[ProtocolV3::kMaxWireFrameBytes]{};
    size_t tx_size_ = 0;
    bool busy_ = false;
    bool completed_ = false;
};

class FakeObserver {
public:
    void update() { ++updates; }
    uint32_t updates = 0;
};

ProtocolV3::NodeIdentity identity() {
    ProtocolV3::NodeIdentity value{};
    value.boot_epoch = kBootEpoch;
    value.node_address = 7;
    value.board_revision = 2;
    value.firmware = {0, 5, 0};
    return value;
}

ProtocolV3::NodeCapabilities capabilities() {
    ProtocolV3::NodeCapabilities value{};
    value.service_flags =
        ProtocolV3::service_capability_bit(ProtocolV3::ServiceId::Measurement) |
        ProtocolV3::service_capability_bit(ProtocolV3::ServiceId::Lifecycle);
    value.feature_flags = ProtocolV3::kFeatureSampleJournal;
    value.max_payload_bytes = ProtocolV3::kMaxPayloadBytes;
    value.journal_capacity_samples = 16;
    value.max_sample_chunk_samples = 4;
    value.output_rate_mask = ProtocolV3::kImplementedOutputRateMask;
    return value;
}

size_t lifecycle_request(uint32_t transaction_id,
                         const runtime::SensorRuntimeCommand& command,
                         uint8_t* wire) {
    ProtocolV3::Frame request{};
    request.role = ProtocolV3::Role::Request;
    request.destination = 7;
    request.source = ProtocolV3::kHostAddress;
    request.service = ProtocolV3::kLifecycleServiceId;
    request.transaction_id = transaction_id;
    assert(ProtocolV3::encode_lifecycle_request(
               command,
               request.payload,
               sizeof(request.payload),
               request.payload_size,
               request.message) == ProtocolV3::LifecycleCodecStatus::Ok);
    size_t wire_size = 0;
    assert(ProtocolV3::encode(
               request, wire, ProtocolV3::kMaxWireFrameBytes, wire_size) ==
           ProtocolV3::Status::Ok);
    return wire_size;
}

runtime::SensorRuntimeResult decode_result(
    const uint8_t* wire, size_t wire_size, uint32_t transaction_id) {
    ProtocolV3::StreamDecoder decoder;
    ProtocolV3::Frame frame{};
    ProtocolV3::Status status = ProtocolV3::Status::NoFrame;
    for (size_t index = 0; index < wire_size; ++index) {
        status = decoder.push(wire[index], frame);
    }
    assert(status == ProtocolV3::Status::Ok);
    assert(frame.transaction_id == transaction_id);
    runtime::SensorRuntimeResult result{};
    ProtocolV3::ResponseStatus remote = ProtocolV3::ResponseStatus::InternalError;
    assert(ProtocolV3::decode_lifecycle_response(
               frame.payload, frame.payload_size, result, remote) ==
           ProtocolV3::LifecycleCodecStatus::Ok);
    return result;
}

void test_core1_scheduler_routes_and_reconciles_lifecycle_transaction() {
    runtime::SpscQueue<measurement::IngressEvent, 8> ingress;
    runtime::RuntimeFaultLatch fault_latch;
    runtime::Core1MeasurementRuntime<8, 16, 4, 4> measurement(
        ingress, kBootEpoch, fault_latch);
    Mailbox mailbox;
    SystemMailbox system_mailbox;
    runtime::DeferredSystemAction system_action;
    ProtocolV3::LifecycleServiceHandler<Mailbox, SystemMailbox, 4> lifecycle(
        mailbox, system_mailbox, system_action);
    ProtocolV3::ApplicationDispatcher dispatcher(
        identity(), capabilities(), &measurement.handler(), &lifecycle);
    FakePort port;
    ProtocolV3::TransportEndpoint<FakePort, 2> endpoint(
        port, dispatcher, {ProtocolV3::kMaxWireFrameBytes, 1000});
    FakeObserver observer;
    runtime::Core1ProtocolRuntime core1(
        measurement, lifecycle, endpoint, observer,
        runtime::Core1ProtocolPolicy{4});

    const runtime::SensorRuntimeCommand start{
        91, runtime::SensorRuntimeOperation::Start};
    uint8_t wire[ProtocolV3::kMaxWireFrameBytes]{};
    size_t wire_size = lifecycle_request(1, start, wire);
    port.push_rx(wire, wire_size);
    assert(core1.poll(100) == runtime::Core1ProtocolPollStatus::Ok);
    runtime::SensorRuntimeResult result = decode_result(
        port.tx_data(), port.tx_size(), 1);
    assert(result.code == runtime::SensorRuntimeResultCode::InProgress);
    assert(observer.updates == 1);

    runtime::SensorRuntimeCommand command{};
    assert(mailbox.try_receive_command(command));
    assert(command.operation_id == 91);
    assert(mailbox.try_send_result({
        91,
        runtime::SensorRuntimeResultCode::Ok,
        runtime::SensorRuntimeState::Recording,
        runtime::ObservabilityProfile::Production,
        SensorStatus::Ok,
    }));
    port.complete_tx();
    assert(core1.poll(101) == runtime::Core1ProtocolPollStatus::Ok);
    assert(core1.state().lifecycle_results_consumed == 1);

    wire_size = lifecycle_request(2, start, wire);
    port.push_rx(wire, wire_size);
    assert(core1.poll(102) == runtime::Core1ProtocolPollStatus::Ok);
    result = decode_result(port.tx_data(), port.tx_size(), 2);
    assert(result.code == runtime::SensorRuntimeResultCode::Ok);
    assert(result.state == runtime::SensorRuntimeState::Recording);
    assert(mailbox.commands_waiting_approx() == 0);
}

}  // namespace

int main() {
    test_core1_scheduler_routes_and_reconciles_lifecycle_transaction();
    return 0;
}
