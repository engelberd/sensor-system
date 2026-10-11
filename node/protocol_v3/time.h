#pragma once

#include <cstddef>
#include <cstdint>

#include "protocol_v3/application.h"

namespace ProtocolV3 {

constexpr uint8_t kTimeServiceId = static_cast<uint8_t>(ServiceId::Time);
constexpr uint8_t kObserveClockMessageId = 1;
constexpr size_t kObserveClockRequestBytes = 0;
constexpr size_t kObserveClockResponseBytes = 25;

struct TimeExchangeResponse {
    uint64_t boot_epoch = 0;
    uint64_t node_receive_us = 0;
    uint64_t node_send_us = 0;
};

enum class TimeCodecStatus : uint8_t {
    Ok = 0,
    InvalidRequestLength,
    OutputTooSmall,
    ZeroBootEpoch,
    NodeClockRegressed,
};

TimeCodecStatus validate_time_request(size_t payload_size);
TimeCodecStatus encode_time_success(
    const TimeExchangeResponse& response,
    uint8_t* output,
    size_t output_capacity);
TimeCodecStatus stamp_time_transmit(
    uint8_t* payload,
    size_t payload_size,
    uint64_t node_send_us);

}  // namespace ProtocolV3
