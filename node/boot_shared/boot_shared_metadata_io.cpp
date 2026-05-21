#include "boot_shared/boot_shared_metadata_io.h"

#include <cstring>

#include "boot/boot_config.h"
#include "hardware/address_mapped.h"
#include "hardware/flash.h"
#include "pico/stdlib.h"
#include "pico/flash.h"
#include "pico/multicore.h"

namespace boot {
namespace {

const BootMetadata* metadata_ptr(uint32_t flash_offset) {
    return reinterpret_cast<const BootMetadata*>(XIP_BASE + flash_offset);
}

bool pick_newest_valid_copy(const BootMetadata* a,
                            const BootMetadata* b,
                            BootMetadata& out) {
    const bool a_valid = boot_metadata_is_valid(*a);
    const bool b_valid = boot_metadata_is_valid(*b);

    if (!a_valid && !b_valid) {
        return false;
    }

    if (a_valid && !b_valid) {
        out = *a;
        return true;
    }

    if (!a_valid && b_valid) {
        out = *b;
        return true;
    }

    out = (a->generation >= b->generation) ? *a : *b;
    return true;
}

struct FlashWriteContext {
    uint32_t flash_offset;
    const uint8_t* data;
    size_t length;
};

void flash_program_callback(void* user_data) {
    auto* ctx = static_cast<FlashWriteContext*>(user_data);
    flash_range_program(ctx->flash_offset, ctx->data, ctx->length);
}

void flash_erase_callback(void* user_data) {
    auto* ctx = static_cast<FlashWriteContext*>(user_data);
    flash_range_erase(ctx->flash_offset, ctx->length);
}

bool write_one_metadata_copy(uint32_t flash_offset, const BootMetadata& metadata) {
    alignas(256) uint8_t page_buffer[FLASH_PAGE_SIZE]{};

    static_assert(sizeof(BootMetadata) <= FLASH_PAGE_SIZE,
                  "BootMetadata must fit in one flash page");

    std::memcpy(page_buffer, &metadata, sizeof(BootMetadata));

    FlashWriteContext erase_ctx{
        .flash_offset = flash_offset,
        .data = nullptr,
        .length = FLASH_SECTOR_SIZE
    };

    FlashWriteContext prog_ctx{
        .flash_offset = flash_offset,
        .data = page_buffer,
        .length = FLASH_PAGE_SIZE
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

    const BootMetadata* verify = metadata_ptr(flash_offset);
    return boot_metadata_is_valid(*verify) &&
           verify->generation == metadata.generation;
}

} // namespace

bool app_boot_metadata_load(BootMetadata& metadata) {
    const BootMetadata* primary = metadata_ptr(METADATA_PRIMARY_OFFSET);
    const BootMetadata* secondary = metadata_ptr(METADATA_SECONDARY_OFFSET);

    if (pick_newest_valid_copy(primary, secondary, metadata)) {
        return true;
    }

    metadata = boot_metadata_make_default();
    return false;
}

bool app_boot_metadata_save(BootMetadata& metadata) {
    ++metadata.generation;
    boot_metadata_finalize(metadata);

    const bool ok_primary = write_one_metadata_copy(METADATA_PRIMARY_OFFSET, metadata);
    const bool ok_secondary = write_one_metadata_copy(METADATA_SECONDARY_OFFSET, metadata);

    return ok_primary && ok_secondary;
}

} // namespace boot
