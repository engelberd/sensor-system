#pragma once

#include <cstddef>
#include <cstdint>

static constexpr size_t DEVICE_HARDWARE_ID_SIZE = 8;

struct DeviceHardwareId {
    uint8_t bytes[DEVICE_HARDWARE_ID_SIZE]{};
};

DeviceHardwareId read_device_hardware_id();
bool device_hardware_id_equals(const DeviceHardwareId& lhs, const uint8_t* rhs);
uint32_t device_hardware_id_hash32(const DeviceHardwareId& id);
