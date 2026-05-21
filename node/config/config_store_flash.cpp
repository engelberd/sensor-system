#include "config/config_store_flash.h"

#include <cstring>

#include "config/config_crc.h"
#include "hardware/address_mapped.h"
#include "hardware/flash.h"
#include "pico/flash.h"
#include "pico/multicore.h"

namespace {
constexpr uint16_t kLegacyPersistentConfigVersionV1 = 1;
constexpr uint16_t kLegacyPersistentConfigVersionV2 = 2;

#pragma pack(push, 1)
struct LegacyDeviceConfigV1 {
    uint8_t node_id = 1;
    uint16_t odr_hz = 250;
    uint8_t range_g = 2;
    int32_t offset_x = 0;
    int32_t offset_y = 0;
    int32_t offset_z = 0;
    uint16_t act_threshold = 0;
    uint8_t act_count = 1;
    uint8_t fifo_watermark = 30;
};

struct LegacyPersistentConfigV1 {
    uint32_t magic = PERSISTENT_CONFIG_MAGIC;
    uint16_t version = kLegacyPersistentConfigVersionV1;
    uint16_t reserved0 = 0;
    uint32_t generation = 0;
    LegacyDeviceConfigV1 device{};
    uint32_t crc32 = 0;
};

struct LegacyDeviceConfigV2 {
    uint8_t node_id = UNASSIGNED_NODE_ID;
    uint32_t baudrate = 115200;
    uint16_t odr_hz = 250;
    uint8_t range_g = 2;
    int32_t offset_x = 0;
    int32_t offset_y = 0;
    int32_t offset_z = 0;
    uint16_t act_threshold = 0;
    uint8_t act_count = 1;
    uint8_t fifo_watermark = 30;
};

struct LegacyPersistentConfigV2 {
    uint32_t magic = PERSISTENT_CONFIG_MAGIC;
    uint16_t version = kLegacyPersistentConfigVersionV2;
    uint16_t reserved0 = 0;
    uint32_t generation = 0;
    LegacyDeviceConfigV2 device{};
    uint32_t crc32 = 0;
};
#pragma pack(pop)

struct FlashWriteContext {
    uint32_t flash_offset;
    const uint8_t* data;
    size_t length;
};

void flash_erase_callback(void* user_data) {
    auto* ctx = static_cast<FlashWriteContext*>(user_data);
    flash_range_erase(ctx->flash_offset, ctx->length);
}

void flash_program_callback(void* user_data) {
    auto* ctx = static_cast<FlashWriteContext*>(user_data);
    flash_range_program(ctx->flash_offset, ctx->data, ctx->length);
}

bool is_valid_legacy_copy(const LegacyPersistentConfigV1& config) {
    if (config.magic != PERSISTENT_CONFIG_MAGIC) {
        return false;
    }

    if (config.version != kLegacyPersistentConfigVersionV1) {
        return false;
    }

    LegacyPersistentConfigV1 copy = config;
    const uint32_t stored_crc = copy.crc32;
    copy.crc32 = 0;

    return stored_crc == config_crc32(&copy, sizeof(LegacyPersistentConfigV1));
}

bool is_valid_legacy_copy(const LegacyPersistentConfigV2& config) {
    if (config.magic != PERSISTENT_CONFIG_MAGIC) {
        return false;
    }

    if (config.version != kLegacyPersistentConfigVersionV2) {
        return false;
    }

    LegacyPersistentConfigV2 copy = config;
    const uint32_t stored_crc = copy.crc32;
    copy.crc32 = 0;

    return stored_crc == config_crc32(&copy, sizeof(LegacyPersistentConfigV2));
}

PersistentConfig migrate_legacy_copy(const LegacyPersistentConfigV1& legacy) {
    PersistentConfig migrated{};
    migrated.generation = legacy.generation;
    migrated.device.node_id = legacy.device.node_id;
    migrated.device.baudrate = 115200;
    migrated.device.odr_hz = legacy.device.odr_hz;
    migrated.device.range_g = legacy.device.range_g;
    migrated.device.high_pass_corner = 0;
    migrated.device.offset_x = legacy.device.offset_x;
    migrated.device.offset_y = legacy.device.offset_y;
    migrated.device.offset_z = legacy.device.offset_z;
    migrated.device.act_threshold = legacy.device.act_threshold;
    migrated.device.act_count = legacy.device.act_count;
    migrated.device.fifo_watermark = legacy.device.fifo_watermark;
    migrated.crc32 = 0;
    migrated.crc32 = config_crc32(&migrated, sizeof(PersistentConfig));
    return migrated;
}

PersistentConfig migrate_legacy_copy(const LegacyPersistentConfigV2& legacy) {
    PersistentConfig migrated{};
    migrated.generation = legacy.generation;
    migrated.device.node_id = legacy.device.node_id;
    migrated.device.baudrate = legacy.device.baudrate;
    migrated.device.odr_hz = legacy.device.odr_hz;
    migrated.device.range_g = legacy.device.range_g;
    migrated.device.high_pass_corner = 0;
    migrated.device.offset_x = legacy.device.offset_x;
    migrated.device.offset_y = legacy.device.offset_y;
    migrated.device.offset_z = legacy.device.offset_z;
    migrated.device.act_threshold = legacy.device.act_threshold;
    migrated.device.act_count = legacy.device.act_count;
    migrated.device.fifo_watermark = legacy.device.fifo_watermark;
    migrated.crc32 = 0;
    migrated.crc32 = config_crc32(&migrated, sizeof(PersistentConfig));
    return migrated;
}
}

