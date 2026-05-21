#include "boot/boot_flash.h"

#include <cstring>

#include "boot/boot_config.h"
#include "hardware/address_mapped.h"
#include "hardware/flash.h"
#include "hardware/sync.h"
#include "pico/stdlib.h"

namespace boot {
namespace {

constexpr uint32_t APP_FLASH_BASE = XIP_BASE;

const BootMetadata* metadata_ptr(uint32_t flash_offset) {
    return reinterpret_cast<const BootMetadata*>(APP_FLASH_BASE + flash_offset);
}

bool write_one_metadata_copy(uint32_t flash_offset, const BootMetadata& metadata) {
    alignas(256) uint8_t page_buffer[FLASH_PAGE_SIZE]{};

    static_assert(sizeof(BootMetadata) <= FLASH_PAGE_SIZE,
                  "BootMetadata must fit in one flash page");

    std::memcpy(page_buffer, &metadata, sizeof(BootMetadata));

    uint32_t irq_state = save_and_disable_interrupts();

    flash_range_erase(flash_offset, FLASH_SECTOR_SIZE);
    flash_range_program(flash_offset, page_buffer, FLASH_PAGE_SIZE);

    restore_interrupts(irq_state);

    const BootMetadata* verify = metadata_ptr(flash_offset);
    return boot_metadata_is_valid(*verify) &&
           verify->generation == metadata.generation;
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

    if (a->generation >= b->generation) {
        out = *a;
    } else {
        out = *b;
    }

    return true;
}

bool slot_vector_table_looks_valid(uint32_t xip_address) {
    const uint32_t* vectors = reinterpret_cast<const uint32_t*>(xip_address);

    const uint32_t initial_sp = vectors[0];
    const uint32_t reset_handler = vectors[1];

    // SP should point into SRAM.
    // Pico 2 SRAM starts at 0x20000000.
    if ((initial_sp & 0xFF000000u) != 0x20000000u) {
        return false;
    }

    // Reset handler should point into XIP flash region and be Thumb.
    if ((reset_handler & 0xFF000000u) != 0x10000000u) {
        return false;
    }

    if ((reset_handler & 0x1u) == 0u) {
        return false;
    }

    return true;
}

uint32_t slot_xip_address(SlotId slot) {
    const uint32_t offset = boot_slot_offset(slot);
    return APP_FLASH_BASE + offset;
}

bool infer_slot_image_properties(SlotId slot,
                                 uint32_t& image_size,
                                 uint32_t& image_crc32) {
    if (!boot_image_header_looks_valid(slot)) {
        return false;
    }

    const uint32_t slot_size = boot_slot_size(slot);
    const uint8_t* image = reinterpret_cast<const uint8_t*>(slot_xip_address(slot));

    uint32_t used_size = 0u;
    for (uint32_t i = slot_size; i > 0u; --i) {
        if (image[i - 1u] != 0xFFu) {
            used_size = i;
            break;
        }
    }

    if (used_size == 0u) {
        return false;
    }

    image_size = used_size;
    image_crc32 = boot_crc32(image, image_size);
    return true;
}

} // namespace

bool boot_metadata_load(BootMetadata& metadata) {
    const BootMetadata* primary = metadata_ptr(METADATA_PRIMARY_OFFSET);
    const BootMetadata* secondary = metadata_ptr(METADATA_SECONDARY_OFFSET);

    if (pick_newest_valid_copy(primary, secondary, metadata)) {
        return true;
    }

    metadata = boot_metadata_make_default();
    return false;
}

bool boot_metadata_save(BootMetadata& metadata) {
    ++metadata.generation;
    boot_metadata_finalize(metadata);

    const bool ok_primary = write_one_metadata_copy(METADATA_PRIMARY_OFFSET, metadata);
    const bool ok_secondary = write_one_metadata_copy(METADATA_SECONDARY_OFFSET, metadata);

    return ok_primary && ok_secondary;
}

bool boot_image_header_looks_valid(SlotId slot) {
    if (slot == SlotId::None) {
        return false;
    }

    return slot_vector_table_looks_valid(slot_xip_address(slot));
}

bool boot_slot_crc_matches(const BootMetadata& metadata, SlotId slot) {
    if (!boot_slot_is_bootable(metadata, slot)) {
        return false;
    }

    const SlotMetadata& sm = boot_slot_metadata(metadata, slot);
    const uint8_t* image = reinterpret_cast<const uint8_t*>(slot_xip_address(slot));

    const uint32_t crc = boot_crc32(image, sm.image_size);
    return crc == sm.image_crc32;
}

bool boot_rebuild_factory_metadata(BootMetadata& metadata) {
    uint32_t slot_a_size = 0u;
    uint32_t slot_a_crc32 = 0u;
    if (!infer_slot_image_properties(SlotId::A, slot_a_size, slot_a_crc32)) {
        return false;
    }

    metadata = boot_metadata_make_default();
    metadata.active_slot = SlotId::A;
    metadata.boot_slot = SlotId::A;
    metadata.trial_slot = SlotId::None;
    metadata.trial_armed = 0u;
    metadata.trial_attempted = 0u;

    metadata.slot_a.image_size = slot_a_size;
    metadata.slot_a.image_crc32 = slot_a_crc32;
    metadata.slot_a.image_version = 0u;
    metadata.slot_a.confirmed_boots = 0u;
    metadata.slot_a.failed_trial_boots = 0u;
    metadata.slot_a.image_valid = 1u;

    metadata.slot_b = {};
    return true;
}

} // namespace boot
