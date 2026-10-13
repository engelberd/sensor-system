#include <cassert>

#include "runtime/configuration_runtime.h"

namespace {

struct FakeSensorRuntime {
    runtime::SensorRuntimeSnapshot value{};
    runtime::SensorRuntimeSnapshot snapshot() const { return value; }
};

struct FakeStats { uint64_t next_sample_seq = 77; };

struct FakeAcquisition {
    SensorStatus next_status = SensorStatus::Ok;
    SensorStatus rollback_status = SensorStatus::Ok;
    DeviceConfig last{};
    unsigned calls = 0;
    SensorStatus reload_runtime_config(const DeviceConfig& value) {
        last = value;
        ++calls;
        return calls == 1 ? next_status : rollback_status;
    }
    FakeStats stats() const { return {}; }
};

struct FakeConfigManager {
    DeviceConfig value{};
    uint32_t revision = 3;
    bool save_ok = true;
    const DeviceConfig& current() const { return value; }
    uint32_t generation() const { return revision; }
    void replace_device_config(const DeviceConfig& next) { value = next; }
    bool save() {
        if (!save_ok) return false;
        ++revision;
        return true;
    }
    static DeviceConfig default_device_config() { return {}; }
};

struct FakeSink {
    uint32_t revision = 3;
    bool set_config_revision(uint32_t value) { revision = value; return true; }
};

using Runtime = runtime::ConfigurationRuntime<
    FakeSensorRuntime, FakeAcquisition, FakeConfigManager, FakeSink>;

runtime::ConfigurationCommand command() {
    runtime::ConfigurationCommand value{};
    value.operation_id = 9;
    value.expected_revision = 3;
    value.configuration.output_rate_code = 4;
    value.configuration.range_g = 4;
    value.configuration.fifo_watermark = 30;
    return value;
}

void applies_only_in_service_states_and_advances_revision() {
    FakeSensorRuntime sensor{};
    FakeAcquisition acquisition{};
    FakeConfigManager config{};
    config.value.odr_hz = 250;
    FakeSink sink{};
    Runtime runtime(sensor, acquisition, config, sink);

    auto result = runtime.handle(command());
    assert(result.code == runtime::ConfigurationResultCode::WrongMode);
    assert(acquisition.calls == 0);

    sensor.value.state = runtime::SensorRuntimeState::Maintenance;
    result = runtime.handle(command());
    assert(result.code == runtime::ConfigurationResultCode::Applied);
    assert(result.revision == 4);
    assert(result.effective_sample_seq == 77);
    assert(config.value.odr_hz == 500);
    assert(config.value.range_g == 4);
    assert(sink.revision == 4);

    sensor.value.state = runtime::SensorRuntimeState::Faulted;
    auto next = command();
    next.operation_id = 10;
    next.expected_revision = 4;
    result = runtime.handle(next);
    assert(result.code == runtime::ConfigurationResultCode::Applied);
    assert(result.revision == 5);
}

void rejects_stale_revision_before_touching_hardware() {
    FakeSensorRuntime sensor{};
    sensor.value.state = runtime::SensorRuntimeState::Maintenance;
    FakeAcquisition acquisition{};
    FakeConfigManager config{};
    FakeSink sink{};
    Runtime runtime(sensor, acquisition, config, sink);
    auto value = command();
    value.expected_revision = 2;
    const auto result = runtime.handle(value);
    assert(result.code == runtime::ConfigurationResultCode::RevisionConflict);
    assert(acquisition.calls == 0);
}

void storage_failure_restores_the_previous_runtime_config() {
    FakeSensorRuntime sensor{};
    sensor.value.state = runtime::SensorRuntimeState::Maintenance;
    FakeAcquisition acquisition{};
    FakeConfigManager config{};
    config.value.odr_hz = 250;
    config.value.range_g = 2;
    config.save_ok = false;
    FakeSink sink{};
    Runtime runtime(sensor, acquisition, config, sink);
    const auto result = runtime.handle(command());
    assert(result.code == runtime::ConfigurationResultCode::StorageFailed);
    assert(acquisition.calls == 2);
    assert(config.value.odr_hz == 250);
    assert(config.value.range_g == 2);
    assert(config.revision == 3);
    assert(sink.revision == 3);
}

void failed_sensor_apply_reports_failed_rollback() {
    FakeSensorRuntime sensor{};
    sensor.value.state = runtime::SensorRuntimeState::Maintenance;
    FakeAcquisition acquisition{};
    acquisition.next_status = SensorStatus::CommError;
    acquisition.rollback_status = SensorStatus::CommError;
    FakeConfigManager config{};
    config.value.odr_hz = 250;
    FakeSink sink{};
    Runtime runtime(sensor, acquisition, config, sink);

    const auto result = runtime.handle(command());
    assert(result.code == runtime::ConfigurationResultCode::RollbackFailed);
    assert(acquisition.calls == 2);
    assert(config.revision == 3);
}

}  // namespace

int main() {
    applies_only_in_service_states_and_advances_revision();
    rejects_stale_revision_before_touching_hardware();
    storage_failure_restores_the_previous_runtime_config();
    failed_sensor_apply_reports_failed_rollback();
    return 0;
}
