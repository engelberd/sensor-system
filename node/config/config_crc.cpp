#include "config/config_crc.h"

namespace {
uint32_t crc32_update(uint32_t crc, uint8_t byte) {
    crc ^= static_cast<uint32_t>(byte);

    for (int i = 0; i < 8; ++i) {
        const uint32_t mask = -(crc & 1u);
        crc = (crc >> 1) ^ (0xEDB88320u & mask);
    }

    return crc;
}
}

uint32_t config_crc32(const void* data, size_t length) {
    if (data == nullptr || length == 0) {
        return 0u;
    }

    const auto* bytes = static_cast<const uint8_t*>(data);

    uint32_t crc = 0xFFFFFFFFu;
    for (size_t i = 0; i < length; ++i) {
        crc = crc32_update(crc, bytes[i]);
    }

    return ~crc;
}