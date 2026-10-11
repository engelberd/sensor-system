#include "boot_shared/boot_runtime_api.h"

#include "boot/boot_metadata.h"
#include "boot_shared/boot_shared_metadata_io.h"

namespace boot {

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
    return app_confirm_current_trial() == TrialBootConfirmResult::Confirmed;
}

SlotId app_current_slot() {
#if SENSOR_SYSTEM_APP_SLOT == 1
    return SlotId::A;
#elif SENSOR_SYSTEM_APP_SLOT == 2
    return SlotId::B;
#else
    return SlotId::None;
#endif
}

TrialBootState app_trial_boot_state() {
    BootMetadata metadata{};
    if (!app_boot_metadata_load(metadata)) {
        return TrialBootState::Unavailable;
    }
    return classify_trial_boot(metadata, app_current_slot());
}

TrialBootConfirmResult app_confirm_current_trial() {
    BootMetadata metadata{};
    if (!app_boot_metadata_load(metadata)) {
        return TrialBootConfirmResult::InvalidMetadata;
    }
    const TrialBootConfirmResult result =
        confirm_trial_boot_in_metadata(metadata, app_current_slot());
    if (result != TrialBootConfirmResult::Confirmed) return result;
    return app_boot_metadata_save(metadata)
        ? TrialBootConfirmResult::Confirmed
        : TrialBootConfirmResult::InvalidMetadata;
}

bool app_request_enter_bootloader() {
    BootMetadata metadata{};
    app_boot_metadata_load(metadata);

    metadata.boot_flags |= BOOT_FLAG_ENTER_UPDATE;
    metadata.last_error = 0u;

    return app_boot_metadata_save(metadata);
}

} // namespace boot
