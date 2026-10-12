#include "protocol_v3/time_service.h"

namespace ProtocolV3 {
namespace {

void write_u64_le(uint8_t* output, uint64_t value) {
    for (size_t index = 0; index < 8; ++index) {
        output[index] = static_cast<uint8_t>(value >> (index * 8u));
    }
}

uint64_t read_u64_le(const uint8_t* input) {
    uint64_t value = 0;
    for (size_t index = 0; index < 8; ++index) {
        value |= static_cast<uint64_t>(input[index]) << (index * 8u);
    }
    return value;
}

}  // namespace

TimeCodecStatus validate_time_request(size_t payload_size) {
    return payload_size == kObserveClockRequestBytes
        ? TimeCodecStatus::Ok
        : TimeCodecStatus::InvalidRequestLength;
}

TimeCodecStatus encode_time_success(
    const TimeExchangeResponse& response,
    uint8_t* output,
    size_t output_capacity) {
    if (output == nullptr || output_capacity < kObserveClockResponseBytes) {
        return TimeCodecStatus::OutputTooSmall;
    }
    if (response.boot_epoch == 0) return TimeCodecStatus::ZeroBootEpoch;
    if (response.node_send_us < response.node_receive_us) {
        return TimeCodecStatus::NodeClockRegressed;
    }
    output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
    write_u64_le(output + 1, response.boot_epoch);
    write_u64_le(output + 9, response.node_receive_us);
    write_u64_le(output + 17, response.node_send_us);
    return TimeCodecStatus::Ok;
}

TimeCodecStatus stamp_time_transmit(
    uint8_t* payload,
    size_t payload_size,
    uint64_t node_send_us) {
    if (payload == nullptr || payload_size != kObserveClockResponseBytes) {
        return TimeCodecStatus::OutputTooSmall;
    }
    const uint64_t node_receive_us = read_u64_le(payload + 9);
    if (node_send_us < node_receive_us) {
        return TimeCodecStatus::NodeClockRegressed;
    }
    write_u64_le(payload + 17, node_send_us);
    return TimeCodecStatus::Ok;
}

}  // namespace ProtocolV3
