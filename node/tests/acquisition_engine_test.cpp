#include <cassert>
#include <cstddef>
#include <cstdint>

#include "acquisition/acquisition_engine.h"
#include "pico/critical_section.h"
#include "pico/time.h"

namespace {

class FakeAccelerometer final : public IAccelerometer {
public:
    SensorStatus init() override { return SensorStatus::Ok; }
    SensorStatus check_device() override { return SensorStatus::Ok; }
    SensorStatus configure(const AccelerometerConfig&) override { return SensorStatus::Ok; }
    SensorStatus set_offset(int32_t, int32_t, int32_t) override { return SensorStatus::Ok; }
    SensorStatus run_self_test(SelfTestResult&) override { return SensorStatus::Ok; }

    SensorStatus read_sample(AccelSample&) override { return SensorStatus::NotSupported; }

    SensorStatus read_fifo_samples(AccelSample* samples,
                                   size_t max_samples,
                                   size_t& samples_read) override {
        const FifoReadOutcome outcome = read_fifo_batch(samples, nullptr, max_samples);
        samples_read = outcome.samples_read;
        return outcome.status;
    }

    FifoReadOutcome read_fifo_batch(
        AccelSample* samples,
        SampleDeviceTime* times,
        size_t max_samples
    ) override {
        ++fifo_read_calls;
        const size_t available = fifo_entries / 3u;
        const size_t requested = available < max_samples ? available : max_samples;
        const size_t samples_read = next_valid_samples <= requested ? next_valid_samples : requested;
        FifoReadOutcome outcome{};
        outcome.samples_read = samples_read;
        outcome.discarded_input_samples = next_discarded_samples;
        if (next_loss_uncertain || next_discarded_samples > 0) {
            outcome.continuity = InputContinuity::Broken;
            outcome.break_reason = next_loss_uncertain
                ? SensorReadBreakReason::TransportUncertain
                : SensorReadBreakReason::InvalidFifoData;
        }
        if (samples_read == 0) {
            outcome.status = next_fifo_status == SensorStatus::Ok
                ? SensorStatus::NoData
                : next_fifo_status;
            return outcome;
        }
        for (size_t i = 0; i < samples_read; ++i) {
            samples[i] = {static_cast<int32_t>(i), static_cast<int32_t>(i), static_cast<int32_t>(i)};
            if (times != nullptr) {
                times[i].device_time_us = 1'000'000u + i * 4'000u;
                times[i].timing_segment_id = timing_segment_id;
                times[i].quality_flags = timestamps_valid
                    ? TIMING_QUALITY_LOCKED
                    : TIMING_QUALITY_INVALID;
                times[i].source = TimestampSource::DrdyTimeUs64;
            }
        }
        fifo_entries = 0;
        status = 0;
        outcome.status = next_fifo_status;
        return outcome;
    }

    SensorStatus read_fifo_samples_timed(
        AccelSample* samples,
        SampleDeviceTime* times,
        size_t max_samples,
        size_t& samples_read
    ) override {
        const FifoReadOutcome outcome = read_fifo_batch(samples, times, max_samples);
        samples_read = outcome.samples_read;
        return outcome.status;
    }

    SensorStatus read_fifo_entries(uint8_t& entries) override {
        entries = fifo_entries;
        diag.last_fifo_entries = fifo_entries;
        return SensorStatus::Ok;
    }

    bool supports_fifo() const override { return true; }
    bool supports_sample_timestamps() const override {
        return timed_mode;
    }

    SensorStatus configure_fifo(uint8_t watermark) override {
        ++fifo_config_calls;
        fifo_entries = 0;
        irq_sources = 0;
        diag.last_fifo_samples = watermark;
        diag.last_int_map = ADXL355::INT_FULL_EN1 | ADXL355::INT_OVR_EN1;
        return SensorStatus::Ok;
    }

    bool supports_data_ready_interrupt() const override { return true; }

    uint8_t consume_data_ready_event_sources() override {
        const uint8_t result = irq_sources;
        irq_sources = 0;
        diag.flags = result;
        return result;
    }

    SensorStatus read_status(uint8_t& value) override {
        if (next_status_read_status != SensorStatus::Ok) {
            return next_status_read_status;
        }
        value = status;
        diag.last_status_reg = status;
        return SensorStatus::Ok;
    }

