#pragma once

#include "pico/stdlib.h"

template <typename Context>
inline void run_core1_transport_loop(Context& ctx) {
    ctx.core1_ready = true;

    while (!ctx.stop_requested) {
        if (ctx.transport != nullptr) {
            ctx.transport->update();
        }

        tight_loop_contents();
    }
}