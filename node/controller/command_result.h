#pragma once

#include <cstdint>

#include "transport/status_codes.h"

enum class DeferredCommandEffectType : uint8_t {
    None = 0,
    Restart,
    EnterBootloader,
    ApplyRs485BaudRate,
};

struct DeferredCommandEffect {
    DeferredCommandEffectType type = DeferredCommandEffectType::None;
    uint32_t value = 0;
};

struct CommandResult {
    CommandResult() = default;

    CommandResult(StatusCode status_value)
        : status(status_value) {
    }

    CommandResult(StatusCode status_value, DeferredCommandEffect effect)
        : status(status_value),
          deferred_effect(effect) {
    }

    StatusCode status = StatusCode::InternalError;
    DeferredCommandEffect deferred_effect{};
};