    SensorStatus refresh_diagnostic_snapshot() override { return SensorStatus::Ok; }
    SensorDiagnosticSnapshot diagnostic_snapshot() const override { return diag; }

    uint8_t irq_sources = 0;
    uint8_t status = 0;
    uint8_t fifo_entries = 0;
    uint32_t fifo_read_calls = 0;
    size_t next_valid_samples = 32;
    uint8_t next_discarded_samples = 0;
    bool next_loss_uncertain = false;
    SensorStatus next_fifo_status = SensorStatus::Ok;
    SensorStatus next_status_read_status = SensorStatus::Ok;
    SensorDiagnosticSnapshot diag{};
    bool timed_mode = false;
    bool timestamps_valid = true;
    uint32_t timing_segment_id = 1;
    uint32_t fifo_config_calls = 0;
};

class CapturingSink final : public ISampleSink,
                            public IAcquisitionQualitySink {
public:
    void on_samples(const StoredSample* samples, size_t count) override {
        on_timed_samples(samples, nullptr, count);
    }

    void on_timed_samples(const StoredSample* samples,
                          const SampleDeviceTime* times,
                          size_t count) override {
        captured_count += count;
        if (count > 0) {
            events[event_count++] = 'S';
            last_sample = samples[count - 1];
            last_time = times != nullptr
                ? times[count - 1]
                : SampleDeviceTime{};
        }
    }

    void on_continuity_break(
        measurement::ContinuityBreakReason reason
    ) override {
        events[event_count++] = 'B';
        last_break_reason = reason;
        ++break_count;
    }

    size_t captured_count = 0;
    size_t break_count = 0;
    size_t event_count = 0;
    char events[8]{};
    StoredSample last_sample{};
    SampleDeviceTime last_time{};
    measurement::ContinuityBreakReason last_break_reason =
        measurement::ContinuityBreakReason::Unknown;
};

class FakeTemperatureSensor final : public ITemperatureSensor {
public:
    SensorStatus read_temperature(TemperatureSample& temperature) override {
        ++calls;
        observed_critical_section_depth = test_critical_section_depth;
        temperature.raw = 1885;
        temperature.celsius = 25.0f;
        return result;
    }

