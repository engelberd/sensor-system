#pragma once

#include <cstddef>
#include <cstdint>

namespace measurement {

enum class GapReason : uint8_t {
    UnknownDiscontinuity = 0,
    SourceLoss = 1,
    IngressOverflow = 2,
};

inline bool is_known_gap_reason(GapReason reason) {
    return reason == GapReason::UnknownDiscontinuity ||
           reason == GapReason::SourceLoss ||
           reason == GapReason::IngressOverflow;
}

struct GapRecord {
    uint64_t first_missing = 0;
    uint64_t missing_count = 0;
    GapReason reason = GapReason::UnknownDiscontinuity;

    bool valid() const {
        return missing_count != 0 &&
               first_missing <= UINT64_MAX - missing_count &&
               is_known_gap_reason(reason);
    }

    uint64_t end() const { return first_missing + missing_count; }
};

enum class GapAppendStatus : uint8_t {
    Ok = 0,
    Coalesced,
    InvalidGap,
    OutOfOrder,
    Full,
};

struct GapLookup {
    bool found = false;
    GapRecord gap{};
};

// Fixed-capacity metadata for exact missing ranges. This component does not
// own samples or decide retention collapse when it becomes full.
template <size_t Capacity>
class GapLedger {
    static_assert(Capacity > 0, "GapLedger capacity must be non-zero");

public:
    GapLedger() = default;
    GapLedger(const GapLedger&) = delete;
    GapLedger& operator=(const GapLedger&) = delete;
    GapLedger(GapLedger&&) = delete;
    GapLedger& operator=(GapLedger&&) = delete;

    static constexpr size_t capacity() { return Capacity; }
    static constexpr size_t storage_bytes() { return sizeof(GapRecord) * Capacity; }

    size_t size() const { return size_; }
    bool empty() const { return size_ == 0; }

    const GapRecord* oldest() const {
        return size_ == 0 ? nullptr : &records_[head_];
    }

    GapAppendStatus append(const GapRecord& gap) {
        if (!gap.valid()) return GapAppendStatus::InvalidGap;

        if (size_ != 0) {
            GapRecord& last = at(size_ - 1);
            if (gap.first_missing < last.end()) return GapAppendStatus::OutOfOrder;
            if (gap.first_missing == last.end() && gap.reason == last.reason) {
                if (last.missing_count > UINT64_MAX - gap.missing_count) {
                    return GapAppendStatus::InvalidGap;
                }
                last.missing_count += gap.missing_count;
                return GapAppendStatus::Coalesced;
            }
        }

        if (size_ == Capacity) return GapAppendStatus::Full;
        records_[physical_index(size_)] = gap;
        ++size_;
        return GapAppendStatus::Ok;
    }

    // Returns the remaining part of a gap when sequence points at its start or
    // into its interior. This makes recovery from an interior cursor exact.
    GapLookup containing(uint64_t sequence) const {
        for (size_t index = 0; index < size_; ++index) {
            const GapRecord& candidate = at(index);
            if (sequence < candidate.first_missing) break;
            if (sequence < candidate.end()) {
                return {
                    true,
                    {sequence, candidate.end() - sequence, candidate.reason},
                };
            }
        }
        return {};
    }

    // Returns the first gap whose end is after sequence. It may contain the
    // sequence or start later.
    GapLookup next_from(uint64_t sequence) const {
        for (size_t index = 0; index < size_; ++index) {
            const GapRecord& candidate = at(index);
            if (candidate.end() > sequence) return {true, candidate};
        }
        return {};
    }

    // Removes metadata before a retention cursor. A gap crossing the cursor is
    // trimmed so the remaining unavailable range stays exact.
    void discard_before(uint64_t cursor) {
        while (size_ != 0 && records_[head_].end() <= cursor) pop_front();
        if (size_ != 0 && records_[head_].first_missing < cursor) {
            GapRecord& first = records_[head_];
            first.missing_count = first.end() - cursor;
            first.first_missing = cursor;
        }
    }

private:
    size_t physical_index(size_t logical_index) const {
        return (head_ + logical_index) % Capacity;
    }

    GapRecord& at(size_t logical_index) {
        return records_[physical_index(logical_index)];
    }

    const GapRecord& at(size_t logical_index) const {
        return records_[physical_index(logical_index)];
    }

    void pop_front() {
        head_ = (head_ + 1) % Capacity;
        --size_;
    }

    GapRecord records_[Capacity]{};
    size_t head_ = 0;
    size_t size_ = 0;
};

}  // namespace measurement
