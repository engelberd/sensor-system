#pragma once

#include <cstddef>
#include <cstdint>

#include "measurement/measurement_journal.h"
#include "measurement/sample_timing.h"

namespace measurement {

struct PullCommand {
    uint64_t boot_epoch = 0;
    uint64_t from = 0;
    uint64_t release_before = 0;
    uint16_t max_samples = 0;
};

struct ReleaseCommand {
    uint64_t boot_epoch = 0;
    uint64_t release_before = 0;
};

enum class PullDecision : uint8_t {
    SampleChunk = 0,
    Gap,
    ContinuityBreak,
    NoData,
    RangeUnavailable,
    StreamChanged,
    InvalidRelease,
    CursorAhead,
    InvalidRequest,
    Busy,
    InternalError,
};

struct PreparedChunk {
    uint64_t token = 0;
    uint64_t first_sequence = 0;
    uint64_t end_sequence = 0;
    uint32_t config_revision = 0;
    uint32_t timing_segment_id = 0;
    TimingSummary timing{};
    MeasurementReadView samples{};
};

struct PreparedGap {
    uint64_t token = 0;
    GapRecord gap{};
    uint64_t retained_from = 0;
    uint64_t produced = 0;
};

struct PreparedContinuityBreak {
    uint64_t token = 0;
    ContinuityBreakRecord value{};
    uint64_t retained_from = 0;
    uint64_t produced = 0;
};

struct PullPlan {
    PullDecision decision = PullDecision::InternalError;
    MeasurementJournalState state{};
    uint64_t actual_boot_epoch = 0;
    PreparedChunk chunk{};
    PreparedGap gap{};
    PreparedContinuityBreak continuity_break{};
};

enum class ReleaseDecision : uint8_t {
    Applied = 0,
    StreamChanged,
    InvalidRelease,
    Busy,
    InternalError,
};

struct ReleaseResult {
    ReleaseDecision decision = ReleaseDecision::InvalidRelease;
    MeasurementJournalState state{};
    uint64_t actual_boot_epoch = 0;
};

enum class CompletionStatus : uint8_t {
    Ok = 0,
    NoPreparedChunk,
    TokenMismatch,
    JournalChanged,
};

// Owns the rules for exposing one MeasurementJournal through Measurement Service.
// It does not decode bytes, allocate memory, schedule nodes or drive a UART.
template <size_t Capacity, size_t MaxChunkSamples, size_t GapCapacity = 8>
class DeliverySession {
    static_assert(Capacity > 0, "journal capacity must be non-zero");
    static_assert(Capacity <= UINT32_MAX, "journal capacity does not fit wire state");
    static_assert(MaxChunkSamples > 0, "chunk limit must be non-zero");
    static_assert(MaxChunkSamples <= UINT16_MAX, "chunk limit does not fit request");

public:
    DeliverySession(
        uint64_t boot_epoch,
        MeasurementJournal<Capacity, GapCapacity>& journal)
        : boot_epoch_(boot_epoch), journal_(journal) {}
    ~DeliverySession() {
        if (prepared_token_ != 0) journal_.cancel_prepared();
    }

    DeliverySession(const DeliverySession&) = delete;
    DeliverySession& operator=(const DeliverySession&) = delete;
    DeliverySession(DeliverySession&&) = delete;
    DeliverySession& operator=(DeliverySession&&) = delete;

    uint64_t boot_epoch() const { return boot_epoch_; }
    bool has_prepared_chunk() const { return prepared_token_ != 0; }
    MeasurementJournalState state() const { return journal_.state(); }

