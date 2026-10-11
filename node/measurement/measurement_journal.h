#pragma once

#include <cstddef>
#include <cstdint>

#include "measurement/sample_record.h"
#include "measurement/stream_continuity.h"
#include "measurement/stream_gap.h"

namespace measurement {

struct MeasurementJournalState {
    uint64_t retained_from = 0;
    uint64_t produced = 0;
    uint64_t delivered = 0;
    uint64_t released = 0;
    uint64_t discarded_samples = 0;
    uint64_t discarded_sequences = 0;
    uint64_t retention_collapses = 0;
    size_t stored_samples = 0;
    size_t capacity_samples = 0;
    size_t stored_gaps = 0;
    size_t capacity_gaps = 0;
    size_t stored_continuity_breaks = 0;
    size_t capacity_continuity_breaks = 0;
};

enum class StreamAppendStatus : uint8_t {
    Ok = 0,
    InvalidSample,
    InvalidGap,
    InvalidContinuityBreak,
    NonContiguous,
    SequenceExhausted,
    InternalError,
};

struct StreamAppendResult {
    StreamAppendStatus status = StreamAppendStatus::InternalError;
    uint64_t first_sequence = 0;
    uint64_t end_sequence = 0;
    bool retention_collapsed = false;
};

enum class StreamReadStatus : uint8_t {
    Samples = 0,
    Gap,
    ContinuityBreak,
    NoData,
    RangeUnavailable,
    AheadOfProduced,
    InvalidLimit,
    InternalError,
};

struct MeasurementReadView {
    StreamReadStatus status = StreamReadStatus::NoData;
    uint64_t first_sequence = 0;
    uint64_t end_sequence = 0;
    uint64_t retained_from = 0;
    uint64_t produced = 0;
    SampleSpan first{};
    SampleSpan second{};
    GapRecord gap{};
    ContinuityBreakRecord continuity_break{};

    size_t sample_count() const { return first.size + second.size; }
};

enum class StreamDeliveryStatus : uint8_t {
    Ok = 0,
    BeyondPreparedRange,
};

enum class StreamReleaseStatus : uint8_t {
    Ok = 0,
    BeyondDeliveredRange,
};

// One logical cursor space over a compact sample ring and rare exact gaps.
// Sequence identity is supplied by SourceSequencer before this owner receives
// an event. No method allocates or synchronizes internally.
template <size_t SampleCapacity,
          size_t GapCapacity,
          size_t ContinuityCapacity = GapCapacity>
class MeasurementJournal {
    static_assert(SampleCapacity > 0, "sample capacity must be non-zero");
    static_assert(GapCapacity > 0, "gap capacity must be non-zero");
    static_assert(ContinuityCapacity > 0,
                  "continuity break capacity must be non-zero");

public:
    MeasurementJournal() = default;
    MeasurementJournal(const MeasurementJournal&) = delete;
    MeasurementJournal& operator=(const MeasurementJournal&) = delete;
    MeasurementJournal(MeasurementJournal&&) = delete;
    MeasurementJournal& operator=(MeasurementJournal&&) = delete;

    static constexpr size_t sample_capacity() { return SampleCapacity; }
    static constexpr size_t gap_capacity() { return GapCapacity; }
    static constexpr size_t continuity_capacity() { return ContinuityCapacity; }
    static constexpr size_t storage_bytes() {
        return sizeof(SampleRecord) * SampleCapacity +
               sizeof(GapRecord) * GapCapacity +
               sizeof(ContinuityBreakRecord) * ContinuityCapacity;
    }

    StreamAppendResult append_sample(
        uint64_t sequence, const SampleRecord& sample) {
        StreamAppendResult result{};
        result.first_sequence = sequence;
        result.end_sequence = sequence;
        if (sequence != produced_) {
            result.status = StreamAppendStatus::NonContiguous;
            return result;
        }
        if (produced_ == UINT64_MAX) {
            result.status = StreamAppendStatus::SequenceExhausted;
            return result;
        }
        if (!valid_sample_record(sample)) {
            result.status = StreamAppendStatus::InvalidSample;
            return result;
        }

        invalidate_prepared_view();
        if (sample_size_ == SampleCapacity) {
            const uint64_t oldest_sample = first_sample_sequence();
            if (oldest_sample == UINT64_MAX ||
                !discard_retention_before(oldest_sample + 1, true)) {
                result.status = StreamAppendStatus::InternalError;
                return result;
            }
            result.retention_collapsed = true;
        }

        samples_[sample_physical_index(sample_size_)] = sample;
        ++sample_size_;
        ++produced_;
        result.status = StreamAppendStatus::Ok;
        result.end_sequence = produced_;
        return result;
    }

