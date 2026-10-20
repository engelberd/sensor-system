#pragma once

#include <cstdint>

#include "pico/critical_section.h"

namespace telemetry {

constexpr uint16_t kAdxl355TemperatureField = 1u << 0;

struct Snapshot {
    uint64_t captured_device_time_us = 0;
    uint16_t valid_fields = 0;
    uint16_t adxl355_temperature_raw = 0;
};

// One core-0 writer and one core-1 reader share a fixed-size snapshot. The
// critical section protects the 64-bit timestamp from a torn cross-core read;
// no sensor I/O is ever performed while holding it.
class SnapshotCache {
public:
    SnapshotCache() { critical_section_init(&critical_section_); }

    SnapshotCache(const SnapshotCache&) = delete;
    SnapshotCache& operator=(const SnapshotCache&) = delete;

    bool publish(const Snapshot& snapshot) {
        if (snapshot.captured_device_time_us == 0 ||
            snapshot.valid_fields != kAdxl355TemperatureField) {
            return false;
        }
        critical_section_enter_blocking(&critical_section_);
        snapshot_ = snapshot;
        available_ = true;
        critical_section_exit(&critical_section_);
        return true;
    }

    bool try_read(Snapshot& output) const {
        critical_section_enter_blocking(
            const_cast<critical_section_t*>(&critical_section_));
        const bool available = available_;
        if (available) output = snapshot_;
        critical_section_exit(
            const_cast<critical_section_t*>(&critical_section_));
        return available;
    }

private:
    mutable critical_section_t critical_section_{};
    Snapshot snapshot_{};
    bool available_ = false;
};

}  // namespace telemetry
