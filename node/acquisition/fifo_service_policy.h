#pragma once

#include <cstdint>

#include "adxl355/adxl355_registers.h"

namespace FifoServicePolicy {

constexpr uint32_t kAxesPerSample = 3u;
constexpr uint32_t kFifoCapacityEntries = 96u;

constexpr bool watermark_irq_configured(uint8_t int_map) {
    return (int_map & (ADXL355::INT_FULL_EN1 | ADXL355::INT_OVR_EN1)) != 0u;
}

constexpr bool fifo_ready(uint8_t status,
                          uint8_t fifo_entries,
                          uint8_t watermark_entries) {
    return fifo_entries >= kAxesPerSample && (
        (status & (ADXL355::STATUS_FIFO_FULL_MASK |
                   ADXL355::STATUS_FIFO_OVR_MASK)) != 0u ||
        fifo_entries >= watermark_entries
    );
}

constexpr uint32_t ceil_div(uint32_t numerator, uint32_t denominator) {
    return (numerator + denominator - 1u) / denominator;
}

constexpr uint32_t fallback_interval_ms(uint32_t odr_hz,
                                        uint32_t watermark_entries) {
    if (odr_hz == 0u) {
        odr_hz = 250u;
    }
    if (watermark_entries < kAxesPerSample) {
        watermark_entries = kAxesPerSample;
    }
    if (watermark_entries > kFifoCapacityEntries) {
        watermark_entries = kFifoCapacityEntries;
    }

    const uint32_t entries_per_second = odr_hz * kAxesPerSample;
    const uint32_t watermark_ms =
        ceil_div(watermark_entries * 1000u, entries_per_second);
    const uint32_t fifo_full_ms =
        ceil_div(kFifoCapacityEntries * 1000u, entries_per_second);

    uint32_t interval_ms = watermark_ms + (watermark_ms / 4u) + 1u;
    const uint32_t latest_safe_ms = fifo_full_ms > 2u ? fifo_full_ms - 2u : 1u;
    if (interval_ms > latest_safe_ms) {
        interval_ms = latest_safe_ms;
    }
    return interval_ms == 0u ? 1u : interval_ms;
}

}  // namespace FifoServicePolicy
