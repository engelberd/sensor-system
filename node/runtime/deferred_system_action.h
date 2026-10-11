#pragma once

#include <atomic>
#include <cstdint>

namespace runtime {

enum class DeferredSystemActionKind : uint8_t {
    None = 0,
    EnterBootloader,
};

enum class DeferredSystemActionRequestStatus : uint8_t {
    Accepted = 0,
    AlreadyRequested,
    Conflict,
    InvalidAction,
};

// Single-producer (core 1) / single-consumer (core 0) action latch. Actions are
// requested only after a response has physically left the RS485 endpoint.
class DeferredSystemAction {
public:
    DeferredSystemActionRequestStatus request(
        DeferredSystemActionKind action) {
        if (action == DeferredSystemActionKind::None) {
            return DeferredSystemActionRequestStatus::InvalidAction;
        }
        uint8_t expected = static_cast<uint8_t>(
            DeferredSystemActionKind::None);
        const uint8_t desired = static_cast<uint8_t>(action);
        if (value_.compare_exchange_strong(
                expected, desired,
                std::memory_order_release,
                std::memory_order_acquire)) {
            return DeferredSystemActionRequestStatus::Accepted;
        }
        return expected == desired
            ? DeferredSystemActionRequestStatus::AlreadyRequested
            : DeferredSystemActionRequestStatus::Conflict;
    }

    DeferredSystemActionKind peek() const {
        return static_cast<DeferredSystemActionKind>(
            value_.load(std::memory_order_acquire));
    }

    bool try_consume(DeferredSystemActionKind& action) {
        uint8_t current = value_.load(std::memory_order_acquire);
        if (current == static_cast<uint8_t>(DeferredSystemActionKind::None)) {
            return false;
        }
        if (!value_.compare_exchange_strong(
                current,
                static_cast<uint8_t>(DeferredSystemActionKind::None),
                std::memory_order_acq_rel,
                std::memory_order_acquire)) {
            return false;
        }
        action = static_cast<DeferredSystemActionKind>(current);
        return true;
    }

private:
    std::atomic<uint8_t> value_{
        static_cast<uint8_t>(DeferredSystemActionKind::None)};
};

}  // namespace runtime
