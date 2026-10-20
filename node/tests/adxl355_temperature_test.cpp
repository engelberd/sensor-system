#include <cassert>
#include <cmath>
#include <cstdint>

#include "adxl355/adxl355_temperature.h"

namespace {

void test_decodes_12_bit_register_value() {
    assert(ADXL355::Temperature::decode_raw(0xA7, 0x5D) == 1885);
}

void test_datasheet_reference_converts_to_25_celsius() {
    const float temperature = ADXL355::Temperature::to_celsius(1885);
    assert(std::fabs(temperature - 25.0f) < 0.001f);
}

void test_rejects_electrically_implausible_extremes() {
    assert(!ADXL355::Temperature::is_plausible_raw(0));
    assert(!ADXL355::Temperature::is_plausible_raw(4095));
    assert(ADXL355::Temperature::is_plausible_raw(1885));

    // The former unchecked conversion presented raw=0 as roughly +233 deg C.
    assert(ADXL355::Temperature::to_celsius(0) > 200.0f);
}

void test_accepts_only_back_to_back_coherent_values() {
    assert(ADXL355::Temperature::readings_are_coherent(1885, 1885));
    assert(ADXL355::Temperature::readings_are_coherent(1885, 1887));
    assert(!ADXL355::Temperature::readings_are_coherent(1885, 1888));
}

}  // namespace

int main() {
    test_decodes_12_bit_register_value();
    test_datasheet_reference_converts_to_25_celsius();
    test_rejects_electrically_implausible_extremes();
    test_accepts_only_back_to_back_coherent_values();
    return 0;
}
