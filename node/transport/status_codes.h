#pragma once

#include <cstdint>

enum class StatusCode : uint8_t {
    Ok = 0,
    BadFrame = 1,
    Unsupported = 2,
    InvalidParam = 3,
    InvalidState = 4,
    Busy = 5,
    NoData = 6,
    SensorError = 7,
    ConfigError = 8,
    StorageError = 9,
    SaveFailed = 10,
    LoadFailed = 11,
    InternalError = 12
};
