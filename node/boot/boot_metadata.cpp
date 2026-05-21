#include "boot/boot_metadata.h"

#include <cstring>

#include "boot/boot_config.h"

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

uint32_t boot_crc32(const void* data, size_t length) {
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

void boot_metadata_finalize(BootMetadata& metadata) {
    metadata.magic = BOOT_METADATA_MAGIC;
    metadata.version = BOOT_METADATA_VERSION;

    metadata.metadata_crc32 = 0u;
    metadata.metadata_crc32 = boot_crc32(&metadata, sizeof(BootMetadata));
}

bool boot_metadata_is_valid(const BootMetadata& metadata) {
    if (metadata.magic != BOOT_METADATA_MAGIC) {
        return false;
    }

    if (metadata.version != BOOT_METADATA_VERSION) {
        return false;
    }

    BootMetadata copy = metadata;
    const uint32_t stored_crc = copy.metadata_crc32;
    copy.metadata_crc32 = 0u;

    const uint32_t computed_crc = boot_crc32(&copy, sizeof(BootMetadata));
    return stored_crc == computed_crc;
}

const SlotMetadata& boot_slot_metadata(const BootMetadata& metadata, SlotId slot) {
    switch (slot) {
        case SlotId::A:
            return metadata.slot_a;
        case SlotId::B:
            return metadata.slot_b;
        case SlotId::None:
        default:
            return metadata.slot_a; // caller should validate slot first
    }
}

SlotMetadata& boot_slot_metadata(BootMetadata& metadata, SlotId slot) {
    switch (slot) {
        case SlotId::A:
            return metadata.slot_a;
        case SlotId::B:
            return metadata.slot_b;
        case SlotId::None:
        default:
            return metadata.slot_a; // caller should validate slot first
    }
}

bool boot_slot_is_bootable(const BootMetadata& metadata, SlotId slot) {
    if (slot == SlotId::None) {
        return false;
    }

    const SlotMetadata& sm = boot_slot_metadata(metadata, slot);

    if (sm.image_valid == 0u) {
        return false;
    }

    if (sm.image_size == 0u) {
        return false;
    }

    if (sm.image_crc32 == 0u) {
        return false;
    }

    if (slot == SlotId::A && sm.image_size > SLOT_A_SIZE) {
        return false;
    }

    if (slot == SlotId::B && sm.image_size > SLOT_B_SIZE) {
        return false;
    }

    return true;
}

uint32_t boot_slot_offset(SlotId slot) {
    switch (slot) {
        case SlotId::A:
            return SLOT_A_OFFSET;
        case SlotId::B:
            return SLOT_B_OFFSET;
        case SlotId::None:
        default:
            return 0u;
    }
}

uint32_t boot_slot_size(SlotId slot) {
    switch (slot) {
        case SlotId::A:
            return SLOT_A_SIZE;
        case SlotId::B:
            return SLOT_B_SIZE;
        case SlotId::None:
        default:
            return 0u;
    }
}

BootMetadata boot_metadata_make_default() {
    BootMetadata md{};
    md.magic = BOOT_METADATA_MAGIC;
    md.version = BOOT_METADATA_VERSION;
    md.generation = 1u;

    md.active_slot = SlotId::A;
    md.boot_slot = SlotId::A;
    md.trial_slot = SlotId::None;
    md.trial_armed = 0u;
    md.trial_attempted = 0u;
    md.node_id = UNASSIGNED_NODE_ID;
    md.boot_flags = BOOT_FLAG_NONE;

    md.boot_counter = 0u;
    md.last_error = 0u;

    md.slot_a = {};
    md.slot_b = {};

    boot_metadata_finalize(md);
    return md;
}

} // namespace boot
