#pragma once

#include <cstddef>
#include <cstdint>

#include "common/protocol_ids.h"

namespace boot {

// ============================================================
// Constants
// ============================================================

static constexpr uint32_t BOOT_METADATA_MAGIC   = 0x424F4F54u; // 'BOOT'
static constexpr uint16_t BOOT_METADATA_VERSION = 2u;

// ============================================================
// Slot identifiers
// ============================================================

enum class SlotId : uint8_t {
    None = 0,
    A    = 1,
    B    = 2
};

// ============================================================
// Boot mode
// ============================================================

enum class BootMode : uint8_t {
    Normal      = 0,
    Safe        = 1,
    Maintenance = 2,
    Halt        = 3
};

// ============================================================
// Maintenance command received during maintenance window
// ============================================================

enum class MaintenanceCommand : uint8_t {
    None          = 0,
    BootDefault   = 1,
    BootSlotA     = 2,
    BootSlotB     = 3,
    BootSafeA     = 4,
    BootSafeB     = 5,
    EnterUpdate   = 6,
    StayInBoot    = 7
};

enum BootMetadataFlags : uint8_t {
    BOOT_FLAG_NONE = 0,
    BOOT_FLAG_ENTER_UPDATE = 1u << 0
};

// ============================================================
// Slot metadata
// ============================================================

#pragma pack(push, 1)
struct SlotMetadata {
    uint32_t image_size = 0;
    uint32_t image_crc32 = 0;
    uint32_t image_version = 0;

    uint32_t confirmed_boots = 0;
    uint32_t failed_trial_boots = 0;

    uint8_t image_valid = 0;
    uint8_t reserved0 = 0;
    uint16_t reserved1 = 0;
};

// ============================================================
// Global boot metadata
// ============================================================

struct BootMetadata {
    uint32_t magic = BOOT_METADATA_MAGIC;
    uint16_t version = BOOT_METADATA_VERSION;
    uint16_t reserved0 = 0;

    uint32_t generation = 0;

    // Last known stable slot
    SlotId active_slot = SlotId::None;

    // Slot selected for next normal boot
    SlotId boot_slot = SlotId::None;

    // Trial boot state
    SlotId trial_slot = SlotId::None;
    uint8_t trial_armed = 0;
    uint8_t trial_attempted = 0;

    // Node identity and persistent boot requests.
    uint8_t node_id = UNASSIGNED_NODE_ID;
    uint8_t boot_flags = BOOT_FLAG_NONE;

    // Slot records
    SlotMetadata slot_a{};
    SlotMetadata slot_b{};

    // Diagnostics / errors
    uint32_t boot_counter = 0;
    uint32_t last_error = 0;

    // Metadata integrity
    uint32_t metadata_crc32 = 0;
};
#pragma pack(pop)

// ============================================================
// Validation / helpers
// ============================================================

uint32_t boot_crc32(const void* data, size_t length);

void boot_metadata_finalize(BootMetadata& metadata);
bool boot_metadata_is_valid(const BootMetadata& metadata);

const SlotMetadata& boot_slot_metadata(const BootMetadata& metadata, SlotId slot);
SlotMetadata& boot_slot_metadata(BootMetadata& metadata, SlotId slot);

bool boot_slot_is_bootable(const BootMetadata& metadata, SlotId slot);
uint32_t boot_slot_offset(SlotId slot);
uint32_t boot_slot_size(SlotId slot);

BootMetadata boot_metadata_make_default();

} // namespace boot
