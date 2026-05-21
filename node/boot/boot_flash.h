#pragma once

#include <cstdint>

#include "boot/boot_metadata.h"

namespace boot {

// ============================================================
// Metadata I/O
// ============================================================

bool boot_metadata_load(BootMetadata& metadata);
bool boot_metadata_save(BootMetadata& metadata);

// ============================================================
// Image validation helpers
// ============================================================

bool boot_image_header_looks_valid(SlotId slot);
bool boot_slot_crc_matches(const BootMetadata& metadata, SlotId slot);
bool boot_rebuild_factory_metadata(BootMetadata& metadata);

} // namespace boot
