#pragma once

#include <cstdint>

enum RuntimeSystemAction : uint8_t {
    RuntimeSystemActionNone = 0,
    RuntimeSystemActionRestart = 1,
    RuntimeSystemActionEnterBootloader = 2,
};

template <typename AcquisitionT, typename TransportT, typename ObserverT>
struct RuntimeContext {
    AcquisitionT* acquisition = nullptr;
    TransportT* transport = nullptr;
    ObserverT* observer = nullptr;

    volatile bool core1_ready = false;
    volatile bool stop_requested = false;
    volatile uint8_t requested_action = RuntimeSystemActionNone;
};
