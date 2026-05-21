#pragma once

#include <cstdint>

struct StoredSample {
    uint64_t sample_seq = 0;
    int32_t x = 0;
    int32_t y = 0;
    int32_t z = 0;
};