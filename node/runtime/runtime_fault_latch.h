#pragma once

#include <atomic>
#include <cstdint>

namespace runtime {

static_assert(std::atomic<uint8_t>::is_always_lock_free,
              "cross-core fault latch must be lock-free");

enum class RuntimeFaultReason : uint8_t {
    None = 0,
    InvalidBootEpoch,
    MeasurementJournalRejected,
};

// Cross-core first-fault latch. It is write-once for the current boot because
// SensorRuntime::Faulted is terminal until a controlled restart.
class RuntimeFaultLatch {
public:
    bool trip(RuntimeFaultReason reason) {
        if (reason == RuntimeFaultReason::None) return false;
        uint8_t expected = static_cast<uint8_t>(RuntimeFaultReason::None);
        return reason_.compare_exchange_strong(
            expected,
            static_cast<uint8_t>(reason),
            std::memory_order_release,
            std::memory_order_relaxed
        );
    }

    RuntimeFaultReason reason() const {
        return static_cast<RuntimeFaultReason>(
            reason_.load(std::memory_order_acquire)
        );
    }

    bool tripped() const {
        return reason() != RuntimeFaultReason::None;
    }

private:
    std::atomic<uint8_t> reason_{
        static_cast<uint8_t>(RuntimeFaultReason::None)};
};

}  // namespace runtime
