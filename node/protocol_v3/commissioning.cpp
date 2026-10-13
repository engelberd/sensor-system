#include "protocol_v3/commissioning.h"

#include <cstring>

#include "protocol_v3/framing.h"

namespace ProtocolV3 {
namespace {

uint16_t read_u16(const uint8_t* input) {
    return static_cast<uint16_t>(input[0]) |
           static_cast<uint16_t>(static_cast<uint16_t>(input[1]) << 8u);
}

uint32_t read_u32(const uint8_t* input) {
    uint32_t value = 0;
    for (size_t i = 0; i < 4; ++i) {
        value |= static_cast<uint32_t>(input[i]) << (8u * i);
    }
    return value;
}

uint64_t read_u64(const uint8_t* input) {
    uint64_t value = 0;
    for (size_t i = 0; i < 8; ++i) {
        value |= static_cast<uint64_t>(input[i]) << (8u * i);
    }
    return value;
}

void write_u32(uint32_t value, uint8_t* output) {
    for (size_t i = 0; i < 4; ++i) {
        output[i] = static_cast<uint8_t>(value >> (8u * i));
    }
}

void write_u64(uint64_t value, uint8_t* output) {
    for (size_t i = 0; i < 8; ++i) {
        output[i] = static_cast<uint8_t>(value >> (8u * i));
    }
}

bool valid_slot_count(uint16_t count) {
    return count != 0 && count <= kMaxCommissioningSlots &&
           (count & static_cast<uint16_t>(count - 1u)) == 0;
}

}  // namespace

bool is_assignable_node_address(uint8_t address) {
    return address >= 1u && address <= 253u;
}

CommissioningCodecStatus decode_probe_slot_request(
    const uint8_t* payload,
    size_t payload_size,
    ProbeSlotRequest& output) {
    if (payload == nullptr || payload_size != kProbeSlotRequestBytes) {
        return CommissioningCodecStatus::InvalidPayloadLength;
    }
    output.round_nonce = read_u64(payload);
    output.slot_count = read_u16(payload + 8);
    output.slot_index = read_u16(payload + 10);
    if (output.round_nonce == 0) {
        return CommissioningCodecStatus::ZeroRoundNonce;
    }
    if (!valid_slot_count(output.slot_count)) {
        return CommissioningCodecStatus::InvalidSlotCount;
    }
    if (output.slot_index >= output.slot_count) {
        return CommissioningCodecStatus::InvalidSlotIndex;
    }
    return CommissioningCodecStatus::Ok;
}

CommissioningCodecStatus decode_assign_address_request(
    const uint8_t* payload,
    size_t payload_size,
    AssignAddressRequest& output) {
    if (payload == nullptr || payload_size != kAssignAddressRequestBytes) {
        return CommissioningCodecStatus::InvalidPayloadLength;
    }
    output.operation_id = read_u32(payload);
    output.round_nonce = read_u64(payload + 4);
    std::memcpy(output.hardware_id, payload + 12, kHardwareIdBytes);
    output.node_address = payload[20];
    if (output.operation_id == 0) {
        return CommissioningCodecStatus::ZeroOperationId;
    }
    if (output.round_nonce == 0) {
        return CommissioningCodecStatus::ZeroRoundNonce;
    }
    if (!is_assignable_node_address(output.node_address)) {
        return CommissioningCodecStatus::InvalidNodeAddress;
    }
    return CommissioningCodecStatus::Ok;
}

CommissioningCodecStatus encode_probe_slot_success(
    const CommissioningIdentity& value,
    uint8_t* output,
    size_t output_capacity) {
    if (value.round_nonce == 0) {
        return CommissioningCodecStatus::ZeroRoundNonce;
    }
    if (value.board_revision == 0) {
        return CommissioningCodecStatus::InvalidBoardRevision;
    }
    if (output == nullptr || output_capacity < kProbeSlotResponseBytes) {
        return CommissioningCodecStatus::OutputTooSmall;
    }
    output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
    write_u64(value.round_nonce, output + 1);
    std::memcpy(output + 9, value.hardware_id, kHardwareIdBytes);
    output[17] = value.board_revision;
    output[18] = value.firmware.major;
    output[19] = value.firmware.minor;
    output[20] = value.firmware.patch;
    output[21] = value.framing_version;
    output[22] = value.application_version;
    return CommissioningCodecStatus::Ok;
}

CommissioningCodecStatus encode_assign_address_success(
    const AssignAddressResponse& value,
    uint8_t* output,
    size_t output_capacity) {
    if (value.operation_id == 0) {
        return CommissioningCodecStatus::ZeroOperationId;
    }
    if (!is_assignable_node_address(value.node_address)) {
        return CommissioningCodecStatus::InvalidNodeAddress;
    }
    if (output == nullptr || output_capacity < kAssignAddressResponseBytes) {
        return CommissioningCodecStatus::OutputTooSmall;
    }
    output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
    write_u32(value.operation_id, output + 1);
    std::memcpy(output + 5, value.hardware_id, kHardwareIdBytes);
    output[13] = value.node_address;
    return CommissioningCodecStatus::Ok;
}

uint16_t commissioning_slot(
    const uint8_t hardware_id[kHardwareIdBytes],
    uint64_t round_nonce,
    uint16_t slot_count) {
    if (hardware_id == nullptr || round_nonce == 0 ||
        !valid_slot_count(slot_count)) {
        return UINT16_MAX;
    }
    uint8_t key[16]{};
    write_u64(round_nonce, key);
    std::memcpy(key + 8, hardware_id, kHardwareIdBytes);
    return static_cast<uint16_t>(crc32c(key, sizeof(key)) % slot_count);
}

bool matches_commissioning_slot(
    const uint8_t hardware_id[kHardwareIdBytes],
    const ProbeSlotRequest& request) {
    return request.slot_index == commissioning_slot(
        hardware_id, request.round_nonce, request.slot_count);
}

}  // namespace ProtocolV3