    PullPlan prepare_pull(const PullCommand& command) {
        PullPlan plan{};
        plan.actual_boot_epoch = boot_epoch_;
        plan.state = journal_.state();

        if (prepared_token_ != 0) {
            plan.decision = PullDecision::Busy;
            return plan;
        }
        if (boot_epoch_ == 0) {
            plan.decision = PullDecision::InternalError;
            return plan;
        }
        if (command.boot_epoch != boot_epoch_) {
            plan.decision = PullDecision::StreamChanged;
            return plan;
        }
        if (command.max_samples == 0) {
            plan.decision = PullDecision::InvalidRequest;
            return plan;
        }
        if (command.release_before > command.from) {
            plan.decision = PullDecision::InvalidRelease;
            return plan;
        }
        if (journal_.release_before(command.release_before) !=
            StreamReleaseStatus::Ok) {
            plan.state = journal_.state();
            plan.decision = PullDecision::InvalidRelease;
            return plan;
        }

        const size_t requested = command.max_samples < MaxChunkSamples
            ? command.max_samples
            : MaxChunkSamples;
        const MeasurementReadView view =
            journal_.prepare_read(command.from, requested);
        plan.state = journal_.state();

        if (view.status == StreamReadStatus::NoData) {
            plan.decision = PullDecision::NoData;
            return plan;
        }
        if (view.status == StreamReadStatus::RangeUnavailable) {
            plan.decision = PullDecision::RangeUnavailable;
            return plan;
        }
        if (view.status == StreamReadStatus::AheadOfProduced) {
            plan.decision = PullDecision::CursorAhead;
            return plan;
        }
        if (next_token_ == 0) {
            journal_.cancel_prepared();
            plan.decision = PullDecision::InternalError;
            return plan;
        }

        if (view.status == StreamReadStatus::Gap) {
            prepared_token_ = next_token_++;
            prepared_end_ = view.end_sequence;
            plan.decision = PullDecision::Gap;
            plan.gap.token = prepared_token_;
            plan.gap.gap = view.gap;
            plan.gap.retained_from = view.retained_from;
            plan.gap.produced = view.produced;
            return plan;
        }
        if (view.status == StreamReadStatus::ContinuityBreak) {
            prepared_token_ = next_token_++;
            prepared_end_ = view.end_sequence;
            plan.decision = PullDecision::ContinuityBreak;
            plan.continuity_break.token = prepared_token_;
            plan.continuity_break.value = view.continuity_break;
            plan.continuity_break.retained_from = view.retained_from;
            plan.continuity_break.produced = view.produced;
            return plan;
        }
        if (view.status != StreamReadStatus::Samples) {
            journal_.cancel_prepared();
            plan.decision = PullDecision::InternalError;
            return plan;
        }

        TimingSummary timing{};
        if (summarize_timing(view, timing) != TimingSummaryStatus::Ok) {
            journal_.cancel_prepared();
            plan.decision = PullDecision::InternalError;
            return plan;
        }

        const SampleRecord& first = sample_at(view, 0);
        prepared_token_ = next_token_++;
        prepared_end_ = view.end_sequence;
        plan.decision = PullDecision::SampleChunk;
        plan.chunk.token = prepared_token_;
        plan.chunk.first_sequence = view.first_sequence;
        plan.chunk.end_sequence = view.end_sequence;
        plan.chunk.config_revision = first.config_revision;
        plan.chunk.timing_segment_id = first.timing_segment_id;
        plan.chunk.timing = timing;
        plan.chunk.samples = view;
        return plan;
    }

    CompletionStatus complete(uint64_t token) {
        if (prepared_token_ == 0) return CompletionStatus::NoPreparedChunk;
        if (token != prepared_token_) return CompletionStatus::TokenMismatch;

        const StreamDeliveryStatus status =
            journal_.mark_delivered(prepared_end_);
        clear_prepared_state();
        return status == StreamDeliveryStatus::Ok
            ? CompletionStatus::Ok
            : CompletionStatus::JournalChanged;
    }

    CompletionStatus abort(uint64_t token) {
        if (prepared_token_ == 0) return CompletionStatus::NoPreparedChunk;
        if (token != prepared_token_) return CompletionStatus::TokenMismatch;
        journal_.cancel_prepared();
        clear_prepared_state();
        return CompletionStatus::Ok;
    }

    ReleaseResult release(const ReleaseCommand& command) {
        ReleaseResult result{};
        result.actual_boot_epoch = boot_epoch_;
        result.state = journal_.state();
        if (prepared_token_ != 0) {
            result.decision = ReleaseDecision::Busy;
            return result;
        }
        if (boot_epoch_ == 0) {
            result.decision = ReleaseDecision::InternalError;
            return result;
        }
        if (command.boot_epoch != boot_epoch_) {
            result.decision = ReleaseDecision::StreamChanged;
            return result;
        }
        if (journal_.release_before(command.release_before) !=
            StreamReleaseStatus::Ok) {
            result.decision = ReleaseDecision::InvalidRelease;
            return result;
        }
        result.decision = ReleaseDecision::Applied;
        result.state = journal_.state();
        return result;
    }

private:
    void clear_prepared_state() {
        prepared_token_ = 0;
        prepared_end_ = 0;
    }

    uint64_t boot_epoch_ = 0;
    MeasurementJournal<Capacity, GapCapacity>& journal_;
    uint64_t next_token_ = 1;
    uint64_t prepared_token_ = 0;
    uint64_t prepared_end_ = 0;
};

}  // namespace measurement
