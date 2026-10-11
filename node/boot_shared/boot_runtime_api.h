#pragma once

#include <cstdint>

#include "boot_shared/boot_runtime_logic.h"

namespace boot {

// Call from application after it has been healthy long enough.
bool app_confirm_boot_success();

// Protocol-v5 path: only the image executing from the armed, already-attempted
// trial slot may promote itself to active. Repeating after a lost response is
// successful when this same slot is already active.
TrialBootConfirmResult app_confirm_current_trial();
TrialBootState app_trial_boot_state();
SlotId app_current_slot();

// Keep boot metadata in sync with current node identity.
bool app_sync_boot_settings(uint8_t node_id);

// Request reboot into bootloader / maintenance mode.
// This requests addressed update mode after reboot.
bool app_request_enter_bootloader();

} // namespace boot
