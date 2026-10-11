#include <atomic>
#include <cassert>
#include <cstddef>
#include <cstdint>
#include <thread>

#include "measurement/measurement_journal.h"
#include "measurement/sample_ingress.h"
#include "runtime/spsc_queue.h"

namespace {
using namespace measurement;

SampleRecord sample(int32_t value) {
    return {static_cast<uint64_t>(value + 1000), value, -value, value + 1,
            1, 1, 1, 0};
}

template <size_t QueueCapacity, size_t SampleCapacity, size_t GapCapacity>
void drain(
    runtime::SpscQueue<IngressEvent, QueueCapacity>& queue,
    MeasurementJournal<SampleCapacity, GapCapacity>& journal) {
    while (!queue.empty_approx()) {
        const ConsumeIngressStatus status =
            consume_ingress_once(queue, journal);
        assert(status == ConsumeIngressStatus::SampleAppended ||
               status == ConsumeIngressStatus::GapAppended ||
               status == ConsumeIngressStatus::ContinuityBreakAppended);
    }
}

void test_overflow_becomes_an_ordered_exact_gap() {
    runtime::SpscQueue<IngressEvent, 2> queue;
    SampleIngress<2> ingress(queue);
    assert(ingress.publish_sample(sample(0)) == PublishStatus::Queued);
    assert(ingress.publish_sample(sample(1)) == PublishStatus::Queued);
    assert(ingress.publish_sample(sample(2)) == PublishStatus::AccountedAsGap);
    assert(ingress.publish_sample(sample(3)) == PublishStatus::AccountedAsGap);

    MeasurementJournal<8, 4> journal;
    drain(queue, journal);
    assert(ingress.flush_pending());
    assert(ingress.publish_sample(sample(4)) == PublishStatus::Queued);
    drain(queue, journal);

    MeasurementReadView view = journal.prepare_read(0, 8);
    assert(view.status == StreamReadStatus::Samples && view.end_sequence == 2);
    assert(journal.mark_delivered(2) == StreamDeliveryStatus::Ok);
    view = journal.prepare_read(2, 8);
    assert(view.status == StreamReadStatus::Gap);
    assert(view.gap.first_missing == 2 && view.gap.missing_count == 2);
    assert(view.gap.reason == GapReason::IngressOverflow);
    assert(journal.mark_delivered(4) == StreamDeliveryStatus::Ok);
    view = journal.prepare_read(4, 8);
    assert(view.status == StreamReadStatus::Samples && view.end_sequence == 5);

    const SampleIngressState state = ingress.state();
    assert(state.source_cursor == 5);
    assert(state.sequences_accounted_as_gap == 2);
    assert(state.queue_full_events == 1);
    assert(state.pending_events == 0);
}

void test_consumer_keeps_rejected_event_at_the_front() {
    runtime::SpscQueue<IngressEvent, 2> queue;
    MeasurementJournal<4, 2> journal;
    assert(queue.try_push(IngressEvent::sample_event(1, sample(1))));
    assert(consume_ingress_once(queue, journal) ==
           ConsumeIngressStatus::JournalRejected);
    assert(queue.size_approx() == 1);

    const IngressEvent* front = nullptr;
    assert(queue.try_front(front));
    assert(front != nullptr && front->first_sequence == 1);
    assert(journal.state().produced == 0);
}

void test_later_sample_never_passes_a_pending_gap() {
    runtime::SpscQueue<IngressEvent, 1> queue;
    SampleIngress<1> ingress(queue);
    assert(ingress.publish_sample(sample(0)) == PublishStatus::Queued);
    assert(ingress.publish_sample(sample(1)) == PublishStatus::AccountedAsGap);

    IngressEvent event{};
    assert(queue.try_pop(event) && event.kind == IngressEventKind::Sample);
    // The only free slot is consumed by the pending gap before sample 2 is
    // considered, so sample 2 extends the next pending range instead.
    assert(ingress.publish_sample(sample(2)) == PublishStatus::AccountedAsGap);
    assert(queue.try_pop(event) && event.kind == IngressEventKind::Gap);
    assert(event.first_sequence == 1 && event.missing_count == 1);
    assert(ingress.flush_pending());
    assert(queue.try_pop(event) && event.kind == IngressEventKind::Gap);
    assert(event.first_sequence == 2 && event.missing_count == 1);
}

void test_mixed_losses_preserve_range_and_degrade_reason_honestly() {
    runtime::SpscQueue<IngressEvent, 1> queue;
    SampleIngress<1> ingress(queue);
    assert(ingress.publish_sample(sample(0)) == PublishStatus::Queued);
    assert(ingress.publish_sample(sample(1)) == PublishStatus::AccountedAsGap);
    assert(ingress.publish_gap(3, GapReason::SourceLoss) ==
           PublishStatus::AccountedAsGap);

    const SampleIngressState state = ingress.state();
    assert(state.pending_events == 1);
    assert(state.pending_front.kind == IngressEventKind::Gap);
    assert(state.pending_front.first_sequence == 1 &&
           state.pending_front.missing_count == 4);
    assert(state.pending_front.gap_reason == GapReason::UnknownDiscontinuity);
    assert(state.mixed_reason_merges == 1);
}

void test_invalid_sample_is_accounted_as_source_loss() {
    runtime::SpscQueue<IngressEvent, 2> queue;
    SampleIngress<2> ingress(queue);
    SampleRecord invalid = sample(0);
    invalid.config_revision = 0;
    assert(ingress.publish_sample(invalid) == PublishStatus::AccountedAsGap);
    assert(ingress.flush_pending());
    IngressEvent event{};
    assert(queue.try_pop(event));
    assert(event.kind == IngressEventKind::Gap);
    assert(event.first_sequence == 0 && event.missing_count == 1);
    assert(event.gap_reason == GapReason::SourceLoss);
}

void test_break_stays_before_later_overflow_gap_and_sample() {
    runtime::SpscQueue<IngressEvent, 2> queue;
    SampleIngress<2> ingress(queue);
    assert(ingress.publish_sample(sample(0)) == PublishStatus::Queued);
    assert(ingress.publish_sample(sample(1)) == PublishStatus::Queued);
    assert(ingress.publish_continuity_break(
               ContinuityBreakReason::SensorOverflow) ==
           PublishStatus::BufferedContinuityBreak);
    assert(ingress.publish_sample(sample(3)) == PublishStatus::AccountedAsGap);

    MeasurementJournal<8, 4> journal;
    drain(queue, journal);
    assert(ingress.flush_pending());
    drain(queue, journal);
    assert(ingress.publish_sample(sample(4)) == PublishStatus::Queued);
    drain(queue, journal);

    MeasurementReadView view = journal.prepare_read(0, 8);
    assert(view.status == StreamReadStatus::Samples && view.end_sequence == 2);
    assert(journal.mark_delivered(2) == StreamDeliveryStatus::Ok);
    view = journal.prepare_read(2, 8);
    assert(view.status == StreamReadStatus::ContinuityBreak);
    assert(view.continuity_break.reason ==
           ContinuityBreakReason::SensorOverflow);
    assert(journal.mark_delivered(3) == StreamDeliveryStatus::Ok);
    view = journal.prepare_read(3, 8);
    assert(view.status == StreamReadStatus::Gap);
    assert(view.gap.missing_count == 1 &&
           view.gap.reason == GapReason::IngressOverflow);
    assert(journal.mark_delivered(4) == StreamDeliveryStatus::Ok);
    view = journal.prepare_read(4, 8);
    assert(view.status == StreamReadStatus::Samples && view.end_sequence == 5);

    const SampleIngressState state = ingress.state();
    assert(state.source_cursor == 5);
    assert(state.queued_continuity_breaks == 1);
    assert(state.pending_events == 0 && !state.faulted);
}

void test_pending_quality_overflow_faults_instead_of_hiding_a_break() {
    runtime::SpscQueue<IngressEvent, 1> queue;
    SampleIngress<1, 1> ingress(queue);
    assert(ingress.publish_sample(sample(0)) == PublishStatus::Queued);
    assert(ingress.publish_continuity_break(
               ContinuityBreakReason::SensorOverflow) ==
           PublishStatus::BufferedContinuityBreak);
    assert(ingress.publish_continuity_break(
               ContinuityBreakReason::InvalidTiming) ==
           PublishStatus::InternalError);

    const SampleIngressState state = ingress.state();
    assert(state.faulted);
    assert(state.pending_events == 1 && state.pending_capacity == 1);
    assert(state.pending_overflow_faults == 1);
    assert(state.source_cursor == 3);
    assert(ingress.publish_sample(sample(3)) == PublishStatus::InternalError);
}

void test_invalid_break_reason_does_not_consume_sequence() {
    runtime::SpscQueue<IngressEvent, 2> queue;
    SampleIngress<2> ingress(queue);
    assert(ingress.publish_continuity_break(
               static_cast<ContinuityBreakReason>(0xff)) ==
           PublishStatus::InvalidContinuityBreak);
    assert(ingress.state().source_cursor == 0);
    assert(queue.empty_approx());
}

void test_spsc_memory_order_under_concurrent_wrap() {
    constexpr uint32_t kCount = 200000;
    runtime::SpscQueue<uint32_t, 31> queue;
    std::atomic<bool> start{false};

    std::thread producer([&]() {
        while (!start.load(std::memory_order_acquire)) {}
        for (uint32_t value = 0; value < kCount; ++value) {
            while (!queue.try_push(value)) {}
        }
    });
    std::thread consumer([&]() {
        start.store(true, std::memory_order_release);
        for (uint32_t expected = 0; expected < kCount; ++expected) {
            uint32_t value = 0;
            while (!queue.try_pop(value)) {}
            assert(value == expected);
        }
    });
    producer.join();
    consumer.join();
    assert(queue.empty_approx());
}

}  // namespace

int main() {
    test_overflow_becomes_an_ordered_exact_gap();
    test_later_sample_never_passes_a_pending_gap();
    test_mixed_losses_preserve_range_and_degrade_reason_honestly();
    test_invalid_sample_is_accounted_as_source_loss();
    test_break_stays_before_later_overflow_gap_and_sample();
    test_pending_quality_overflow_faults_instead_of_hiding_a_break();
    test_invalid_break_reason_does_not_consume_sequence();
    test_consumer_keeps_rejected_event_at_the_front();
    test_spsc_memory_order_under_concurrent_wrap();
    return 0;
}
