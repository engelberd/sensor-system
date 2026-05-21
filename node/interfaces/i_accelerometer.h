#pragma once

#include <cstddef>
#include <cstdint>

#include "common/sensor_types.h"

class IAccelerometer {
public:
    virtual ~IAccelerometer() = default;

    virtual SensorStatus init() = 0;
    virtual SensorStatus check_device() = 0;
    virtual SensorStatus configure(const AccelerometerConfig& config) = 0;

    virtual SensorStatus read_sample(AccelSample& sample) = 0;
    virtual SensorStatus read_fifo_samples(AccelSample* samples,
                                           size_t max_samples,
                                           size_t& samples_read) = 0;

    virtual bool supports_fifo() const = 0;
    virtual SensorStatus configure_fifo(uint8_t watermark) = 0;

    virtual bool supports_data_ready_interrupt() const = 0;
    virtual bool consume_data_ready_event() = 0;
    virtual SensorStatus read_status(uint8_t& status) = 0;

    virtual SensorStatus set_offset(int32_t x, int32_t y, int32_t z) = 0;

    virtual SensorStatus run_self_test(SelfTestResult& result) = 0;
};