    StreamAppendResult append_gap(const GapRecord& gap) {
        StreamAppendResult result{};
        result.first_sequence = gap.first_missing;
        result.end_sequence = gap.first_missing;
        if (!gap.valid()) {
            result.status = StreamAppendStatus::InvalidGap;
            return result;
        }
        if (gap.first_missing != produced_) {
            result.status = StreamAppendStatus::NonContiguous;
            return result;
        }

        invalidate_prepared_view();
        GapAppendStatus status = gaps_.append(gap);
        if (status == GapAppendStatus::Full) {
            const GapRecord* oldest = gaps_.oldest();
            if (oldest == nullptr ||
                !discard_retention_before(oldest->end(), true)) {
                result.status = StreamAppendStatus::InternalError;
                return result;
            }
            result.retention_collapsed = true;
            status = gaps_.append(gap);
        }
        if (status != GapAppendStatus::Ok &&
            status != GapAppendStatus::Coalesced) {
            result.status = status == GapAppendStatus::InvalidGap
                ? StreamAppendStatus::InvalidGap
                : status == GapAppendStatus::OutOfOrder
                    ? StreamAppendStatus::NonContiguous
                    : StreamAppendStatus::InternalError;
            return result;
        }

        produced_ = gap.end();
        result.status = StreamAppendStatus::Ok;
        result.end_sequence = produced_;
        return result;
    }

    StreamAppendResult append_continuity_break(
        const ContinuityBreakRecord& value) {
        StreamAppendResult result{};
        result.first_sequence = value.at_sequence;
        result.end_sequence = value.at_sequence;
        if (!value.valid()) {
            result.status = StreamAppendStatus::InvalidContinuityBreak;
            return result;
        }
        if (value.at_sequence != produced_) {
            result.status = StreamAppendStatus::NonContiguous;
            return result;
        }

        invalidate_prepared_view();
        ContinuityAppendStatus status = continuity_.append(value);
        if (status == ContinuityAppendStatus::Full) {
            const ContinuityBreakRecord* oldest = continuity_.oldest();
            if (oldest == nullptr ||
                !discard_retention_before(oldest->end(), true)) {
                result.status = StreamAppendStatus::InternalError;
                return result;
            }
            result.retention_collapsed = true;
            status = continuity_.append(value);
        }
        if (status != ContinuityAppendStatus::Ok) {
            result.status = status == ContinuityAppendStatus::InvalidBreak
                ? StreamAppendStatus::InvalidContinuityBreak
                : status == ContinuityAppendStatus::OutOfOrder
                    ? StreamAppendStatus::NonContiguous
                    : StreamAppendStatus::InternalError;
            return result;
        }

        produced_ = value.end();
        result.status = StreamAppendStatus::Ok;
        result.end_sequence = produced_;
        return result;
    }

    MeasurementReadView prepare_read(uint64_t from, size_t sample_limit) {
        invalidate_prepared_view();
        MeasurementReadView result{};
        result.first_sequence = from;
        result.end_sequence = from;
        result.retained_from = retained_from_;
        result.produced = produced_;

        if (sample_limit == 0) {
            result.status = StreamReadStatus::InvalidLimit;
            return result;
        }
        if (from < retained_from_) {
            result.status = StreamReadStatus::RangeUnavailable;
            return result;
        }
        if (from > produced_) {
            result.status = StreamReadStatus::AheadOfProduced;
            return result;
        }
        if (from == produced_) {
            result.status = StreamReadStatus::NoData;
            return result;
        }

        const GapLookup containing = gaps_.containing(from);
        if (containing.found) {
            result.status = StreamReadStatus::Gap;
            result.gap = containing.gap;
            result.end_sequence = containing.gap.end();
            prepare_through(result.end_sequence);
            return result;
        }

        const ContinuityLookup break_at = continuity_.containing(from);
        if (break_at.found) {
            result.status = StreamReadStatus::ContinuityBreak;
            result.continuity_break = break_at.value;
            result.end_sequence = break_at.value.end();
            prepare_through(result.end_sequence);
            return result;
        }

        uint64_t sample_end = produced_;
        const GapLookup next = gaps_.next_from(from);
        if (next.found && next.gap.first_missing < sample_end) {
            sample_end = next.gap.first_missing;
        }
        const ContinuityLookup next_break = continuity_.next_from(from);
        if (next_break.found &&
            next_break.value.at_sequence < sample_end) {
            sample_end = next_break.value.at_sequence;
        }
        const uint64_t available = sample_end - from;
        size_t count = available < sample_limit
            ? static_cast<size_t>(available)
            : sample_limit;
        const size_t logical_offset = samples_between(retained_from_, from);
        if (count == 0 || logical_offset >= sample_size_ ||
            count > sample_size_ - logical_offset) {
            result.status = StreamReadStatus::InternalError;
            return result;
        }

        const SampleRecord& boundary =
            samples_[sample_physical_index(logical_offset)];
        size_t bounded_count = 1;
        while (bounded_count < count) {
            const SampleRecord& candidate =
                samples_[sample_physical_index(logical_offset + bounded_count)];
            if (candidate.config_revision != boundary.config_revision ||
                candidate.timing_segment_id != boundary.timing_segment_id) {
                break;
            }
            ++bounded_count;
        }
        count = bounded_count;

        const size_t start = sample_physical_index(logical_offset);
        const size_t first_size = count < (SampleCapacity - start)
            ? count
            : (SampleCapacity - start);
        result.status = StreamReadStatus::Samples;
        result.end_sequence = from + count;
        result.first = {&samples_[start], first_size};
        result.second = {samples_, count - first_size};
        prepare_through(result.end_sequence);
        return result;
    }

