#pragma once

#include <cstdint>

#include "common/sensor_types.h"
#include "interfaces/i_temperature_sensor.h"
#include "telemetry/telemetry_snapshot.h"

namespace runtime {

using DeviceTimeSource = uint64_t (*)();

struct TelemetryRuntimePolicy {
    uint64_t refresh_interval_us = 60ull * 1000ull * 1000ull;
    uint64_t retry_interval_us = 1000ull * 1000ull;

    bool valid() const {
        return refresh_interval_us > 0 && retry_interval_us > 0 &&
               retry_interval_us <= refresh_interval_us;
    }
};

struct TelemetryRuntimeState {
    SensorStatus last_status = SensorStatus::NoData;
    uint64_t next_refresh_us = 0;
    uint32_t successful_refreshes = 0;
    uint32_t failed_refreshes = 0;
};

// Core-0 periodic producer. Acquisition serializes the short sensor access;
// protocol handlers only read SnapshotCache and never call this path.
template <typename Acquisition>
class TelemetryRuntime {
public:
    TelemetryRuntime(Acquisition& acquisition,
                     ITemperatureSensor& temperature_sensor,
                     telemetry::SnapshotCache& cache,
                     DeviceTimeSource device_time,
                     TelemetryRuntimePolicy policy = {})
        : acquisition_(acquisition),
          temperature_sensor_(temperature_sensor),
          cache_(cache),
          device_time_(device_time),
          policy_(policy) {}

    void poll() {
        if (device_time_ == nullptr || !policy_.valid()) {
            last_status_ = SensorStatus::InvalidParam;
            increment(failed_refreshes_);
            return;
        }
        const uint64_t now_us = device_time_();
        if (next_refresh_us_ != 0 && now_us < next_refresh_us_) return;

        TemperatureSample temperature{};
        const SensorStatus status = acquisition_.read_temperature(
            temperature_sensor_, temperature);
        const uint64_t captured_us = device_time_();
        last_status_ = status;
        if (status == SensorStatus::Ok && captured_us != 0 &&
            cache_.publish({
                captured_us,
                telemetry::kAdxl355TemperatureField,
                temperature.raw,
            })) {
            increment(successful_refreshes_);
            next_refresh_us_ = saturating_add(
                captured_us, policy_.refresh_interval_us);
            return;
        }

        if (status == SensorStatus::Ok) {
            last_status_ = SensorStatus::InvalidSample;
        }
        increment(failed_refreshes_);
        next_refresh_us_ = saturating_add(
            captured_us, policy_.retry_interval_us);
    }

    TelemetryRuntimeState state() const {
        return {
            last_status_,
            next_refresh_us_,
            successful_refreshes_,
            failed_refreshes_,
        };
    }

private:
    static uint64_t saturating_add(uint64_t lhs, uint64_t rhs) {
        return UINT64_MAX - lhs < rhs ? UINT64_MAX : lhs + rhs;
    }

    static void increment(uint32_t& value) {
        if (value != UINT32_MAX) ++value;
    }

    Acquisition& acquisition_;
    ITemperatureSensor& temperature_sensor_;
    telemetry::SnapshotCache& cache_;
    DeviceTimeSource device_time_ = nullptr;
    TelemetryRuntimePolicy policy_{};
    SensorStatus last_status_ = SensorStatus::NoData;
    uint64_t next_refresh_us_ = 0;
    uint32_t successful_refreshes_ = 0;
    uint32_t failed_refreshes_ = 0;
};

}  // namespace runtime
