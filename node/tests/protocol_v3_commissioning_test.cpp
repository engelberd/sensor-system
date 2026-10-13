#include <cassert>
#include <cstdint>

#include "protocol_v3/commissioning.h"

namespace {

void slot_selection_is_deterministic_and_bounded() {
    const uint8_t hardware[ProtocolV3::kHardwareIdBytes] =
        {1, 2, 3, 4, 5, 6, 7, 8};
    const uint16_t first = ProtocolV3::commissioning_slot(
        hardware, 0x0102030405060708ull, 16);
    const uint16_t second = ProtocolV3::commissioning_slot(
        hardware, 0x0102030405060708ull, 16);
    assert(first == second);
    assert(first < 16);
    assert(ProtocolV3::commissioning_slot(hardware, 1, 3) == UINT16_MAX);
}

void probe_and_assignment_fail_closed() {
    const uint8_t probe[ProtocolV3::kProbeSlotRequestBytes] = {
        8, 7, 6, 5, 4, 3, 2, 1, 16, 0, 3, 0,
    };
    ProtocolV3::ProbeSlotRequest request{};
    assert(ProtocolV3::decode_probe_slot_request(
               probe, sizeof(probe), request) ==
           ProtocolV3::CommissioningCodecStatus::Ok);
    assert(request.slot_count == 16);
    assert(request.slot_index == 3);

    uint8_t assignment[ProtocolV3::kAssignAddressRequestBytes]{};
    assignment[0] = 1;
    assignment[4] = 1;
    assignment[20] = 254;
    ProtocolV3::AssignAddressRequest decoded{};
    assert(ProtocolV3::decode_assign_address_request(
               assignment, sizeof(assignment), decoded) ==
           ProtocolV3::CommissioningCodecStatus::InvalidNodeAddress);
    assignment[20] = 42;
    assert(ProtocolV3::decode_assign_address_request(
               assignment, sizeof(assignment), decoded) ==
           ProtocolV3::CommissioningCodecStatus::Ok);
}

void response_uses_the_same_layout_as_the_host_codec() {
    ProtocolV3::CommissioningIdentity identity{};
    identity.round_nonce = 0x0102030405060708ull;
    for (size_t i = 0; i < ProtocolV3::kHardwareIdBytes; ++i) {
        identity.hardware_id[i] = static_cast<uint8_t>(i + 1);
    }
    identity.board_revision = 2;
    identity.firmware = {0, 5, 7};
    uint8_t output[ProtocolV3::kProbeSlotResponseBytes]{};
    assert(ProtocolV3::encode_probe_slot_success(
               identity, output, sizeof(output)) ==
           ProtocolV3::CommissioningCodecStatus::Ok);
    assert(output[0] == 0);
    assert(output[1] == 8 && output[8] == 1);
    assert(output[9] == 1 && output[16] == 8);
    assert(output[17] == 2);
}

}  // namespace

int main() {
    slot_selection_is_deterministic_and_bounded();
    probe_and_assignment_fail_closed();
    response_uses_the_same_layout_as_the_host_codec();
    return 0;
}
