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

enum : uint8_t {
    SENSOR_DIAG_FLAG_FIFO_EMPTY_ENTRY = 0x01,
    SENSOR_DIAG_FLAG_FIFO_AXIS_MISMATCH = 0x02,
    SENSOR_DIAG_FLAG_INT1_EVENT = 0x04,
    SENSOR_DIAG_FLAG_DRDY_EVENT = 0x08,
    SENSOR_DIAG_FLAG_FIFO_LOSS_UNCERTAIN = 0x10,
};

struct SensorDiagnosticSnapshot {
    uint8_t last_status_reg = 0;
    uint8_t last_fifo_entries = 0;
    uint8_t last_fifo_read_status = 0;
    uint8_t flags = 0;
    uint8_t last_int_map = 0;
    uint8_t last_fifo_samples = 0;
    uint8_t last_int1_level = 0;
    uint8_t last_drdy_level = 0;
    uint32_t int1_gpio_edges = 0;
    uint32_t drdy_gpio_edges = 0;
    uint8_t last_fifo_requested_samples = 0;
    uint8_t last_fifo_discarded_samples = 0;
};

struct SelfTestResult {
    AccelSample baseline;
    AccelSample st1;
    AccelSample st2;
    AccelSample delta;
    bool passed;
};
