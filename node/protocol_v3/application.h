#pragma once

#include <cstdint>

namespace ProtocolV3 {

enum class ServiceId : uint8_t {
    Identity = 1,
    Measurement = 2,
    Time = 3,
    Health = 4,
    Configuration = 5,
    Lifecycle = 6,
    Diagnostics = 7,
    Commissioning = 8,
    Telemetry = 9,
};

constexpr uint32_t service_capability_bit(ServiceId service) {
    return 1u << (static_cast<uint8_t>(service) - 1u);
}

enum class ResponseStatus : uint8_t {
    Ok = 0,
    MalformedRequest = 1,
    UnsupportedService = 2,
    UnsupportedMessage = 3,
    WrongMode = 4,
    Busy = 5,
    Conflict = 6,
    InvalidState = 7,
    InternalError = 8,
};

}  // namespace ProtocolV3
