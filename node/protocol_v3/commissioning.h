#pragma once

#include <cstddef>
#include <cstdint>

#include "protocol_v3/application.h"
#include "protocol_v3/identity.h"

namespace ProtocolV3 {

constexpr uint8_t kCommissioningServiceId =
    static_cast<uint8_t>(ServiceId::Commissioning);
constexpr uint8_t kProbeSlotMessageId = 1;
constexpr uint8_t kAssignAddressMessageId = 2;
constexpr size_t kProbeSlotRequestBytes = 12;
constexpr size_t kProbeSlotResponseBytes = 23;
constexpr size_t kAssignAddressRequestBytes = 21;
constexpr size_t kAssignAddressResponseBytes = 14;
constexpr uint16_t kMaxCommissioningSlots = 256;

struct ProbeSlotRequest {
    uint64_t round_nonce = 0;
    uint16_t slot_count = 0;
    uint16_t slot_index = 0;
};

struct CommissioningIdentity {
    uint64_t round_nonce = 0;
    uint8_t hardware_id[kHardwareIdBytes]{};
    uint8_t board_revision = 0;
    FirmwareVersion firmware{};
    uint8_t framing_version = kFramingVersion;
    uint8_t application_version = kApplicationProtocolVersion;
};

struct AssignAddressRequest {
    uint32_t operation_id = 0;
    uint64_t round_nonce = 0;
    uint8_t hardware_id[kHardwareIdBytes]{};
    uint8_t node_address = 0;
};

struct AssignAddressResponse {
    uint32_t operation_id = 0;
    uint8_t hardware_id[kHardwareIdBytes]{};
    uint8_t node_address = 0;
};

enum class CommissioningCodecStatus : uint8_t {
    Ok = 0,
    OutputTooSmall,
    InvalidPayloadLength,
    ZeroRoundNonce,
    InvalidSlotCount,
    InvalidSlotIndex,
    ZeroOperationId,
    InvalidNodeAddress,
    InvalidBoardRevision,
};

CommissioningCodecStatus decode_probe_slot_request(
    const uint8_t* payload,
    size_t payload_size,
    ProbeSlotRequest& output);
CommissioningCodecStatus decode_assign_address_request(
    const uint8_t* payload,
    size_t payload_size,
    AssignAddressRequest& output);
CommissioningCodecStatus encode_probe_slot_success(
    const CommissioningIdentity& value,
    uint8_t* output,
    size_t output_capacity);
CommissioningCodecStatus encode_assign_address_success(
    const AssignAddressResponse& value,
    uint8_t* output,
    size_t output_capacity);

uint16_t commissioning_slot(
    const uint8_t hardware_id[kHardwareIdBytes],
    uint64_t round_nonce,
    uint16_t slot_count);
bool matches_commissioning_slot(
    const uint8_t hardware_id[kHardwareIdBytes],
    const ProbeSlotRequest& request);
bool is_assignable_node_address(uint8_t address);

}  // namespace ProtocolV3