    StreamDeliveryStatus mark_delivered(uint64_t end_sequence) {
        if (!prepared_valid_ || end_sequence != prepared_through_) {
            return StreamDeliveryStatus::BeyondPreparedRange;
        }
        if (end_sequence > delivered_) delivered_ = end_sequence;
        invalidate_prepared_view();
        return StreamDeliveryStatus::Ok;
    }

    void cancel_prepared() { invalidate_prepared_view(); }

    StreamReleaseStatus release_before(uint64_t cursor) {
        invalidate_prepared_view();
        if (cursor <= released_) return StreamReleaseStatus::Ok;
        const uint64_t maximum = delivered_ > retained_from_
            ? delivered_
            : retained_from_;
        if (cursor > maximum) return StreamReleaseStatus::BeyondDeliveredRange;
        if (cursor > retained_from_ &&
            !discard_retention_before(cursor, false)) {
            return StreamReleaseStatus::BeyondDeliveredRange;
        }
        released_ = cursor;
        return StreamReleaseStatus::Ok;
    }

    MeasurementJournalState state() const {
        return {
            retained_from_, produced_, delivered_, released_,
            discarded_samples_, discarded_sequences_, retention_collapses_,
            sample_size_, SampleCapacity, gaps_.size(), GapCapacity,
            continuity_.size(), ContinuityCapacity,
        };
    }

private:
    size_t sample_physical_index(size_t logical_offset) const {
        return (sample_head_ + logical_offset) % SampleCapacity;
    }

    size_t samples_between(uint64_t begin, uint64_t end) const {
        if (end <= begin) return 0;
        uint64_t missing = 0;
        uint64_t cursor = begin;
        while (cursor < end) {
            const GapLookup next = gaps_.next_from(cursor);
            if (!next.found || next.gap.first_missing >= end) break;
            const uint64_t gap_begin = next.gap.first_missing > cursor
                ? next.gap.first_missing
                : cursor;
            const uint64_t gap_end = next.gap.end() < end
                ? next.gap.end()
                : end;
            if (gap_end > gap_begin) missing += gap_end - gap_begin;
            cursor = next.gap.end();
        }
        uint64_t breaks = 0;
        uint64_t break_cursor = begin;
        while (break_cursor < end) {
            const ContinuityLookup next = continuity_.next_from(break_cursor);
            if (!next.found || next.value.at_sequence >= end) break;
            ++breaks;
            break_cursor = next.value.end();
        }
        return static_cast<size_t>((end - begin) - missing - breaks);
    }

    uint64_t first_sample_sequence() const {
        uint64_t cursor = retained_from_;
        while (cursor < produced_) {
            const GapLookup gap = gaps_.containing(cursor);
            if (gap.found) {
                cursor = gap.gap.end();
                continue;
            }
            const ContinuityLookup break_at = continuity_.containing(cursor);
            if (break_at.found) {
                cursor = break_at.value.end();
                continue;
            }
            return cursor;
        }
        return UINT64_MAX;
    }

    bool discard_retention_before(uint64_t cursor, bool is_loss) {
        if (cursor < retained_from_ || cursor > produced_) return false;
        const uint64_t old_retained = retained_from_;
        const size_t remove = samples_between(retained_from_, cursor);
        if (remove > sample_size_) return false;
        sample_head_ = (sample_head_ + remove) % SampleCapacity;
        sample_size_ -= remove;
        gaps_.discard_before(cursor);
        continuity_.discard_before(cursor);
        retained_from_ = cursor;
        if (is_loss) {
            discarded_samples_ += remove;
            discarded_sequences_ += cursor - old_retained;
            ++retention_collapses_;
        }
        return true;
    }

    void prepare_through(uint64_t cursor) {
        prepared_valid_ = true;
        prepared_through_ = cursor;
    }

    void invalidate_prepared_view() {
        prepared_valid_ = false;
        prepared_through_ = 0;
    }

    SampleRecord samples_[SampleCapacity]{};
    GapLedger<GapCapacity> gaps_{};
    ContinuityLedger<ContinuityCapacity> continuity_{};
    size_t sample_head_ = 0;
    size_t sample_size_ = 0;
    uint64_t retained_from_ = 0;
    uint64_t produced_ = 0;
    uint64_t delivered_ = 0;
    uint64_t released_ = 0;
    uint64_t discarded_samples_ = 0;
    uint64_t discarded_sequences_ = 0;
    uint64_t retention_collapses_ = 0;
    bool prepared_valid_ = false;
    uint64_t prepared_through_ = 0;
};

}  // namespace measurement
