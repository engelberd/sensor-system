#include <cassert>
#include <cstddef>
#include <cstdint>

#include "observer/observer_snapshot_sink.h"
#include "processing/sample_sink_fanout.h"

namespace {

StoredSample sample(uint64_t sequence) {
    StoredSample value{};
    value.sample_seq = sequence;
    value.x = static_cast<int32_t>(sequence + 10);
    value.y = static_cast<int32_t>(sequence + 20);
    value.z = static_cast<int32_t>(sequence + 30);
    return value;
}

SampleDeviceTime time(uint64_t device_time_us, uint32_t segment) {
    SampleDeviceTime value{};
    value.device_time_us = device_time_us;
    value.timing_segment_id = segment;
    value.quality_flags = TIMING_QUALITY_LOCKED;
    value.source = TimestampSource::DrdyTimeUs64;
    return value;
}

void test_empty_sink_has_no_snapshot() {
    ObserverSnapshotSink<4> sink{};
    ObserverSampleSnapshot<4> snapshot{};

    assert(!sink.read_latest(snapshot));
    assert(sink.stats().published_batches == 0);
}

void test_latest_complete_timed_batch_is_published() {
    ObserverSnapshotSink<4> sink{};
    StoredSample samples[2]{sample(7), sample(8)};
    SampleDeviceTime times[2]{time(1000, 3), time(2000, 3)};

    sink.on_timed_samples(samples, times, 2);

    ObserverSampleSnapshot<4> snapshot{};
    assert(sink.read_latest(snapshot));
    assert(snapshot.generation == 1);
    assert(snapshot.sample_count == 2);
    assert(snapshot.samples[0].sample_seq == 7);
    assert(snapshot.samples[1].z == 38);
    assert(snapshot.times[0].device_time_us == 1000);
    assert(snapshot.times[1].timing_segment_id == 3);
    assert(snapshot.times[1].valid());
}

void test_new_batch_replaces_previous_snapshot() {
    ObserverSnapshotSink<4> sink{};
    StoredSample first[2]{sample(1), sample(2)};
    StoredSample second[1]{sample(20)};

    sink.on_samples(first, 2);
    sink.on_samples(second, 1);

    ObserverSampleSnapshot<4> snapshot{};
    assert(sink.read_latest(snapshot));
    assert(snapshot.generation == 2);
    assert(snapshot.sample_count == 1);
    assert(snapshot.samples[0].sample_seq == 20);
    assert(!snapshot.times[0].valid());
    assert(sink.stats().published_batches == 2);
}

void test_oversized_batch_keeps_newest_samples() {
    ObserverSnapshotSink<3> sink{};
    StoredSample samples[5]{sample(10), sample(11), sample(12), sample(13), sample(14)};

    sink.on_samples(samples, 5);

    ObserverSampleSnapshot<3> snapshot{};
    assert(sink.read_latest(snapshot));
    assert(snapshot.sample_count == 3);
    assert(snapshot.samples[0].sample_seq == 12);
    assert(snapshot.samples[2].sample_seq == 14);
    assert(sink.stats().truncated_batches == 1);
}

class RecordingSink final : public ISampleSink {
public:
    RecordingSink(uint8_t identity, uint8_t* order, size_t& calls)
        : identity_(identity), order_(order), calls_(calls) {}

    void on_samples(const StoredSample*, size_t) override {
        order_[calls_++] = identity_;
    }

private:
    uint8_t identity_;
    uint8_t* order_;
    size_t& calls_;
};

void test_fanout_calls_production_sink_first() {
    uint8_t order[2]{};
    size_t calls = 0;
    RecordingSink primary(1, order, calls);
    RecordingSink observer(2, order, calls);
    SampleSinkFanout fanout(primary, observer);
    StoredSample value = sample(1);

    fanout.on_samples(&value, 1);

    assert(calls == 2);
    assert(order[0] == 1);
    assert(order[1] == 2);
}

}  // namespace

int main() {
    test_empty_sink_has_no_snapshot();
    test_latest_complete_timed_batch_is_published();
    test_new_batch_replaces_previous_snapshot();
    test_oversized_batch_keeps_newest_samples();
    test_fanout_calls_production_sink_first();
    return 0;
}
