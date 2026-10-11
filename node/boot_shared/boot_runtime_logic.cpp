#include "boot_shared/boot_runtime_logic.h"

namespace boot {

TrialBootState classify_trial_boot(
    const BootMetadata& metadata, SlotId current_slot) {
    if (!boot_metadata_is_valid(metadata)) {
        return TrialBootState::Unavailable;
    }
    if (current_slot == SlotId::None) {
        return TrialBootState::NotRunningFromSlot;
    }
    if (metadata.trial_armed == 0u || metadata.trial_slot == SlotId::None) {
        return metadata.active_slot == current_slot &&
                       metadata.boot_slot == current_slot
            ? TrialBootState::Stable
            : TrialBootState::Unavailable;
    }
    return metadata.trial_slot == current_slot &&
                   metadata.boot_slot == current_slot &&
                   metadata.trial_attempted != 0u
        ? TrialBootState::PendingCurrentSlot
        : TrialBootState::PendingOtherSlot;
}

TrialBootConfirmResult confirm_trial_boot_in_metadata(
    BootMetadata& metadata, SlotId current_slot) {
    switch (classify_trial_boot(metadata, current_slot)) {
        case TrialBootState::Stable:
            return TrialBootConfirmResult::AlreadyConfirmed;
        case TrialBootState::PendingCurrentSlot:
            break;
        case TrialBootState::Unavailable:
            return TrialBootConfirmResult::InvalidMetadata;
        case TrialBootState::NotRunningFromSlot:
        case TrialBootState::PendingOtherSlot:
        default:
            return TrialBootConfirmResult::NotRunningTrial;
    }

    if (!boot_slot_is_bootable(metadata, current_slot)) {
        return TrialBootConfirmResult::InvalidMetadata;
    }
    metadata.active_slot = current_slot;
    metadata.boot_slot = current_slot;
    SlotMetadata& slot = boot_slot_metadata(metadata, current_slot);
    if (slot.confirmed_boots != UINT32_MAX) ++slot.confirmed_boots;
    metadata.trial_slot = SlotId::None;
    metadata.trial_armed = 0u;
    metadata.trial_attempted = 0u;
    metadata.last_error = 0u;
    return TrialBootConfirmResult::Confirmed;
}

}  // namespace boot
