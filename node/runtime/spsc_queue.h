#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>

namespace runtime {

// Bounded single-producer/single-consumer queue. Producer and consumer own
// separate monotonic cursors; release/acquire publishes initialized slots.
template <typename T, size_t Capacity>
class SpscQueue {
    static_assert(Capacity > 0, "SPSC capacity must be non-zero");
    static_assert(Capacity <= UINT32_MAX / 2u,
                  "SPSC capacity is too large for cursor arithmetic");
    static_assert(std::atomic<uint32_t>::is_always_lock_free,
                  "SPSC cursors must be lock-free on the target");

public:
    SpscQueue() = default;
    SpscQueue(const SpscQueue&) = delete;
    SpscQueue& operator=(const SpscQueue&) = delete;
    SpscQueue(SpscQueue&&) = delete;
    SpscQueue& operator=(SpscQueue&&) = delete;

    static constexpr size_t capacity() { return Capacity; }

    bool try_push(const T& value) {
        const uint32_t write = write_.load(std::memory_order_relaxed);
        const uint32_t read = read_.load(std::memory_order_acquire);
        if (write - read >= Capacity) return false;
        slots_[write % Capacity] = value;
        write_.store(write + 1u, std::memory_order_release);
        return true;
    }

    bool try_pop(T& output) {
        const T* value = nullptr;
        if (!try_front(value)) return false;
        output = *value;
        return pop_front();
    }

    bool try_front(const T*& output) const {
        const uint32_t read = read_.load(std::memory_order_relaxed);
        const uint32_t write = write_.load(std::memory_order_acquire);
        if (read == write) {
            output = nullptr;
            return false;
        }
        output = &slots_[read % Capacity];
        return true;
    }

    bool pop_front() {
        const uint32_t read = read_.load(std::memory_order_relaxed);
        const uint32_t write = write_.load(std::memory_order_acquire);
        if (read == write) return false;
        read_.store(read + 1u, std::memory_order_release);
        return true;
    }

    size_t size_approx() const {
        const uint32_t write = write_.load(std::memory_order_acquire);
        const uint32_t read = read_.load(std::memory_order_acquire);
        const uint32_t used = write - read;
        return used > Capacity ? Capacity : static_cast<size_t>(used);
    }

    bool empty_approx() const { return size_approx() == 0; }

private:
    T slots_[Capacity]{};
    std::atomic<uint32_t> write_{0};
    std::atomic<uint32_t> read_{0};
};

}  // namespace runtime
