#pragma once

#include <cstddef>
#include <cstdint>

namespace measurement {

constexpr int32_t kSampleAxisMin = -0x00800000;
constexpr int32_t kSampleAxisMax = 0x007fffff;
constexpr uint16_t kKnownTimingQualityFlags = 0x001fu;

// Kept at 32 bytes for predictable RAM budgeting. Sequence identity belongs
// to the logical MeasurementJournal and is not repeated in every payload.
struct SampleRecord {
    uint64_t device_time_us = 0;
    int32_t x = 0;
    int32_t y = 0;
    int32_t z = 0;
    uint32_t config_revision = 0;
    uint32_t timing_segment_id = 0;
    uint16_t timing_quality_flags = 0;
    uint16_t reserved = 0;
};

static_assert(sizeof(SampleRecord) == 32, "SampleRecord RAM cost changed");

inline bool valid_sample_record(const SampleRecord& sample) {
    return sample.x >= kSampleAxisMin && sample.x <= kSampleAxisMax &&
           sample.y >= kSampleAxisMin && sample.y <= kSampleAxisMax &&
           sample.z >= kSampleAxisMin && sample.z <= kSampleAxisMax &&
           sample.config_revision != 0 && sample.timing_segment_id != 0 &&
           (sample.timing_quality_flags &
            static_cast<uint16_t>(~kKnownTimingQualityFlags)) == 0;
}

struct SampleSpan {
    const SampleRecord* data = nullptr;
    size_t size = 0;
};

}  // namespace measurement
