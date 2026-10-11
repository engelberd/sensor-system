#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>

#include "interfaces/i_sample_sink.h"

struct ObserverSnapshotStats {
    uint32_t published_batches = 0;
    uint32_t truncated_batches = 0;
    uint32_t read_retries = 0;
};

template <size_t Capacity>
struct ObserverSampleSnapshot {
    static_assert(Capacity > 0, "Observer snapshot capacity must be positive");

    uint32_t generation = 0;
    size_t sample_count = 0;
    StoredSample samples[Capacity]{};
    SampleDeviceTime times[Capacity]{};
};

// Single-producer, single-consumer publication of the latest complete sample
// batch. The producer never waits for the observer and the observer never
// changes acquisition or delivery state.
template <size_t Capacity>
class ObserverSnapshotSink final : public ISampleSink {
public:
    static_assert(Capacity > 0, "Observer snapshot capacity must be positive");

    using Snapshot = ObserverSampleSnapshot<Capacity>;

    void on_samples(const StoredSample* samples, size_t count) override {
        publish(samples, nullptr, count);
    }

    void on_timed_samples(const StoredSample* samples,
                          const SampleDeviceTime* times,
                          size_t count) override {
        publish(samples, times, count);
    }

    bool read_latest(Snapshot& destination) {
        if (!has_snapshot_.load(std::memory_order_acquire)) {
            return false;
        }

        while (true) {
            const uint8_t index = published_index_.load(std::memory_order_acquire);
            reading_index_.store(index, std::memory_order_seq_cst);

            if (published_index_.load(std::memory_order_acquire) != index) {
                reading_index_.store(kNoReader, std::memory_order_release);
                read_retries_.fetch_add(1, std::memory_order_relaxed);
                continue;
            }

            copy_snapshot(buffers_[index], destination);
            reading_index_.store(kNoReader, std::memory_order_release);
            return true;
        }
    }

    ObserverSnapshotStats stats() const {
        ObserverSnapshotStats result{};
        result.published_batches = published_batches_.load(std::memory_order_relaxed);
        result.truncated_batches = truncated_batches_.load(std::memory_order_relaxed);
        result.read_retries = read_retries_.load(std::memory_order_relaxed);
        return result;
    }

private:
    static constexpr uint8_t kBufferCount = 3;
    static constexpr uint8_t kNoReader = 0xFF;

    static void copy_snapshot(const Snapshot& source, Snapshot& destination) {
        destination.generation = source.generation;
        destination.sample_count = source.sample_count;
        for (size_t i = 0; i < source.sample_count; ++i) {
            destination.samples[i] = source.samples[i];
            destination.times[i] = source.times[i];
        }
    }

    uint8_t select_write_buffer() const {
        const uint8_t published = published_index_.load(std::memory_order_acquire);
        const uint8_t reading = reading_index_.load(std::memory_order_seq_cst);

        for (uint8_t index = 0; index < kBufferCount; ++index) {
            if (index != published && index != reading) {
                return index;
            }
        }

        // Three buffers guarantee a free slot for one producer and one reader.
        return static_cast<uint8_t>((published + 1u) % kBufferCount);
    }

    void publish(const StoredSample* samples,
                 const SampleDeviceTime* times,
                 size_t count) {
        if (samples == nullptr || count == 0) {
            return;
        }

        size_t first = 0;
        size_t stored_count = count;
        if (stored_count > Capacity) {
            first = stored_count - Capacity;
            stored_count = Capacity;
            truncated_batches_.fetch_add(1, std::memory_order_relaxed);
        }

        const uint8_t write_index = select_write_buffer();
        Snapshot& target = buffers_[write_index];
        target.generation = ++next_generation_;
        target.sample_count = stored_count;

        for (size_t i = 0; i < stored_count; ++i) {
            target.samples[i] = samples[first + i];
            target.times[i] = times != nullptr
                ? times[first + i]
                : SampleDeviceTime{};
        }

        published_index_.store(write_index, std::memory_order_release);
        has_snapshot_.store(true, std::memory_order_release);
        published_batches_.fetch_add(1, std::memory_order_relaxed);
    }

    Snapshot buffers_[kBufferCount]{};
    std::atomic<uint8_t> published_index_{0};
    std::atomic<uint8_t> reading_index_{kNoReader};
    std::atomic<bool> has_snapshot_{false};
    std::atomic<uint32_t> published_batches_{0};
    std::atomic<uint32_t> truncated_batches_{0};
    std::atomic<uint32_t> read_retries_{0};
    uint32_t next_generation_ = 0;
};
