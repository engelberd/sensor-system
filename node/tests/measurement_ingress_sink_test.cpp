#include <cassert>

#include "measurement/measurement_ingress_sink.h"
#include "measurement/measurement_journal.h"

namespace {

using namespace measurement;

SampleDeviceTime time(uint64_t value, uint32_t segment = 3) {
    return {value, segment, TIMING_QUALITY_LOCKED,
            TimestampSource::DrdyTimeUs64, 0};
}

void test_adapter_preserves_sample_break_sample_order() {
    runtime::SpscQueue<IngressEvent, 8> queue;
    SampleIngress<8> ingress(queue);
    MeasurementIngressSink<SampleIngress<8>> sink(ingress, 7);
    const StoredSample first[] = {{100, 1, 2, 3}, {101, 4, 5, 6}};
    const SampleDeviceTime first_times[] = {time(1000), time(2000)};
    sink.on_timed_samples(first, first_times, 2);
    sink.on_continuity_break(ContinuityBreakReason::SensorReadUncertain);
    const StoredSample last[] = {{900, 7, 8, 9}};
    const SampleDeviceTime last_times[] = {time(3000, 4)};
    sink.on_timed_samples(last, last_times, 1);

    MeasurementJournal<8, 4> journal;
    while (!queue.empty_approx()) {
        assert(consume_ingress_once(queue, journal) !=
               ConsumeIngressStatus::JournalRejected);
    }

    MeasurementReadView view = journal.prepare_read(0, 8);
    assert(view.status == StreamReadStatus::Samples);
    assert(view.first_sequence == 0 && view.end_sequence == 2);
    assert(view.first.data[0].config_revision == 7);
    assert(view.first.data[0].device_time_us == 1000);
    assert(journal.mark_delivered(2) == StreamDeliveryStatus::Ok);
    view = journal.prepare_read(2, 8);
    assert(view.status == StreamReadStatus::ContinuityBreak);
    assert(view.continuity_break.reason ==
           ContinuityBreakReason::SensorReadUncertain);
    assert(journal.mark_delivered(3) == StreamDeliveryStatus::Ok);
    view = journal.prepare_read(3, 8);
    assert(view.status == StreamReadStatus::Samples);
    assert(view.first.data[0].x == 7);

    const MeasurementIngressSinkState state = sink.state();
    assert(state.accepted_samples == 3);
    assert(state.continuity_breaks == 1);
    assert(!state.faulted);
}

void test_queue_pressure_becomes_exact_output_gap() {
    runtime::SpscQueue<IngressEvent, 1> queue;
    SampleIngress<1> ingress(queue);
    MeasurementIngressSink<SampleIngress<1>> sink(ingress, 1);
    const StoredSample samples[] = {{0, 1, 2, 3}, {1, 4, 5, 6}};
    const SampleDeviceTime times[] = {time(100), time(200)};
    sink.on_timed_samples(samples, times, 2);

    const auto state = sink.state();
    assert(state.accepted_samples == 1);
    assert(state.exact_gaps == 1);
    assert(!state.faulted);
    assert(ingress.state().source_cursor == 2);
}

void test_zero_config_revision_faults_before_publication() {
    runtime::SpscQueue<IngressEvent, 2> queue;
    SampleIngress<2> ingress(queue);
    MeasurementIngressSink<SampleIngress<2>> sink(ingress, 0);
    const StoredSample sample[] = {{0, 1, 2, 3}};
    const SampleDeviceTime times[] = {time(100)};
    sink.on_timed_samples(sample, times, 1);
    assert(sink.state().faulted);
    assert(queue.empty_approx());
}

void test_settled_requires_pending_and_cross_core_queue_to_be_empty() {
    runtime::SpscQueue<IngressEvent, 1> queue;
    SampleIngress<1> ingress(queue);
    MeasurementIngressSink<SampleIngress<1>> sink(ingress, 1);
    assert(sink.settled());

    const StoredSample samples[] = {{0, 1, 2, 3}, {1, 4, 5, 6}};
    const SampleDeviceTime times[] = {time(100), time(200)};
    sink.on_timed_samples(samples, times, 2);
    assert(!sink.settled());

    IngressEvent event{};
    assert(queue.try_pop(event));
    assert(sink.flush_pending());
    assert(!sink.settled());
    assert(queue.try_pop(event));
    assert(sink.settled());
}

}  // namespace

int main() {
    test_adapter_preserves_sample_break_sample_order();
    test_queue_pressure_becomes_exact_output_gap();
    test_zero_config_revision_faults_before_publication();
    test_settled_requires_pending_and_cross_core_queue_to_be_empty();
    return 0;
}
