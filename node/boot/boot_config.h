#pragma once

#include <cstdint>

namespace boot {

// ============================================================
// Board flash layout (Pico 2 / 4 MB flash)
// ============================================================

// Total external flash on Raspberry Pi Pico 2
static constexpr uint32_t FLASH_TOTAL_SIZE = 4u * 1024u * 1024u;

// Bootloader region
static constexpr uint32_t BOOTLOADER_OFFSET = 0x00000000u;
static constexpr uint32_t BOOTLOADER_SIZE   = 0x00020000u; // 128 kB

// Metadata / control region
static constexpr uint32_t METADATA_OFFSET   = 0x00020000u;
static constexpr uint32_t METADATA_SIZE     = 0x00020000u; // 128 kB

// App slots
static constexpr uint32_t SLOT_A_OFFSET     = 0x00040000u;
static constexpr uint32_t SLOT_A_SIZE       = 0x001E0000u; // 1.875 MB

static constexpr uint32_t SLOT_B_OFFSET     = 0x00220000u;
static constexpr uint32_t SLOT_B_SIZE       = 0x001E0000u; // 1.875 MB

// Metadata copies inside metadata region
static constexpr uint32_t METADATA_PRIMARY_OFFSET   = METADATA_OFFSET + 0x0000u;
static constexpr uint32_t METADATA_SECONDARY_OFFSET = METADATA_OFFSET + 0x1000u;

// Persistent config copies stored outside application slots.
static constexpr uint32_t CONFIG_PRIMARY_OFFSET     = METADATA_OFFSET + 0x2000u;
static constexpr uint32_t CONFIG_SECONDARY_OFFSET   = METADATA_OFFSET + 0x3000u;

// Flash program / erase assumptions
static constexpr uint32_t FLASH_PAGE_SIZE   = 256u;
static constexpr uint32_t FLASH_SECTOR_SIZE = 4096u;

// Update chunk size for RS-485 firmware upload
static constexpr uint32_t UPDATE_CHUNK_SIZE = 1024u;

// Maintenance window after reset / power-on
static constexpr uint32_t MAINTENANCE_WINDOW_MS = 5000u;

// Requested update recovery window. If power is lost or the host disappears
// during upload, the bootloader gives the host a bounded chance to resume and
// then falls back to the last confirmed application slot.
static constexpr uint32_t REQUESTED_UPDATE_MAX_SESSIONS = 6u;

// ============================================================
// Sanity checks
// ============================================================

static_assert((BOOTLOADER_OFFSET + BOOTLOADER_SIZE) == METADATA_OFFSET,
              "Bootloader region must end at metadata region start");

static_assert((SLOT_A_OFFSET + SLOT_A_SIZE) == SLOT_B_OFFSET,
              "Slot A must end where Slot B begins");

static_assert((SLOT_B_OFFSET + SLOT_B_SIZE) <= FLASH_TOTAL_SIZE,
              "App slots exceed total flash size");

static_assert((METADATA_PRIMARY_OFFSET + FLASH_SECTOR_SIZE) <= METADATA_OFFSET + METADATA_SIZE,
              "Primary metadata copy must fit metadata region");

static_assert((METADATA_SECONDARY_OFFSET + FLASH_SECTOR_SIZE) <= METADATA_OFFSET + METADATA_SIZE,
              "Secondary metadata copy must fit metadata region");

static_assert((CONFIG_PRIMARY_OFFSET + FLASH_SECTOR_SIZE) <= METADATA_OFFSET + METADATA_SIZE,
              "Primary config copy must fit metadata region");

static_assert((CONFIG_SECONDARY_OFFSET + FLASH_SECTOR_SIZE) <= METADATA_OFFSET + METADATA_SIZE,
              "Secondary config copy must fit metadata region");

} // namespace boot
