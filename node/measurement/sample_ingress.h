#pragma once

#include <cstddef>
#include <cstdint>

#include "measurement/measurement_journal.h"
#include "measurement/sample_record.h"
#include "measurement/source_sequencer.h"
#include "measurement/stream_gap.h"
#include "runtime/spsc_queue.h"

namespace measurement {

enum class IngressEventKind : uint8_t {
    Sample = 0,
    Gap,
    ContinuityBreak,
};

struct IngressEvent {
    SampleRecord sample{};
    uint64_t first_sequence = 0;
    uint64_t missing_count = 0;
    IngressEventKind kind = IngressEventKind::Sample;
    GapReason gap_reason = GapReason::UnknownDiscontinuity;
    ContinuityBreakReason continuity_reason = ContinuityBreakReason::Unknown;

    static IngressEvent sample_event(
        uint64_t sequence, const SampleRecord& value) {
        IngressEvent event{};
        event.sample = value;
        event.first_sequence = sequence;
        event.kind = IngressEventKind::Sample;
        return event;
    }

    static IngressEvent gap_event(const GapRecord& value) {
        IngressEvent event{};
        event.first_sequence = value.first_missing;
        event.missing_count = value.missing_count;
        event.kind = IngressEventKind::Gap;
        event.gap_reason = value.reason;
        return event;
    }

    static IngressEvent continuity_break_event(
        const ContinuityBreakRecord& value) {
        IngressEvent event{};
        event.first_sequence = value.at_sequence;
        event.kind = IngressEventKind::ContinuityBreak;
        event.continuity_reason = value.reason;
        return event;
    }

    GapRecord gap() const {
        return {first_sequence, missing_count, gap_reason};
    }

    ContinuityBreakRecord continuity_break() const {
        return {first_sequence, continuity_reason};
    }
};

static_assert(sizeof(IngressEvent) == 56, "IngressEvent RAM cost changed");

struct SampleIngressState {
    uint64_t source_cursor = 0;
    uint64_t queued_samples = 0;
    uint64_t queued_gaps = 0;
    uint64_t queued_continuity_breaks = 0;
    uint64_t sequences_accounted_as_gap = 0;
    uint64_t queue_full_events = 0;
    uint64_t mixed_reason_merges = 0;
    uint64_t pending_overflow_faults = 0;
    size_t pending_events = 0;
    size_t pending_capacity = 0;
    bool faulted = false;
    IngressEvent pending_front{};
    size_t queue_size_approx = 0;
    size_t queue_capacity = 0;
};

enum class PublishStatus : uint8_t {
    Queued = 0,
    AccountedAsGap,
    BufferedContinuityBreak,
    SequenceExhausted,
    InvalidGap,
    InvalidContinuityBreak,
    InternalError,
};

enum class ConsumeIngressStatus : uint8_t {
    Empty = 0,
    SampleAppended,
    GapAppended,
    ContinuityBreakAppended,
    JournalRejected,
    InvalidEvent,
};

template <size_t QueueCapacity, size_t SampleCapacity, size_t GapCapacity>
ConsumeIngressStatus consume_ingress_once(
    runtime::SpscQueue<IngressEvent, QueueCapacity>& queue,
    MeasurementJournal<SampleCapacity, GapCapacity>& journal) {
    const IngressEvent* event = nullptr;
    if (!queue.try_front(event)) return ConsumeIngressStatus::Empty;

    StreamAppendResult appended{};
    ConsumeIngressStatus success = ConsumeIngressStatus::InvalidEvent;
    if (event->kind == IngressEventKind::Sample) {
        appended = journal.append_sample(event->first_sequence, event->sample);
        success = ConsumeIngressStatus::SampleAppended;
    } else if (event->kind == IngressEventKind::Gap) {
        appended = journal.append_gap(event->gap());
        success = ConsumeIngressStatus::GapAppended;
    } else if (event->kind == IngressEventKind::ContinuityBreak) {
        appended = journal.append_continuity_break(event->continuity_break());
        success = ConsumeIngressStatus::ContinuityBreakAppended;
    } else {
        return ConsumeIngressStatus::InvalidEvent;
    }
    if (appended.status != StreamAppendStatus::Ok) {
        return ConsumeIngressStatus::JournalRejected;
    }
    return queue.pop_front() ? success : ConsumeIngressStatus::JournalRejected;
}

// Core-0 owner of source identity and overload accounting. It never blocks.
// Once a gap is pending, no later sample can pass it in queue order.
template <size_t Capacity, size_t PendingCapacity = 3>
class SampleIngress {
    static_assert(PendingCapacity > 0,
                  "pending ingress capacity must be non-zero");

public:
    using Queue = runtime::SpscQueue<IngressEvent, Capacity>;

    explicit SampleIngress(Queue& queue, uint64_t initial_cursor = 0)
        : queue_(queue), sequencer_(initial_cursor) {}

    SampleIngress(const SampleIngress&) = delete;
    SampleIngress& operator=(const SampleIngress&) = delete;

    PublishStatus publish_sample(const SampleRecord& sample) {
        if (faulted_) return PublishStatus::InternalError;
        const SequenceClaim claim = sequencer_.claim_sample();
        if (claim.status != SequenceClaimStatus::Ok) {
            return PublishStatus::SequenceExhausted;
        }

        if (!valid_sample_record(sample)) {
            return account_missing(claim.first, 1, GapReason::SourceLoss)
                ? PublishStatus::AccountedAsGap
                : PublishStatus::InternalError;
        }

        if (pending_size_ != 0 && !flush_pending()) {
            return account_missing(claim.first, 1, GapReason::IngressOverflow)
                ? PublishStatus::AccountedAsGap
                : PublishStatus::InternalError;
        }

        if (!queue_.try_push(IngressEvent::sample_event(claim.first, sample))) {
            saturating_increment(queue_full_events_);
            return account_missing(claim.first, 1, GapReason::IngressOverflow)
                ? PublishStatus::AccountedAsGap
                : PublishStatus::InternalError;
        }
        saturating_increment(queued_samples_);
        return PublishStatus::Queued;
    }

