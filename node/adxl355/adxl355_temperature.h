#pragma once

#include <cstdint>

namespace ADXL355::Temperature {

// Rev. D datasheet typical transfer function: 1885 LSB at 25 deg C and
// -9.05 LSB/deg C. Raw remains the source value; Celsius is derived data.
constexpr uint16_t kRawAt25C = 1885;
constexpr float kSlopeLsbPerC = -9.05f;

// Deliberately wider than the ADXL355 -40..125 deg C operating range. The
// margin avoids rejecting a real boundary reading while still rejecting the
// all-zero/all-one SPI values that used to appear as roughly +233/-219 deg C.
constexpr uint16_t kMinimumPlausibleRaw = 750;
constexpr uint16_t kMaximumPlausibleRaw = 2700;

// Back-to-back reads may differ by a code while the non-double-buffered
// temperature registers update. A larger jump cannot be physical at SPI speed.
constexpr uint16_t kMaximumCoherentDeltaRaw = 2;
constexpr uint8_t kMaximumReadAttempts = 3;

constexpr uint16_t decode_raw(uint8_t temp2, uint8_t temp1) {
    return static_cast<uint16_t>(
        (static_cast<uint16_t>(temp2 & 0x0F) << 8) |
        static_cast<uint16_t>(temp1)
    );
}

constexpr bool is_plausible_raw(uint16_t raw) {
    return raw >= kMinimumPlausibleRaw && raw <= kMaximumPlausibleRaw;
}

constexpr uint16_t absolute_delta(uint16_t lhs, uint16_t rhs) {
    return lhs >= rhs
        ? static_cast<uint16_t>(lhs - rhs)
        : static_cast<uint16_t>(rhs - lhs);
}

constexpr bool readings_are_coherent(uint16_t lhs, uint16_t rhs) {
    return absolute_delta(lhs, rhs) <= kMaximumCoherentDeltaRaw;
}

constexpr float to_celsius(uint16_t raw) {
    return 25.0f +
        (static_cast<float>(raw) - static_cast<float>(kRawAt25C)) /
            kSlopeLsbPerC;
}

}  // namespace ADXL355::Temperature
