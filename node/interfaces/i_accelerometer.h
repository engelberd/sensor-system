#pragma once

#include <cstddef>
#include <cstdint>

#include "common/sensor_types.h"
#include "timing/sample_timing.h"

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
    virtual SensorStatus read_fifo_samples_timed(
        AccelSample* samples,
        SampleDeviceTime* times,
        size_t max_samples,
        size_t& samples_read
    ) {
        const SensorStatus status =
            read_fifo_samples(samples, max_samples, samples_read);
        if (times != nullptr) {
            for (size_t i = 0; i < samples_read; ++i) {
                times[i] = {};
            }
        }
        return status;
    }
    virtual SensorStatus read_fifo_entries(uint8_t& entries) = 0;

    virtual bool supports_fifo() const = 0;
    virtual bool supports_sample_timestamps() const { return false; }
    virtual void notify_fifo_timing_discontinuity(uint16_t quality_flags) {
        (void)quality_flags;
    }
    virtual SensorStatus configure_fifo(uint8_t watermark) = 0;

    virtual bool supports_data_ready_interrupt() const = 0;
    virtual uint8_t consume_data_ready_event_sources() = 0;
    virtual SensorStatus read_status(uint8_t& status) = 0;
    virtual SensorStatus refresh_diagnostic_snapshot() = 0;
    virtual SensorDiagnosticSnapshot diagnostic_snapshot() const = 0;

    virtual SensorStatus set_offset(int32_t x, int32_t y, int32_t z) = 0;

    virtual SensorStatus run_self_test(SelfTestResult& result) = 0;
};
