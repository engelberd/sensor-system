#pragma once

#include <array>
#include <atomic>
#include <cstddef>
#include <cstdint>

#include "timing/sample_timing.h"

template <size_t Capacity>
class DrdyTimestampRing {
public:
    static_assert(Capacity > 0, "DRDY timestamp ring must not be empty");
    static_assert(Capacity <= UINT32_MAX / 2u,
                  "DRDY timestamp ring capacity is too large");

    bool push_from_isr(const DrdyTimestamp& value) {
        const uint32_t write = write_count_.load(std::memory_order_relaxed);
        const uint32_t read = read_count_.load(std::memory_order_acquire);
        if (static_cast<uint32_t>(write - read) >= Capacity) {
            overflow_latched_.store(true, std::memory_order_release);
            overflow_count_.fetch_add(1, std::memory_order_relaxed);
            return false;
        }

        entries_[write % Capacity] = value;
        write_count_.store(write + 1u, std::memory_order_release);
        return true;
    }

    bool pop(DrdyTimestamp& value) {
        const uint32_t read = read_count_.load(std::memory_order_relaxed);
        const uint32_t write = write_count_.load(std::memory_order_acquire);
        if (read == write) {
            return false;
        }

        value = entries_[read % Capacity];
        read_count_.store(read + 1u, std::memory_order_release);
        return true;
    }

    size_t size() const {
        const uint32_t write = write_count_.load(std::memory_order_acquire);
        const uint32_t read = read_count_.load(std::memory_order_acquire);
        const uint32_t used = write - read;
        return used > Capacity ? Capacity : static_cast<size_t>(used);
    }

    constexpr size_t capacity() const {
        return Capacity;
    }

    bool overflow_latched() const {
        return overflow_latched_.load(std::memory_order_acquire);
    }

    uint32_t overflow_count() const {
        return overflow_count_.load(std::memory_order_acquire);
    }

    bool consume_overflow_latched() {
        return overflow_latched_.exchange(false, std::memory_order_acq_rel);
    }

    // The producer IRQ must be disarmed before reset().
    void reset() {
        read_count_.store(0, std::memory_order_relaxed);
        write_count_.store(0, std::memory_order_relaxed);
        overflow_latched_.store(false, std::memory_order_relaxed);
        overflow_count_.store(0, std::memory_order_relaxed);
    }

private:
    std::array<DrdyTimestamp, Capacity> entries_{};
    std::atomic<uint32_t> read_count_{0};
    std::atomic<uint32_t> write_count_{0};
    std::atomic<uint32_t> overflow_count_{0};
    std::atomic<bool> overflow_latched_{false};
};
