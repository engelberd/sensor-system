#pragma once

#include <cstdint>

namespace boot {

// Call from application after it has been healthy long enough.
bool app_confirm_boot_success();

// Keep boot metadata in sync with current node identity.
bool app_sync_boot_settings(uint8_t node_id);

// Request reboot into bootloader / maintenance mode.
// This requests addressed update mode after reboot.
bool app_request_enter_bootloader();

} // namespace boot
