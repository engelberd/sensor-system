#pragma once

#include <cstdint>

#include "common/device_config.h"

static constexpr uint32_t PERSISTENT_CONFIG_MAGIC = 0x43464732; // CFG2
static constexpr uint16_t PERSISTENT_CONFIG_VERSION = 3;

#pragma pack(push, 1)
struct PersistentConfig {
    uint32_t magic = PERSISTENT_CONFIG_MAGIC;
    uint16_t version = PERSISTENT_CONFIG_VERSION;
    uint16_t reserved0 = 0;

    uint32_t generation = 0;

    DeviceConfig device{};

    uint32_t crc32 = 0;
};
#pragma pack(pop)
