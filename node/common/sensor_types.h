#pragma once

#include <cstdint>

enum class SensorStatus {
    Ok = 0,
    NotInitialized,
    Busy,
    CommError,
    Timeout,
    InvalidParam,
    NotSupported,
    InvalidDevice,
    NoData,
    InvalidSample,
    InternalError
};

struct AccelerometerConfig {
    uint16_t odr_hz = 250;
    uint8_t range_g = 2;
    uint8_t high_pass_corner = 0;
};

struct AccelSample {
    int32_t x = 0;
    int32_t y = 0;
    int32_t z = 0;
};

struct TemperatureSample {
    uint16_t raw = 0;
    float celsius = 0.0f;
};

struct SelfTestResult {
    AccelSample baseline;
    AccelSample st1;
    AccelSample st2;
    AccelSample delta;
    bool passed;
};