    PublishStatus publish_gap(uint64_t count, GapReason reason) {
        if (faulted_) return PublishStatus::InternalError;
        GapRecord claimed{};
        const SequenceClaim claim = sequencer_.claim_gap(count, reason, claimed);
        if (claim.status == SequenceClaimStatus::InvalidCount ||
            claim.status == SequenceClaimStatus::InvalidReason) {
            return PublishStatus::InvalidGap;
        }
        if (claim.status != SequenceClaimStatus::Ok) {
            return PublishStatus::SequenceExhausted;
        }

        if (pending_size_ != 0 && !flush_pending()) {
            return account_missing(
                       claimed.first_missing, claimed.missing_count, reason)
                ? PublishStatus::AccountedAsGap
                : PublishStatus::InternalError;
        }
        if (!queue_.try_push(IngressEvent::gap_event(claimed))) {
            saturating_increment(queue_full_events_);
            return account_missing(
                       claimed.first_missing, claimed.missing_count, reason)
                ? PublishStatus::AccountedAsGap
                : PublishStatus::InternalError;
        }
        saturating_increment(queued_gaps_);
        return PublishStatus::Queued;
    }

    PublishStatus publish_continuity_break(ContinuityBreakReason reason) {
        if (faulted_) return PublishStatus::InternalError;
        ContinuityBreakRecord claimed{};
        const SequenceClaim claim =
            sequencer_.claim_continuity_break(reason, claimed);
        if (claim.status == SequenceClaimStatus::InvalidReason) {
            return PublishStatus::InvalidContinuityBreak;
        }
        if (claim.status != SequenceClaimStatus::Ok) {
            return PublishStatus::SequenceExhausted;
        }

        const IngressEvent event =
            IngressEvent::continuity_break_event(claimed);
        if (pending_size_ != 0 && !flush_pending()) {
            return buffer_event(event)
                ? PublishStatus::BufferedContinuityBreak
                : PublishStatus::InternalError;
        }
        if (!queue_.try_push(event)) {
            saturating_increment(queue_full_events_);
            return buffer_event(event)
                ? PublishStatus::BufferedContinuityBreak
                : PublishStatus::InternalError;
        }
        saturating_increment(queued_continuity_breaks_);
        return PublishStatus::Queued;
    }

    bool flush_pending() {
        if (faulted_) return false;
        while (pending_size_ != 0) {
            if (!queue_.try_push(pending_[0])) return false;
            if (pending_[0].kind == IngressEventKind::Gap) {
                saturating_increment(queued_gaps_);
            } else if (pending_[0].kind ==
                       IngressEventKind::ContinuityBreak) {
                saturating_increment(queued_continuity_breaks_);
            }
            for (size_t index = 1; index < pending_size_; ++index) {
                pending_[index - 1] = pending_[index];
            }
            --pending_size_;
            pending_[pending_size_] = {};
        }
        return true;
    }

    SampleIngressState state() const {
        return {
            sequencer_.cursor(),
            queued_samples_,
            queued_gaps_,
            queued_continuity_breaks_,
            sequences_accounted_as_gap_,
            queue_full_events_,
            mixed_reason_merges_,
            pending_overflow_faults_,
            pending_size_,
            PendingCapacity,
            faulted_,
            pending_size_ == 0 ? IngressEvent{} : pending_[0],
            queue_.size_approx(),
            Capacity,
        };
    }

private:
    static void saturating_increment(uint64_t& value) {
        if (value != UINT64_MAX) ++value;
    }

    static void saturating_add(uint64_t& value, uint64_t increment) {
        value = increment > UINT64_MAX - value ? UINT64_MAX : value + increment;
    }

    bool account_missing(uint64_t first, uint64_t count, GapReason reason) {
        const IngressEvent event =
            IngressEvent::gap_event({first, count, reason});
        if (pending_size_ != 0 &&
            pending_[pending_size_ - 1].kind == IngressEventKind::Gap) {
            IngressEvent& last = pending_[pending_size_ - 1];
            const GapRecord current = last.gap();
            const bool adjacent = current.end() == first;
            if (!adjacent || current.missing_count > UINT64_MAX - count) {
                faulted_ = true;
                return false;
            }
            if (last.gap_reason != reason) {
                if (last.gap_reason != GapReason::UnknownDiscontinuity) {
                    saturating_increment(mixed_reason_merges_);
                }
                last.gap_reason = GapReason::UnknownDiscontinuity;
            }
            last.missing_count += count;
        } else if (!buffer_event(event)) {
            return false;
        }
        saturating_add(sequences_accounted_as_gap_, count);
        return true;
    }

    bool buffer_event(const IngressEvent& event) {
        if (pending_size_ == PendingCapacity) {
            saturating_increment(pending_overflow_faults_);
            faulted_ = true;
            return false;
        }
        pending_[pending_size_++] = event;
        return true;
    }

    Queue& queue_;
    SourceSequencer sequencer_{};
    IngressEvent pending_[PendingCapacity]{};
    uint64_t queued_samples_ = 0;
    uint64_t queued_gaps_ = 0;
    uint64_t queued_continuity_breaks_ = 0;
    uint64_t sequences_accounted_as_gap_ = 0;
    uint64_t queue_full_events_ = 0;
    uint64_t mixed_reason_merges_ = 0;
    uint64_t pending_overflow_faults_ = 0;
    size_t pending_size_ = 0;
    bool faulted_ = false;
};

}  // namespace measurement
