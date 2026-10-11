#pragma once

#include <cstddef>
#include <cstdint>

#include "common/sensor_types.h"

enum class InputContinuity : uint8_t {
    Preserved = 0,
    Broken,
};

enum class SensorReadBreakReason : uint8_t {
    None = 0,
    InvalidFifoData,
    TransportUncertain,
};

// One immutable result of one FIFO transaction. The discarded count describes
// raw sensor inputs, not missing samples after filtering or decimation.
struct FifoReadOutcome {
    SensorStatus status = SensorStatus::InternalError;
    size_t samples_read = 0;
    uint32_t discarded_input_samples = 0;
    InputContinuity continuity = InputContinuity::Preserved;
    SensorReadBreakReason break_reason = SensorReadBreakReason::None;

    bool valid_for(size_t max_samples) const {
        if (samples_read > max_samples) return false;
        if (continuity == InputContinuity::Preserved) {
            return break_reason == SensorReadBreakReason::None &&
                   discarded_input_samples == 0;
        }
        return continuity == InputContinuity::Broken &&
               break_reason != SensorReadBreakReason::None;
    }
};
