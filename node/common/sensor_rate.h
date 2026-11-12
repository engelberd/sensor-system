#pragma once
#include <cstdint>

// ADXL355 rates are powers-of-two fractions of 4000 Hz: exactly representable
// in binary float. Floats stay local; protocol configuration uses integer codes.
namespace SensorRate {
inline bool from_register(uint8_t code, float& hz) {
    if (code > 10) return false;
    hz = 4000.0f / static_cast<float>(1u << code);
    return true;
}
inline bool to_register(float hz, uint8_t& code) {
    for (uint8_t candidate = 0; candidate <= 10; ++candidate) {
        float expected = 0;
        from_register(candidate, expected);
        if (expected == hz) { code = candidate; return true; }
    }
    return false;
}
} // namespace SensorRate
