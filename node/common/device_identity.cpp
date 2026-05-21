#include "common/device_identity.h"

#include "pico/unique_id.h"

DeviceHardwareId read_device_hardware_id() {
    pico_unique_board_id_t board_id{};
    pico_get_unique_board_id(&board_id);

    DeviceHardwareId hardware_id{};
    static_assert(PICO_UNIQUE_BOARD_ID_SIZE_BYTES >= DEVICE_HARDWARE_ID_SIZE,
                  "board id must be at least 8 bytes");
    for (size_t i = 0; i < DEVICE_HARDWARE_ID_SIZE; ++i) {
        hardware_id.bytes[i] = board_id.id[i];
    }
    return hardware_id;
}

bool device_hardware_id_equals(const DeviceHardwareId& lhs, const uint8_t* rhs) {
    if (rhs == nullptr) {
        return false;
    }

    for (size_t i = 0; i < DEVICE_HARDWARE_ID_SIZE; ++i) {
        if (lhs.bytes[i] != rhs[i]) {
            return false;
        }
    }

    return true;
}

uint32_t device_hardware_id_hash32(const DeviceHardwareId& id) {
    uint32_t hash = 2166136261u;
    for (size_t i = 0; i < DEVICE_HARDWARE_ID_SIZE; ++i) {
        hash ^= static_cast<uint32_t>(id.bytes[i]);
        hash *= 16777619u;
    }
    return hash;
}
