#pragma once

#include <cstdint>

#include "boot/boot_metadata.h"

namespace boot {

enum class TrialBootState : uint8_t {
    Unavailable = 0,
    NotRunningFromSlot,
    Stable,
    PendingCurrentSlot,
    PendingOtherSlot,
};

enum class TrialBootConfirmResult : uint8_t {
    Confirmed = 0,
    AlreadyConfirmed,
    NotRunningTrial,
    InvalidMetadata,
};

TrialBootState classify_trial_boot(
    const BootMetadata& metadata, SlotId current_slot);

// Mutates RAM only. The application-side API owns redundant flash storage.
TrialBootConfirmResult confirm_trial_boot_in_metadata(
    BootMetadata& metadata, SlotId current_slot);

}  // namespace boot
