#include <cassert>
#include <cstdint>

#include "runtime/telemetry_runtime.h"

namespace {

uint64_t fake_time_us = 1;
uint64_t now_us() { return fake_time_us; }

class FakeTemperatureSensor final : public ITemperatureSensor {
public:
    SensorStatus read_temperature(TemperatureSample& output) override {
        ++calls;
        if (status == SensorStatus::Ok) output = value;
        return status;
    }

    TemperatureSample value{1885, 25.0f};
    SensorStatus status = SensorStatus::Ok;
    uint32_t calls = 0;
};

class FakeAcquisition {
public:
    SensorStatus read_temperature(
        ITemperatureSensor& sensor, TemperatureSample& output) {
        ++calls;
        fake_time_us += 7;
        return sensor.read_temperature(output);
    }
    uint32_t calls = 0;
};

void test_refreshes_cache_and_respects_interval() {
    fake_time_us = 100;
    FakeAcquisition acquisition;
    FakeTemperatureSensor sensor;
    telemetry::SnapshotCache cache;
    runtime::TelemetryRuntime runtime(
        acquisition, sensor, cache, now_us, {1000, 100});

    telemetry::Snapshot snapshot{};
    assert(!cache.try_read(snapshot));
    runtime.poll();
    assert(cache.try_read(snapshot));
    assert(snapshot.captured_device_time_us == 107);
    assert(snapshot.adxl355_temperature_raw == 1885);
    assert(acquisition.calls == 1);

    fake_time_us = 1000;
    runtime.poll();
    assert(acquisition.calls == 1);
    fake_time_us = 1107;
    runtime.poll();
    assert(acquisition.calls == 2);
}

void test_failure_preserves_last_good_snapshot_and_retries_later() {
    fake_time_us = 10;
    FakeAcquisition acquisition;
    FakeTemperatureSensor sensor;
    telemetry::SnapshotCache cache;
    runtime::TelemetryRuntime runtime(
        acquisition, sensor, cache, now_us, {1000, 100});
    runtime.poll();
    telemetry::Snapshot before{};
    assert(cache.try_read(before));

    sensor.status = SensorStatus::InvalidSample;
    fake_time_us = 1017;
    runtime.poll();
    telemetry::Snapshot after{};
    assert(cache.try_read(after));
    assert(after.captured_device_time_us == before.captured_device_time_us);
    assert(after.adxl355_temperature_raw == before.adxl355_temperature_raw);
    assert(runtime.state().failed_refreshes == 1);

    fake_time_us = 1100;
    runtime.poll();
    assert(acquisition.calls == 2);
    fake_time_us = 1124;
    runtime.poll();
    assert(acquisition.calls == 3);
}

}  // namespace

int main() {
    test_refreshes_cache_and_respects_interval();
    test_failure_preserves_last_good_snapshot_and_retries_later();
    return 0;
}
