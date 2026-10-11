#pragma once

#include <cstdint>

#include "measurement/stream_continuity.h"
#include "measurement/stream_gap.h"

namespace measurement {

enum class SequenceClaimStatus : uint8_t {
    Ok = 0,
    InvalidCount,
    InvalidReason,
    Exhausted,
};

struct SequenceClaim {
    SequenceClaimStatus status = SequenceClaimStatus::Exhausted;
    uint64_t first = 0;
    uint64_t count = 0;
};

// Single-owner source cursor. It assigns identity before an event crosses a
// lossy boundary and does not depend on a concrete sensor payload.
class SourceSequencer {
public:
    explicit SourceSequencer(uint64_t initial_cursor = 0)
        : next_(initial_cursor) {}

    uint64_t cursor() const { return next_; }

    SequenceClaim claim_sample() { return claim(1); }

    SequenceClaim claim(uint64_t count) {
        if (count == 0) return {SequenceClaimStatus::InvalidCount, next_, 0};
        if (next_ > UINT64_MAX - count) {
            return {SequenceClaimStatus::Exhausted, next_, 0};
        }
        const uint64_t first = next_;
        next_ += count;
        return {SequenceClaimStatus::Ok, first, count};
    }

    SequenceClaim claim_gap(
        uint64_t count, GapReason reason, GapRecord& output) {
        if (!is_known_gap_reason(reason)) {
            return {SequenceClaimStatus::InvalidReason, next_, 0};
        }
        const SequenceClaim result = claim(count);
        if (result.status == SequenceClaimStatus::Ok) {
            output = {result.first, result.count, reason};
        }
        return result;
    }

    SequenceClaim claim_continuity_break(
        ContinuityBreakReason reason, ContinuityBreakRecord& output) {
        if (!is_known_continuity_break_reason(reason)) {
            return {SequenceClaimStatus::InvalidReason, next_, 0};
        }
        const SequenceClaim result = claim(1);
        if (result.status == SequenceClaimStatus::Ok) {
            output = {result.first, reason};
        }
        return result;
    }

private:
    uint64_t next_ = 0;
};

}  // namespace measurement
