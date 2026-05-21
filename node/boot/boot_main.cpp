#include "pico/stdlib.h"
#include "hardware/watchdog.h"
#include "hardware/gpio.h"
#include "boards/pico.h"

#include "boot/boot_config.h"
#include "boot/boot_decision.h"
#include "boot/boot_flash.h"
#include "boot/boot_jump.h"
#include "boot/boot_maintenance.h"
#include "boot/boot_metadata.h"
#include "boot/boot_update_engine.h"
#include "boot/boot_update_server.h"

namespace {

void init_status_led() {
    gpio_init(PICO_DEFAULT_LED_PIN);
    gpio_set_dir(PICO_DEFAULT_LED_PIN, GPIO_OUT);
    gpio_put(PICO_DEFAULT_LED_PIN, 0);
}

void set_status_led(bool on) {
    gpio_put(PICO_DEFAULT_LED_PIN, on ? 1 : 0);
}

void blink_status_led(uint32_t count, uint32_t on_ms, uint32_t off_ms) {
    for (uint32_t i = 0; i < count; ++i) {
        set_status_led(true);
        sleep_ms(on_ms);
        set_status_led(false);
        sleep_ms(off_ms);
    }
}

[[noreturn]] void reboot_forever() {
    watchdog_reboot(0, 0, 10);
    while (true) {
        tight_loop_contents();
    }
}

bool run_one_update_session(boot::BootMetadata& metadata) {
    boot::BootUpdateEngine engine{};
    boot::BootUpdateServer server(engine, metadata.node_id);
    return server.run(metadata);
}

} // namespace

int main() {
    using namespace boot;

    init_status_led();
    blink_status_led(2, 60, 60);

    stdio_init_all();
    boot_maintenance_uart_init();

    BootMetadata metadata{};
    const bool metadata_loaded = boot_metadata_load(metadata);

    if (!metadata_loaded) {
        if (boot_rebuild_factory_metadata(metadata)) {
            boot_metadata_save(metadata);
            boot_console_puts("BOOT> metadata rebuilt from slot A\r\n");
        } else {
            metadata = boot_metadata_make_default();
            boot_metadata_save(metadata);
            boot_console_puts("BOOT> metadata recreated from defaults\r\n");
        }
    }

    const bool force_update =
        (metadata.boot_flags & BOOT_FLAG_ENTER_UPDATE) != 0u;
    if (force_update) {
        boot_console_puts("BOOT> update requested by application\r\n");
    }

    MaintenanceCommand cmd = force_update
        ? MaintenanceCommand::EnterUpdate
        : boot_wait_for_maintenance_command(MAINTENANCE_WINDOW_MS);

    if (cmd == MaintenanceCommand::EnterUpdate) {
        blink_status_led(3, 80, 80);
        boot_console_puts("BOOT> entering requested update mode\r\n");
        for (uint32_t attempt = 0; attempt < REQUESTED_UPDATE_MAX_SESSIONS; ++attempt) {
            set_status_led(true);
            const bool success = run_one_update_session(metadata);
            if (success) {
                boot_console_puts("BOOT> rebooting after successful update\r\n");
                reboot_forever();
            }

            boot_console_puts("BOOT> update session ended, waiting for retry\r\n");
        }

        boot_console_puts("BOOT> requested update timed out, booting last confirmed app\r\n");
        metadata.boot_flags &= static_cast<uint8_t>(~BOOT_FLAG_ENTER_UPDATE);
        metadata.last_error = static_cast<uint32_t>(BootError::RequestedUpdateTimeout);
        boot_metadata_save(metadata);
        cmd = MaintenanceCommand::BootDefault;
    }

    BootDecision decision = boot_decide_next(metadata, cmd);

    if (decision.slot != SlotId::None) {
        if (!boot_image_header_looks_valid(decision.slot)) {
            if (metadata.trial_armed != 0u && decision.slot == metadata.trial_slot) {
                SlotMetadata& trial_md =
                    boot_slot_metadata(metadata, metadata.trial_slot);
                ++trial_md.failed_trial_boots;

                const SlotId rollback_slot = metadata.active_slot;
                metadata.trial_armed = 0u;
                metadata.trial_attempted = 0u;
                metadata.trial_slot = SlotId::None;
                metadata.boot_slot = rollback_slot;
                metadata.last_error = static_cast<uint32_t>(BootError::TrialRollback);

                if (boot_slot_is_bootable(metadata, rollback_slot) &&
                    boot_image_header_looks_valid(rollback_slot)) {
                    decision.mode = BootMode::Normal;
                    decision.slot = rollback_slot;
                    decision.error = BootError::TrialRollback;
                } else {
                    decision.mode = BootMode::Maintenance;
                    decision.slot = SlotId::None;
                    decision.error = BootError::NoBootableSlot;
                }
            } else {
                metadata.last_error =
                    static_cast<uint32_t>(BootError::RequestedSlotNotBootable);
                decision.mode = BootMode::Maintenance;
                decision.slot = SlotId::None;
                decision.error = BootError::RequestedSlotNotBootable;
            }

            decision.metadata_changed = true;
        }
    }

    // boot_counter participates in metadata CRC, so mutating it before the
    // boot decision would make the already-loaded metadata appear invalid and
    // force the bootloader into maintenance mode on every boot.
    ++metadata.boot_counter;

    if (decision.metadata_changed) {
        boot_metadata_save(metadata);
    }

    switch (decision.mode) {
        case BootMode::Normal:
        case BootMode::Safe:
            set_status_led(false);
            boot_console_puts("BOOT> jumping to application\r\n");
            boot_jump_to_slot(decision.slot);
            break;

        case BootMode::Maintenance:
            blink_status_led(4, 80, 80);
            boot_console_puts("BOOT> entering maintenance mode\r\n");
            while (true) {
                set_status_led(true);
                if (run_one_update_session(metadata)) {
                    boot_console_puts("BOOT> rebooting after successful update\r\n");
                    reboot_forever();
                }
                set_status_led(false);
                boot_console_puts("BOOT> update session ended, staying in maintenance\r\n");
            }
            break;

        case BootMode::Halt:
        default:
            set_status_led(true);
            boot_console_puts("BOOT> halt\r\n");
            while (true) {
                tight_loop_contents();
            }
            break;
    }
}
