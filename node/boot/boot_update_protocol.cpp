#include "boot/boot_update_protocol.h"

namespace boot {
namespace {

uint32_t crc32_update(uint32_t crc, uint8_t byte) {
    crc ^= static_cast<uint32_t>(byte);

    for (int i = 0; i < 8; ++i) {
        const uint32_t mask = -(crc & 1u);
        crc = (crc >> 1) ^ (0xEDB88320u & mask);
    }

    return crc;
}

} // namespace

uint32_t update_packet_crc32(const uint8_t* data, size_t length) {
    if (data == nullptr || length == 0) {
        return 0u;
    }

    uint32_t crc = 0xFFFFFFFFu;
    for (size_t i = 0; i < length; ++i) {
        crc = crc32_update(crc, data[i]);
    }

    return ~crc;
}

bool update_packet_type_is_valid(uint8_t raw_type) {
    switch (static_cast<UpdatePacketType>(raw_type)) {
        case UpdatePacketType::Hello:
        case UpdatePacketType::Begin:
        case UpdatePacketType::Chunk:
        case UpdatePacketType::End:
        case UpdatePacketType::Abort:
        case UpdatePacketType::Ack:
        case UpdatePacketType::Error:
            return true;
        default:
            return false;
    }
}

} // namespace boot