#include "boot_shared/boot_runtime_api.h"

#include "boot/boot_metadata.h"
#include "boot_shared/boot_shared_metadata_io.h"

namespace boot {
namespace {

bool confirm_trial_success_in_metadata(BootMetadata& metadata) {
    if (metadata.trial_armed == 0u || metadata.trial_slot == SlotId::None) {
        return false;
    }

    if (!boot_slot_is_bootable(metadata, metadata.trial_slot)) {
        return false;
    }

    metadata.active_slot = metadata.trial_slot;
    metadata.boot_slot = metadata.trial_slot;

    SlotMetadata& sm = boot_slot_metadata(metadata, metadata.trial_slot);
    ++sm.confirmed_boots;

    metadata.trial_slot = SlotId::None;
    metadata.trial_armed = 0u;
    metadata.trial_attempted = 0u;
    metadata.last_error = 0u;

    return true;
}

} // namespace

bool app_sync_boot_settings(uint8_t node_id) {
    BootMetadata metadata{};
    app_boot_metadata_load(metadata);

    if (metadata.node_id == node_id) {
        return true;
    }

    metadata.node_id = node_id;
    return app_boot_metadata_save(metadata);
}

bool app_confirm_boot_success() {
    BootMetadata metadata{};
    if (!app_boot_metadata_load(metadata)) {
        return false;
    }

    if (!confirm_trial_success_in_metadata(metadata)) {
        return false;
    }

    return app_boot_metadata_save(metadata);
}

bool app_request_enter_bootloader() {
    BootMetadata metadata{};
    app_boot_metadata_load(metadata);

    metadata.boot_flags |= BOOT_FLAG_ENTER_UPDATE;
    metadata.last_error = 0u;

    return app_boot_metadata_save(metadata);
}

} // namespace boot
