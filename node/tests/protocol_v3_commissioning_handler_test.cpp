#include <cassert>
#include <cstring>

#include "protocol_v3/commissioning_handler.h"
#include "protocol_v3/dispatcher.h"
#include "runtime/operation_mailbox.h"

namespace {

using Mailbox = runtime::OperationMailbox<
    runtime::CommissioningCommand,
    runtime::CommissioningResult,
    2,
    2>;
using Handler = ProtocolV3::CommissioningServiceHandler<Mailbox, 2>;

ProtocolV3::NodeIdentity unassigned_identity() {
    ProtocolV3::NodeIdentity value{};
    for (size_t i = 0; i < ProtocolV3::kHardwareIdBytes; ++i) {
        value.hardware_id[i] = static_cast<uint8_t>(i + 1);
    }
    value.boot_epoch = 1;
    value.node_address = 0;
    value.board_revision = 2;
    value.firmware = {0, 5, 7};
    return value;
}

ProtocolV3::CommissioningIdentity commissioning_identity() {
    const auto node = unassigned_identity();
    ProtocolV3::CommissioningIdentity value{};
    std::memcpy(value.hardware_id, node.hardware_id,
                ProtocolV3::kHardwareIdBytes);
    value.board_revision = node.board_revision;
    value.firmware = node.firmware;
    return value;
}

void write_u64(uint64_t value, uint8_t* output) {
    for (size_t i = 0; i < 8; ++i) {
        output[i] = static_cast<uint8_t>(value >> (8 * i));
    }
}

void unassigned_node_responds_only_in_its_host_polled_slot() {
    Mailbox mailbox;
    runtime::DeferredSystemAction action;
    auto commissioning_id = commissioning_identity();
    Handler handler(mailbox, action, commissioning_id);
    ProtocolV3::NodeCapabilities caps{};
    ProtocolV3::ApplicationDispatcher dispatcher(
        unassigned_identity(), caps, nullptr, nullptr, nullptr, nullptr,
        &handler);

    ProtocolV3::Frame request{};
    request.role = ProtocolV3::Role::Request;
    request.source = ProtocolV3::kHostAddress;
    request.destination = ProtocolV3::kBroadcastAddress;
    request.service = ProtocolV3::kCommissioningServiceId;
    request.message = ProtocolV3::kProbeSlotMessageId;
    request.transaction_id = 11;
    const uint64_t nonce = 0x0102030405060708ull;
    write_u64(nonce, request.payload);
    request.payload[8] = 16;
    request.payload[10] = static_cast<uint8_t>(
        ProtocolV3::commissioning_slot(
            commissioning_id.hardware_id, nonce, 16));
    request.payload_size = ProtocolV3::kProbeSlotRequestBytes;

    ProtocolV3::Frame response{};
    assert(dispatcher.dispatch(request, response));
    assert(response.source == ProtocolV3::kCommissioningAddress);
    assert(response.destination == ProtocolV3::kHostAddress);
    assert(response.payload_size == ProtocolV3::kProbeSlotResponseBytes);

    request.payload[10] = static_cast<uint8_t>(
        (request.payload[10] + 1) % 16);
    assert(!dispatcher.dispatch(request, response));
}

void assignment_restarts_only_after_success_response_is_transmitted() {
    Mailbox mailbox;
    runtime::DeferredSystemAction action;
    auto commissioning_id = commissioning_identity();
    Handler handler(mailbox, action, commissioning_id);
    ProtocolV3::NodeCapabilities caps{};
    ProtocolV3::ApplicationDispatcher dispatcher(
        unassigned_identity(), caps, nullptr, nullptr, nullptr, nullptr,
        &handler);

    ProtocolV3::Frame request{};
    request.role = ProtocolV3::Role::Request;
    request.source = ProtocolV3::kHostAddress;
    request.destination = ProtocolV3::kBroadcastAddress;
    request.service = ProtocolV3::kCommissioningServiceId;
    request.message = ProtocolV3::kAssignAddressMessageId;
    request.transaction_id = 12;
    request.payload[0] = 7;
    request.payload[4] = 9;
    std::memcpy(request.payload + 12, commissioning_id.hardware_id,
                ProtocolV3::kHardwareIdBytes);
    request.payload[20] = 42;
    request.payload_size = ProtocolV3::kAssignAddressRequestBytes;

    ProtocolV3::Frame response{};
    auto dispatched = dispatcher.dispatch(request, response);
    assert(dispatched);
    assert(response.payload[0] == static_cast<uint8_t>(
        ProtocolV3::ResponseStatus::Busy));
    runtime::CommissioningCommand command{};
    assert(mailbox.try_receive_command(command));
    assert(command.operation_id == 7 && command.node_address == 42);
    assert(mailbox.try_send_result({
        7, runtime::CommissioningResultCode::Assigned, 42}));

    dispatched = dispatcher.dispatch(request, response);
    assert(dispatched.transmission.required);
    assert(response.payload_size == ProtocolV3::kAssignAddressResponseBytes);
    assert(action.peek() == runtime::DeferredSystemActionKind::None);
    assert(dispatcher.settle_transmission(
               dispatched.transmission, false) ==
           ProtocolV3::TransmissionSettlementStatus::Ok);
    assert(action.peek() == runtime::DeferredSystemActionKind::None);
    assert(dispatcher.settle_transmission(
               dispatched.transmission, true) ==
           ProtocolV3::TransmissionSettlementStatus::Ok);
    assert(action.peek() == runtime::DeferredSystemActionKind::RestartNode);
}

}  // namespace

int main() {
    unassigned_node_responds_only_in_its_host_polled_slot();
    assignment_restarts_only_after_success_response_is_transmitted();
    return 0;
}
