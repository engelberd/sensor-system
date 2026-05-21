#pragma once

#include <array>
#include <cstddef>
#include <cstdint>

#include "common/node_types.h"
#include "pico/critical_section.h"
#include "storage/stored_sample.h"

template <size_t Capacity>
class AcquisitionBuffer {
public:
    AcquisitionBuffer() {
        critical_section_init(&cs_);
    }

    bool push_sample(const StoredSample& sample) {
        critical_section_enter_blocking(&cs_);

        bool overwritten_oldest = false;
        buffer_[write_index_] = sample;
        write_index_ = (write_index_ + 1) % Capacity;

        if (stored_samples_ < Capacity) {
            ++stored_samples_;
        } else {
            overwritten_oldest = true;
            ++overwrite_count_;
        }

        critical_section_exit(&cs_);
        return overwritten_oldest;
    }

    size_t copy_latest(StoredSample* out, size_t max_count) const {
        if (out == nullptr || max_count == 0) {
            return 0;
        }

        critical_section_enter_blocking(const_cast<critical_section_t*>(&cs_));

        const size_t count = (stored_samples_ < max_count) ? stored_samples_ : max_count;
        if (count == 0) {
            critical_section_exit(&cs_);
            return 0;
        }

        const size_t first_logical = stored_samples_ - count;
        for (size_t i = 0; i < count; ++i) {
            out[i] = sample_at_logical_index_unlocked(first_logical + i);
        }

        critical_section_exit(&cs_);
        return count;
    }

    size_t copy_from_seq(uint64_t start_seq, StoredSample* out, size_t max_count) const {
        if (out == nullptr || max_count == 0) {
            return 0;
        }

        critical_section_enter_blocking(const_cast<critical_section_t*>(&cs_));

        if (stored_samples_ == 0) {
            critical_section_exit(&cs_);
            return 0;
        }

        size_t copied = 0;
        for (size_t i = 0; i < stored_samples_ && copied < max_count; ++i) {
            const StoredSample s = sample_at_logical_index_unlocked(i);
            if (s.sample_seq >= start_seq) {
                out[copied++] = s;
            }
        }

        critical_section_exit(&cs_);
        return copied;
    }

    BufferState state() const {
        BufferState st{};

        critical_section_enter_blocking(const_cast<critical_section_t*>(&cs_));

        st.capacity_samples = Capacity;
        st.stored_samples = stored_samples_;
        st.overwrite_count = overwrite_count_;

        if (stored_samples_ > 0) {
            const StoredSample oldest = sample_at_logical_index_unlocked(0);
            const StoredSample newest = sample_at_logical_index_unlocked(stored_samples_ - 1);

            st.oldest_seq = oldest.sample_seq;
            st.newest_seq = newest.sample_seq;
        } else {
            st.oldest_seq = 0;
            st.newest_seq = 0;
        }

        critical_section_exit(&cs_);
        return st;
    }

    void clear() {
        critical_section_enter_blocking(&cs_);

        write_index_ = 0;
        stored_samples_ = 0;
        overwrite_count_ = 0;

        critical_section_exit(&cs_);
    }

private:
    StoredSample sample_at_logical_index_unlocked(size_t logical_index) const {
        size_t oldest_physical = 0;

        if (stored_samples_ == Capacity) {
            oldest_physical = write_index_;
        }

        const size_t physical_index = (oldest_physical + logical_index) % Capacity;
        return buffer_[physical_index];
    }

private:
    mutable critical_section_t cs_{};
    std::array<StoredSample, Capacity> buffer_{};

    size_t write_index_ = 0;
    size_t stored_samples_ = 0;
    uint32_t overwrite_count_ = 0;
};