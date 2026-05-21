#pragma once

#include "boot/boot_metadata.h"

namespace boot {

// Shared runtime-safe metadata access from application side.
// These APIs are intended for the normal application, not the bootloader.

bool app_boot_metadata_load(BootMetadata& metadata);
bool app_boot_metadata_save(BootMetadata& metadata);

} // namespace boot