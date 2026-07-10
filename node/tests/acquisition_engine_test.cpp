#include <cassert>
#include <cstddef>
#include <cstdint>

#include "acquisition/acquisition_engine.h"
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
        ++fifo_read_calls;
        const size_t available = fifo_entries / 3u;
        const size_t requested = available < max_samples ? available : max_samples;
        samples_read = next_valid_samples <= requested ? next_valid_samples : requested;
        if (samples_read == 0) {
            diag.last_fifo_discarded_samples = next_discarded_samples;
            if (next_loss_uncertain) {
                diag.flags |= SENSOR_DIAG_FLAG_FIFO_LOSS_UNCERTAIN;
            }
            diag.last_fifo_read_status = static_cast<uint8_t>(SensorStatus::NoData);
            return next_fifo_status == SensorStatus::Ok
                ? SensorStatus::NoData
                : next_fifo_status;
        }
        for (size_t i = 0; i < samples_read; ++i) {
            samples[i] = {static_cast<int32_t>(i), static_cast<int32_t>(i), static_cast<int32_t>(i)};
        }
        fifo_entries = 0;
        status = 0;
        diag.last_fifo_discarded_samples = next_discarded_samples;
        if (next_loss_uncertain) {
            diag.flags |= SENSOR_DIAG_FLAG_FIFO_LOSS_UNCERTAIN;
        }
        diag.last_fifo_read_status = static_cast<uint8_t>(next_fifo_status);
        return next_fifo_status;
    }

    SensorStatus read_fifo_entries(uint8_t& entries) override {
        entries = fifo_entries;
        diag.last_fifo_entries = fifo_entries;
        return SensorStatus::Ok;
    }

    bool supports_fifo() const override { return true; }

    SensorStatus configure_fifo(uint8_t watermark) override {
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
    SensorDiagnosticSnapshot diag{};
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
    TestEngine engine(sensor, buffer);
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
    TestEngine engine(sensor, buffer);
    assert(engine.init(test_config()) == SensorStatus::Ok);

    sensor.irq_sources = SENSOR_DIAG_FLAG_INT1_EVENT;
    sensor.status = ADXL355::STATUS_FIFO_FULL_MASK;
    sensor.fifo_entries = 30;
    sensor.next_valid_samples = 4;
    sensor.next_discarded_samples = 6;
    sensor.next_fifo_status = SensorStatus::InvalidSample;
    engine.update();

    const AcquisitionStats stats = engine.stats();
    assert(stats.fifo_samples_read == 4);
    assert(stats.fifo_discarded_samples == 6);
    assert(stats.dropped_samples == 6);
    assert(stats.sensor_errors == 1);
}

}  // namespace

int main() {
    test_spurious_irq_does_not_touch_fifo_or_delay_fallback();
    test_verified_watermark_irq_reads_fifo_immediately();
    test_partial_read_preserves_prefix_and_reports_exact_loss();
    return 0;
}