    SensorStatus result = SensorStatus::Ok;
    uint32_t calls = 0;
    int observed_critical_section_depth = 0;
};

using TestEngine = AcquisitionEngine<128>;

DeviceConfig test_config() {
    DeviceConfig config{};
    config.odr_hz = 250;
    config.fifo_watermark = 30;
    return config;
}

void test_spurious_irq_does_not_touch_fifo_or_delay_fallback() {
    test_time_us = 0;
    FakeAccelerometer sensor{};
    AcquisitionBuffer<128> buffer{};
    CapturingSink sink{};
    TestEngine engine(sensor, buffer, &sink, nullptr, &sink);
    assert(engine.init(test_config()) == SensorStatus::Ok);

    sensor.irq_sources = SENSOR_DIAG_FLAG_INT1_EVENT;
    engine.update();
    assert(sensor.fifo_read_calls == 0);
    assert(engine.stats().spurious_int1_events == 1);

    sensor.fifo_entries = 30;
    test_time_us = 52'000;
    engine.update();
    assert(sensor.fifo_read_calls == 1);
    assert(engine.stats().fifo_poll_fallback_reads == 1);
    assert(engine.stats().fifo_samples_read == 10);
}

void test_verified_watermark_irq_reads_fifo_immediately() {
    test_time_us = 0;
    FakeAccelerometer sensor{};
    AcquisitionBuffer<128> buffer{};
    TestEngine engine(sensor, buffer);
    assert(engine.init(test_config()) == SensorStatus::Ok);

    sensor.irq_sources = SENSOR_DIAG_FLAG_INT1_EVENT;
    sensor.status = ADXL355::STATUS_FIFO_FULL_MASK;
    sensor.fifo_entries = 30;
    engine.update();
    assert(sensor.fifo_read_calls == 1);
    assert(engine.stats().spurious_int1_events == 0);
    assert(engine.stats().last_irq_event_ms == 0);
}

void test_partial_read_preserves_prefix_and_reports_exact_loss() {
    test_time_us = 0;
    FakeAccelerometer sensor{};
    AcquisitionBuffer<128> buffer{};
    CapturingSink sink{};
    TestEngine engine(sensor, buffer, &sink, nullptr, &sink);
    assert(engine.init(test_config()) == SensorStatus::Ok);

    sensor.irq_sources = SENSOR_DIAG_FLAG_INT1_EVENT;
    sensor.status = ADXL355::STATUS_FIFO_FULL_MASK;
    sensor.fifo_entries = 30;
    sensor.next_valid_samples = 4;
    sensor.next_discarded_samples = 6;
    // A stale service snapshot must not affect this transaction's outcome.
    sensor.diag.last_fifo_discarded_samples = 99;
    sensor.next_fifo_status = SensorStatus::InvalidSample;
    engine.update();

    const AcquisitionStats stats = engine.stats();
    assert(stats.fifo_samples_read == 4);
    assert(stats.fifo_discarded_samples == 6);
    assert(stats.dropped_samples == 6);
    assert(stats.sensor_errors == 1);
    assert(sink.break_count == 1);
    assert(sink.last_break_reason ==
           measurement::ContinuityBreakReason::SensorReadUncertain);
}

void test_timed_fifo_propagates_center_fir_timestamp() {
    test_time_us = 0;
    FakeAccelerometer sensor{};
    sensor.timed_mode = true;
    AcquisitionBuffer<128> buffer{};
    CapturingSink sink{};
    TestEngine engine(sensor, buffer, &sink, nullptr, &sink);
    assert(engine.init(test_config()) == SensorStatus::Ok);

    sensor.irq_sources = SENSOR_DIAG_FLAG_INT1_EVENT;
    sensor.status = ADXL355::STATUS_FIFO_FULL_MASK;
    sensor.fifo_entries = 96;
    sensor.next_valid_samples = 32;
    engine.update();

    assert(sink.captured_count == 1);
    assert(sink.last_time.valid());
    assert(sink.last_time.timing_segment_id == 1);
    // Balanced FIR uses raw input index 16 for its first output.
    assert(sink.last_time.device_time_us == 1'064'000u);
}

void test_invalid_timing_is_dropped_and_rearms_fifo() {
    test_time_us = 0;
    FakeAccelerometer sensor{};
    sensor.timed_mode = true;
    sensor.timestamps_valid = false;
    AcquisitionBuffer<128> buffer{};
    CapturingSink sink{};
    TestEngine engine(sensor, buffer, &sink, nullptr, &sink);
    assert(engine.init(test_config()) == SensorStatus::Ok);
    const uint32_t initial_config_calls = sensor.fifo_config_calls;

    sensor.irq_sources = SENSOR_DIAG_FLAG_INT1_EVENT;
    sensor.status = ADXL355::STATUS_FIFO_FULL_MASK;
    sensor.fifo_entries = 96;
    sensor.next_valid_samples = 32;
    engine.update();

    assert(sink.captured_count == 0);
    assert(engine.stats().soft_recover_count == 1);
    assert(engine.stats().sensor_errors == 1);
    assert(sensor.fifo_config_calls == initial_config_calls + 1);
    assert(sink.break_count == 1);
    assert(sink.last_break_reason ==
           measurement::ContinuityBreakReason::InvalidTiming);
}

void test_fifo_overrun_break_precedes_recovered_samples() {
    test_time_us = 0;
    FakeAccelerometer sensor{};
    sensor.timed_mode = true;
    AcquisitionBuffer<128> buffer{};
    CapturingSink sink{};
    TestEngine engine(sensor, buffer, &sink, nullptr, &sink);
    assert(engine.init(test_config()) == SensorStatus::Ok);

    sensor.irq_sources = SENSOR_DIAG_FLAG_INT1_EVENT;
    sensor.status = ADXL355::STATUS_FIFO_FULL_MASK |
                    ADXL355::STATUS_FIFO_OVR_MASK;
    sensor.fifo_entries = 96;
    sensor.next_valid_samples = 32;
    engine.update();

    assert(sink.break_count == 1);
    assert(sink.captured_count == 1);
    assert(sink.event_count == 2);
    assert(sink.events[0] == 'B' && sink.events[1] == 'S');
    assert(sink.last_break_reason ==
           measurement::ContinuityBreakReason::SensorOverflow);
}

void test_temperature_read_is_serialized_with_sensor_access() {
    test_time_us = 0;
    test_critical_section_depth = 0;
    FakeAccelerometer sensor{};
    FakeTemperatureSensor temperature_sensor{};
    AcquisitionBuffer<128> buffer{};
    TestEngine engine(sensor, buffer);
    assert(engine.init(test_config()) == SensorStatus::Ok);

    TemperatureSample temperature{};
    assert(
        engine.read_temperature(temperature_sensor, temperature) ==
        SensorStatus::Ok
    );
    assert(temperature_sensor.calls == 1);
    assert(temperature_sensor.observed_critical_section_depth > 0);
    assert(test_critical_section_depth == 0);
    assert(temperature.raw == 1885);
}

void test_prepare_for_runtime_rearms_fifo_without_counting_startup_backlog() {
    test_time_us = 100'000;
    FakeAccelerometer sensor{};
    AcquisitionBuffer<128> buffer{};
    TestEngine engine(sensor, buffer);
    assert(engine.init(test_config()) == SensorStatus::Ok);
    const uint32_t initial_config_calls = sensor.fifo_config_calls;

    sensor.fifo_entries = 57;
    sensor.irq_sources = SENSOR_DIAG_FLAG_INT1_EVENT;
    assert(engine.prepare_for_runtime() == SensorStatus::Ok);

    assert(sensor.fifo_config_calls == initial_config_calls + 1);
    assert(sensor.fifo_entries == 0);
    assert(sensor.irq_sources == 0);
    assert(engine.stats().dropped_samples == 0);
    assert(engine.stats().sensor_errors == 0);
}

void test_finish_recording_drains_fifo_and_emits_final_boundary() {
    test_time_us = 100'000;
    FakeAccelerometer sensor{};
    sensor.timed_mode = true;
    AcquisitionBuffer<128> buffer{};
    CapturingSink sink{};
    TestEngine engine(sensor, buffer, &sink, nullptr, &sink);
    assert(engine.init(test_config()) == SensorStatus::Ok);

    sensor.fifo_entries = 96;
    sensor.next_valid_samples = 32;
    assert(engine.finish_recording() == SensorStatus::Ok);
    assert(engine.is_paused());
    assert(sensor.fifo_read_calls == 1);
    assert(sink.captured_count == 1);
    assert(sink.break_count == 1);
    assert(sink.last_break_reason ==
           measurement::ContinuityBreakReason::ProcessingReset);
    assert(sink.event_count == 2);
    assert(sink.events[0] == 'S' && sink.events[1] == 'B');

    engine.update();
    assert(sensor.fifo_read_calls == 1);
}

void test_finish_recording_failure_stays_paused_and_has_no_false_boundary() {
    test_time_us = 100'000;
    FakeAccelerometer sensor{};
    sensor.next_status_read_status = SensorStatus::CommError;
    AcquisitionBuffer<128> buffer{};
    CapturingSink sink{};
    TestEngine engine(sensor, buffer, &sink, nullptr, &sink);
    assert(engine.init(test_config()) == SensorStatus::Ok);

    assert(engine.finish_recording() == SensorStatus::CommError);
    assert(engine.is_paused());
    assert(sink.break_count == 0);
    engine.update();
    assert(sensor.fifo_read_calls == 0);
}

}  // namespace

int main() {
    test_spurious_irq_does_not_touch_fifo_or_delay_fallback();
    test_verified_watermark_irq_reads_fifo_immediately();
    test_partial_read_preserves_prefix_and_reports_exact_loss();
    test_timed_fifo_propagates_center_fir_timestamp();
    test_invalid_timing_is_dropped_and_rearms_fifo();
    test_fifo_overrun_break_precedes_recovered_samples();
    test_temperature_read_is_serialized_with_sensor_access();
    test_prepare_for_runtime_rearms_fifo_without_counting_startup_backlog();
    test_finish_recording_drains_fifo_and_emits_final_boundary();
    test_finish_recording_failure_stays_paused_and_has_no_false_boundary();
    return 0;
}
