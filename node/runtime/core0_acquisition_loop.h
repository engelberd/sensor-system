#pragma once

#include "boot/picoboot_constants.h"
#include "hardware/watchdog.h"
#include "pico/bootrom.h"
#include "pico/stdlib.h"
#include "system/runtime_context.h"

[[noreturn]] inline void run_system_action(uint8_t action) {
    if (action == RuntimeSystemActionEnterBootloader) {
        rom_reboot(
            REBOOT2_FLAG_REBOOT_TYPE_NORMAL |
                REBOOT2_FLAG_REBOOT_TO_ARM |
                REBOOT2_FLAG_NO_RETURN_ON_SUCCESS,
            10,
            1,
            0
        );
    } else {
        watchdog_reboot(0, 0, 100);
    }

    while (true) {
        tight_loop_contents();
    }
}

template <typename Context>
inline void run_core0_acquisition_loop(Context& ctx) {
    while (!ctx.stop_requested) {
        watchdog_update();

        const uint8_t action = ctx.requested_action;
        if (action != RuntimeSystemActionNone) {
            ctx.stop_requested = true;
            sleep_ms(20);
            run_system_action(action);
        }

        if (ctx.acquisition != nullptr) {
            ctx.acquisition->update();
        }

        tight_loop_contents();
    }
}