const PersistentConfig* FlashConfigStore::flash_ptr(uint32_t flash_offset) {
    return reinterpret_cast<const PersistentConfig*>(XIP_BASE + flash_offset);
}

bool FlashConfigStore::is_valid_copy(const PersistentConfig& config) {
    if (config.magic != PERSISTENT_CONFIG_MAGIC) {
        return false;
    }

    if (config.version != PERSISTENT_CONFIG_VERSION) {
        return false;
    }

    PersistentConfig copy = config;
    const uint32_t stored_crc = copy.crc32;
    copy.crc32 = 0;

    return stored_crc == config_crc32(&copy, sizeof(PersistentConfig));
}

bool FlashConfigStore::load_copy(uint32_t flash_offset, PersistentConfig& config) {
    const PersistentConfig* current = flash_ptr(flash_offset);
    if (is_valid_copy(*current)) {
        config = *current;
        return true;
    }

    const auto* legacy_v2 =
        reinterpret_cast<const LegacyPersistentConfigV2*>(XIP_BASE + flash_offset);
    if (is_valid_legacy_copy(*legacy_v2)) {
        config = migrate_legacy_copy(*legacy_v2);
        return true;
    }

    const auto* legacy =
        reinterpret_cast<const LegacyPersistentConfigV1*>(XIP_BASE + flash_offset);
    if (!is_valid_legacy_copy(*legacy)) {
        return false;
    }

    config = migrate_legacy_copy(*legacy);
    return true;
}

bool FlashConfigStore::load(PersistentConfig& config) {
    PersistentConfig a{};
    PersistentConfig b{};
    const bool a_valid = load_copy(CONFIG_COPY0_OFFSET, a);
    const bool b_valid = load_copy(CONFIG_COPY1_OFFSET, b);

    if (!a_valid && !b_valid) {
        return false;
    }

    if (a_valid && !b_valid) {
        config = a;
        return true;
    }

    if (!a_valid && b_valid) {
        config = b;
        return true;
    }

    config = (a.generation >= b.generation) ? a : b;
    return true;
}

bool FlashConfigStore::write_copy(uint32_t flash_offset, const PersistentConfig& config) {
    alignas(FLASH_PAGE_BYTES) uint8_t page_buffer[FLASH_PAGE_BYTES] = {};

    static_assert(sizeof(PersistentConfig) <= FLASH_PAGE_BYTES,
                  "PersistentConfig must fit in one flash page");

    std::memcpy(page_buffer, &config, sizeof(PersistentConfig));

    FlashWriteContext erase_ctx{
        .flash_offset = flash_offset,
        .data = nullptr,
        .length = FLASH_SECTOR_BYTES
    };

    FlashWriteContext prog_ctx{
        .flash_offset = flash_offset,
        .data = page_buffer,
        .length = FLASH_PAGE_BYTES
    };

    multicore_lockout_start_blocking();

    const int erase_rc = flash_safe_execute(flash_erase_callback, &erase_ctx, UINT32_MAX);
    if (erase_rc != PICO_OK) {
        multicore_lockout_end_blocking();
        return false;
    }

    const int prog_rc = flash_safe_execute(flash_program_callback, &prog_ctx, UINT32_MAX);
    multicore_lockout_end_blocking();

    if (prog_rc != PICO_OK) {
        return false;
    }

    const PersistentConfig* verify = flash_ptr(flash_offset);
    return is_valid_copy(*verify) && verify->generation == config.generation;
}

bool FlashConfigStore::save(const PersistentConfig& config) {
    const PersistentConfig* a = flash_ptr(CONFIG_COPY0_OFFSET);
    const PersistentConfig* b = flash_ptr(CONFIG_COPY1_OFFSET);

    const bool a_valid = is_valid_copy(*a);
    const bool b_valid = is_valid_copy(*b);

    uint32_t target_offset = CONFIG_COPY0_OFFSET;

    if (!a_valid) {
        target_offset = CONFIG_COPY0_OFFSET;
    } else if (!b_valid) {
        target_offset = CONFIG_COPY1_OFFSET;
    } else {
        target_offset = (a->generation <= b->generation) ? CONFIG_COPY0_OFFSET
                                                         : CONFIG_COPY1_OFFSET;
    }

    return write_copy(target_offset, config);
}
