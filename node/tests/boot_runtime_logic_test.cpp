#include <cassert>

#include "boot/boot_metadata.h"
#include "boot_shared/boot_runtime_logic.h"

namespace {

boot::BootMetadata trial_metadata() {
    boot::BootMetadata metadata = boot::boot_metadata_make_default();
    metadata.slot_a = {1024, 1, 1, 3, 0, 1, 0, 0};
    metadata.slot_b = {1024, 2, 2, 0, 0, 1, 0, 0};
    metadata.active_slot = boot::SlotId::A;
    metadata.boot_slot = boot::SlotId::B;
    metadata.trial_slot = boot::SlotId::B;
    metadata.trial_armed = 1;
    metadata.trial_attempted = 1;
    boot::boot_metadata_finalize(metadata);
    return metadata;
}

void test_only_current_attempted_trial_can_confirm() {
    auto metadata = trial_metadata();
    assert(boot::classify_trial_boot(metadata, boot::SlotId::B) ==
           boot::TrialBootState::PendingCurrentSlot);
    assert(boot::classify_trial_boot(metadata, boot::SlotId::A) ==
           boot::TrialBootState::PendingOtherSlot);

    auto wrong_slot = metadata;
    assert(boot::confirm_trial_boot_in_metadata(
               wrong_slot, boot::SlotId::A) ==
           boot::TrialBootConfirmResult::NotRunningTrial);
    assert(wrong_slot.active_slot == boot::SlotId::A);
    assert(wrong_slot.trial_armed == 1);

    assert(boot::confirm_trial_boot_in_metadata(
               metadata, boot::SlotId::B) ==
           boot::TrialBootConfirmResult::Confirmed);
    assert(metadata.active_slot == boot::SlotId::B);
    assert(metadata.boot_slot == boot::SlotId::B);
    assert(metadata.trial_slot == boot::SlotId::None);
    assert(metadata.trial_armed == 0);
    assert(metadata.slot_b.confirmed_boots == 1);
}

void test_retry_after_commit_is_idempotent() {
    auto metadata = trial_metadata();
    assert(boot::confirm_trial_boot_in_metadata(
               metadata, boot::SlotId::B) ==
           boot::TrialBootConfirmResult::Confirmed);
    boot::boot_metadata_finalize(metadata);
    assert(boot::confirm_trial_boot_in_metadata(
               metadata, boot::SlotId::B) ==
           boot::TrialBootConfirmResult::AlreadyConfirmed);
    assert(metadata.slot_b.confirmed_boots == 1);
}

void test_unattempted_trial_is_rejected() {
    auto metadata = trial_metadata();
    metadata.trial_attempted = 0;
    boot::boot_metadata_finalize(metadata);
    assert(boot::confirm_trial_boot_in_metadata(
               metadata, boot::SlotId::B) ==
           boot::TrialBootConfirmResult::NotRunningTrial);
}

void test_confirmation_counter_saturates() {
    auto metadata = trial_metadata();
    metadata.slot_b.confirmed_boots = UINT32_MAX;
    boot::boot_metadata_finalize(metadata);
    assert(boot::confirm_trial_boot_in_metadata(
               metadata, boot::SlotId::B) ==
           boot::TrialBootConfirmResult::Confirmed);
    assert(metadata.slot_b.confirmed_boots == UINT32_MAX);
}

}  // namespace

int main() {
    test_only_current_attempted_trial_can_confirm();
    test_retry_after_commit_is_idempotent();
    test_unattempted_trial_is_rejected();
    test_confirmation_counter_saturates();
    return 0;
}
