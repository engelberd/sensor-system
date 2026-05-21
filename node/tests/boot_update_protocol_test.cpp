#include <cassert>
#include <cstddef>
#include <cstdint>

#include "boot/boot_update_protocol.h"

namespace {

void test_crc32_matches_standard_vector() {
    static constexpr char kInput[] = "123456789";
    const uint32_t crc = boot::update_packet_crc32(
        reinterpret_cast<const uint8_t*>(kInput),
        sizeof(kInput) - 1
    );
    assert(crc == 0xCBF43926u);
}

void test_crc32_empty_returns_zero() {
    assert(boot::update_packet_crc32(nullptr, 0) == 0u);

    const uint8_t dummy = 0;
    assert(boot::update_packet_crc32(&dummy, 0) == 0u);
}

void test_packet_type_validation() {
    assert(boot::update_packet_type_is_valid(
        static_cast<uint8_t>(boot::UpdatePacketType::Hello)
    ));
    assert(boot::update_packet_type_is_valid(
        static_cast<uint8_t>(boot::UpdatePacketType::Begin)
    ));
    assert(boot::update_packet_type_is_valid(
        static_cast<uint8_t>(boot::UpdatePacketType::Chunk)
    ));
    assert(boot::update_packet_type_is_valid(
        static_cast<uint8_t>(boot::UpdatePacketType::End)
    ));
    assert(boot::update_packet_type_is_valid(
        static_cast<uint8_t>(boot::UpdatePacketType::Abort)
    ));
    assert(boot::update_packet_type_is_valid(
        static_cast<uint8_t>(boot::UpdatePacketType::Ack)
    ));
    assert(boot::update_packet_type_is_valid(
        static_cast<uint8_t>(boot::UpdatePacketType::Error)
    ));

    assert(!boot::update_packet_type_is_valid(0));
    assert(!boot::update_packet_type_is_valid(6));
    assert(!boot::update_packet_type_is_valid(42));
}

}  // namespace

int main() {
    test_crc32_matches_standard_vector();
    test_crc32_empty_returns_zero();
    test_packet_type_validation();
    return 0;
}

