#pragma once

#include <cstdint>

enum class TimestampSource : uint8_t {
    None = 0,
    DrdyTimeUs64 = 1,
};

enum TimingQualityFlag : uint16_t {
    TIMING_QUALITY_NONE = 0,
    TIMING_QUALITY_LOCKED = 1u << 0,
    TIMING_QUALITY_DEGRADED = 1u << 1,
    TIMING_QUALITY_INVALID = 1u << 2,
    TIMING_QUALITY_DRDY_MISSING = 1u << 3,
    TIMING_QUALITY_DRDY_EXCESS = 1u << 4,
    TIMING_QUALITY_RING_OVERFLOW = 1u << 5,
    TIMING_QUALITY_FIFO_OVERRUN = 1u << 6,
    TIMING_QUALITY_INCOMPLETE_SAMPLE = 1u << 7,
    TIMING_QUALITY_RECOVERY = 1u << 8,
};

struct DrdyTimestamp {
    uint64_t event_seq = 0;
    uint64_t device_time_us = 0;
};

struct SampleDeviceTime {
    uint64_t device_time_us = 0;
    uint32_t timing_segment_id = 0;
    uint16_t quality_flags = TIMING_QUALITY_INVALID;
    TimestampSource source = TimestampSource::None;
    uint8_t reserved = 0;

    bool valid() const {
        return source == TimestampSource::DrdyTimeUs64 &&
               (quality_flags & TIMING_QUALITY_INVALID) == 0;
    }
};

struct TimedAccelSample {
    int32_t x = 0;
    int32_t y = 0;
    int32_t z = 0;
    SampleDeviceTime time{};
};

static_assert(sizeof(DrdyTimestamp) == 16, "DrdyTimestamp size mismatch");
static_assert(sizeof(SampleDeviceTime) == 16, "SampleDeviceTime size mismatch");
