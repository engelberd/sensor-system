#pragma once

#include <cstddef>
#include <cstdint>

namespace measurement {

enum class ContinuityBreakReason : uint8_t {
    Unknown = 0,
    SensorOverflow = 1,
    SensorReadUncertain = 2,
    InvalidTiming = 3,
    ProcessingReset = 4,
};

inline bool is_known_continuity_break_reason(ContinuityBreakReason reason) {
    return reason == ContinuityBreakReason::Unknown ||
           reason == ContinuityBreakReason::SensorOverflow ||
           reason == ContinuityBreakReason::SensorReadUncertain ||
           reason == ContinuityBreakReason::InvalidTiming ||
           reason == ContinuityBreakReason::ProcessingReset;
}

// Occupies one logical cursor position but does not claim that exactly one
// physical sample was lost. The following sample starts a new segment.
struct ContinuityBreakRecord {
    uint64_t at_sequence = 0;
    ContinuityBreakReason reason = ContinuityBreakReason::Unknown;

    bool valid() const {
        return at_sequence != UINT64_MAX &&
               is_known_continuity_break_reason(reason);
    }

    uint64_t end() const { return at_sequence + 1u; }
};

enum class ContinuityAppendStatus : uint8_t {
    Ok = 0,
    InvalidBreak,
    OutOfOrder,
    Full,
};

struct ContinuityLookup {
    bool found = false;
    ContinuityBreakRecord value{};
};

template <size_t Capacity>
class ContinuityLedger {
    static_assert(Capacity > 0, "continuity ledger capacity must be non-zero");

public:
    static constexpr size_t capacity() { return Capacity; }
    static constexpr size_t storage_bytes() {
        return sizeof(ContinuityBreakRecord) * Capacity;
    }

    size_t size() const { return size_; }

    const ContinuityBreakRecord* oldest() const {
        return size_ == 0 ? nullptr : &records_[head_];
    }

    ContinuityAppendStatus append(const ContinuityBreakRecord& value) {
        if (!value.valid()) return ContinuityAppendStatus::InvalidBreak;
        if (size_ != 0 && value.at_sequence < at(size_ - 1).end()) {
            return ContinuityAppendStatus::OutOfOrder;
        }
        if (size_ == Capacity) return ContinuityAppendStatus::Full;
        records_[physical_index(size_)] = value;
        ++size_;
        return ContinuityAppendStatus::Ok;
    }

    ContinuityLookup containing(uint64_t sequence) const {
        for (size_t index = 0; index < size_; ++index) {
            const ContinuityBreakRecord& candidate = at(index);
            if (sequence < candidate.at_sequence) break;
            if (sequence == candidate.at_sequence) return {true, candidate};
        }
        return {};
    }

    ContinuityLookup next_from(uint64_t sequence) const {
        for (size_t index = 0; index < size_; ++index) {
            const ContinuityBreakRecord& candidate = at(index);
            if (candidate.at_sequence >= sequence) return {true, candidate};
        }
        return {};
    }

    void discard_before(uint64_t cursor) {
        while (size_ != 0 && records_[head_].end() <= cursor) pop_front();
    }

private:
    size_t physical_index(size_t logical_index) const {
        return (head_ + logical_index) % Capacity;
    }

    ContinuityBreakRecord& at(size_t logical_index) {
        return records_[physical_index(logical_index)];
    }

    const ContinuityBreakRecord& at(size_t logical_index) const {
        return records_[physical_index(logical_index)];
    }

    void pop_front() {
        head_ = (head_ + 1) % Capacity;
        --size_;
    }

    ContinuityBreakRecord records_[Capacity]{};
    size_t head_ = 0;
    size_t size_ = 0;
};

}  // namespace measurement
