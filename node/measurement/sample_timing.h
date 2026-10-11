#pragma once

#include <cstddef>
#include <cstdint>

#include "measurement/measurement_journal.h"

namespace measurement {

constexpr uint16_t kTimingValidFlag = 1u << 0u;

struct TimingSummary {
    uint64_t first_device_time_us = 0;
    uint64_t last_device_time_us = 0;
    uint32_t max_fit_residual_us = 0;
    uint16_t quality_flags = 0;
};

enum class TimingSummaryStatus : uint8_t {
    Ok = 0,
    EmptyRange,
    ReversedAnchors,
    NonMonotonicSamples,
    InvalidValidAnchors,
};

inline const SampleRecord& sample_at(
    const MeasurementReadView& view, size_t index) {
    return index < view.first.size
        ? view.first.data[index]
        : view.second.data[index - view.first.size];
}

inline TimingSummaryStatus summarize_timing(
    const MeasurementReadView& view, TimingSummary& output) {
    const size_t count = view.sample_count();
    if (count == 0) return TimingSummaryStatus::EmptyRange;

    const SampleRecord& first = sample_at(view, 0);
    const SampleRecord& last = sample_at(view, count - 1);
    if (last.device_time_us < first.device_time_us) {
        return TimingSummaryStatus::ReversedAnchors;
    }

    output = {};
    output.first_device_time_us = first.device_time_us;
    output.last_device_time_us = last.device_time_us;
    output.quality_flags = first.timing_quality_flags;

    if (count == 1) return TimingSummaryStatus::Ok;

    const uint64_t intervals = count - 1;
    const uint64_t duration = last.device_time_us - first.device_time_us;
    const uint64_t whole_step = duration / intervals;
    const uint64_t remainder = duration % intervals;
    bool all_valid = (first.timing_quality_flags & kTimingValidFlag) != 0;
    uint64_t previous_time = first.device_time_us;

    for (size_t index = 1; index < count; ++index) {
        const SampleRecord& sample = sample_at(view, index);
        if (sample.device_time_us < previous_time) {
            return TimingSummaryStatus::NonMonotonicSamples;
        }
        previous_time = sample.device_time_us;
        all_valid = all_valid &&
                    (sample.timing_quality_flags & kTimingValidFlag) != 0;
        output.quality_flags |= static_cast<uint16_t>(
            sample.timing_quality_flags &
            static_cast<uint16_t>(~kTimingValidFlag));

        const uint64_t scaled_remainder = remainder * index;
        const uint64_t expected =
            first.device_time_us + whole_step * index +
            (scaled_remainder + intervals / 2u) / intervals;
        const uint64_t actual = sample.device_time_us;
        const uint64_t residual = actual > expected
            ? actual - expected
            : expected - actual;
        const uint32_t bounded = residual > UINT32_MAX
            ? UINT32_MAX
            : static_cast<uint32_t>(residual);
        if (bounded > output.max_fit_residual_us) {
            output.max_fit_residual_us = bounded;
        }
    }

    if (!all_valid) {
        output.quality_flags &= static_cast<uint16_t>(~kTimingValidFlag);
    } else if (duration == 0) {
        return TimingSummaryStatus::InvalidValidAnchors;
    }
    return TimingSummaryStatus::Ok;
}

}  // namespace measurement
