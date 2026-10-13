#pragma once

#include <cstdint>

#include "common/device_config.h"
#include "common/sensor_types.h"
#include "runtime/sensor_runtime.h"

namespace runtime {

enum class ConfigurationOperation : uint8_t { Apply = 0, Reset = 1 };

struct RuntimeMeasurementConfiguration {
    uint8_t output_rate_code = 5;
    uint8_t range_g = 2;
    uint8_t high_pass_corner = 0;
    uint8_t filter_profile = 1;
    uint8_t fifo_watermark = 30;
    int16_t offset_x = 0;
    int16_t offset_y = 0;
    int16_t offset_z = 0;
};

struct ConfigurationCommand {
    uint32_t operation_id = 0;
    uint32_t expected_revision = 0;
    ConfigurationOperation operation = ConfigurationOperation::Apply;
    RuntimeMeasurementConfiguration configuration{};
};

enum class ConfigurationResultCode : uint8_t {
    Applied = 0,
    RevisionConflict = 1,
    InvalidConfiguration = 2,
    StorageFailed = 3,
    SensorFailed = 4,
    RollbackFailed = 5,
    WrongMode = 6,
};

struct ConfigurationResult {
    uint32_t operation_id = 0;
    ConfigurationResultCode code = ConfigurationResultCode::Applied;
    uint32_t revision = 0;
    RuntimeMeasurementConfiguration configuration{};
    uint8_t sensor_odr_register = 0;
    uint8_t decimation_factor = 2;
    uint64_t effective_sample_seq = 0;
};

inline bool output_rate_to_sensor(
    uint8_t output_rate_code,
    uint16_t& odr_hz,
    uint8_t& odr_register) {
    switch (output_rate_code) {
        case 1: odr_hz = 4000; odr_register = 0; return true;
        case 2: odr_hz = 2000; odr_register = 1; return true;
        case 3: odr_hz = 1000; odr_register = 2; return true;
        case 4: odr_hz = 500; odr_register = 3; return true;
        case 5: odr_hz = 250; odr_register = 4; return true;
        case 6: odr_hz = 125; odr_register = 5; return true;
        default: return false;
    }
}

inline RuntimeMeasurementConfiguration runtime_configuration_from_device(
    const DeviceConfig& value) {
    RuntimeMeasurementConfiguration output{};
    switch (value.odr_hz) {
        case 4000: output.output_rate_code = 1; break;
        case 2000: output.output_rate_code = 2; break;
        case 1000: output.output_rate_code = 3; break;
        case 500: output.output_rate_code = 4; break;
        case 250: output.output_rate_code = 5; break;
        case 125: output.output_rate_code = 6; break;
        default: output.output_rate_code = UINT8_MAX; break;
    }
    output.range_g = value.range_g;
    output.high_pass_corner = value.high_pass_corner;
    output.filter_profile = value.filter_profile;
    output.fifo_watermark = value.fifo_watermark;
    output.offset_x = static_cast<int16_t>(value.offset_x);
    output.offset_y = static_cast<int16_t>(value.offset_y);
    output.offset_z = static_cast<int16_t>(value.offset_z);
    return output;
}

inline bool valid_runtime_configuration(
    const RuntimeMeasurementConfiguration& value) {
    uint16_t odr_hz = 0;
    uint8_t odr_register = 0;
    return output_rate_to_sensor(
               value.output_rate_code, odr_hz, odr_register) &&
           (value.range_g == 2 || value.range_g == 4 || value.range_g == 8) &&
           value.high_pass_corner <= 7 && value.filter_profile <= 2 &&
           value.fifo_watermark >= 3 && value.fifo_watermark <= 96 &&
           value.fifo_watermark % 3 == 0;
}

template <typename SensorRuntime,
          typename Acquisition,
          typename ConfigManager,
          typename MeasurementSink>
class ConfigurationRuntime {
public:
    ConfigurationRuntime(SensorRuntime& sensor_runtime,
                         Acquisition& acquisition,
                         ConfigManager& config_manager,
                         MeasurementSink& measurement_sink)
        : sensor_runtime_(sensor_runtime),
          acquisition_(acquisition),
          config_manager_(config_manager),
          measurement_sink_(measurement_sink) {}

    ConfigurationResult handle(const ConfigurationCommand& command) {
        const SensorRuntimeState state = sensor_runtime_.snapshot().state;
        if (state != SensorRuntimeState::Maintenance &&
            state != SensorRuntimeState::Faulted) {
            return result(command.operation_id, ConfigurationResultCode::WrongMode);
        }
        if (command.expected_revision != config_manager_.generation()) {
            return result(command.operation_id,
                          ConfigurationResultCode::RevisionConflict);
        }

        RuntimeMeasurementConfiguration requested = command.configuration;
        if (command.operation == ConfigurationOperation::Reset) {
            DeviceConfig defaults = ConfigManager::default_device_config();
            requested = runtime_configuration_from_device(defaults);
        }
        if (!valid_runtime_configuration(requested)) {
            return result(command.operation_id,
                          ConfigurationResultCode::InvalidConfiguration);
        }

        const DeviceConfig previous = config_manager_.current();
        DeviceConfig next = previous;
        uint8_t odr_register = 0;
        if (!output_rate_to_sensor(
                requested.output_rate_code, next.odr_hz, odr_register)) {
            return result(command.operation_id,
                          ConfigurationResultCode::InvalidConfiguration);
        }
        next.range_g = requested.range_g;
        next.high_pass_corner = requested.high_pass_corner;
        next.filter_profile = requested.filter_profile;
        next.fifo_watermark = requested.fifo_watermark;
        next.offset_x = requested.offset_x;
        next.offset_y = requested.offset_y;
        next.offset_z = requested.offset_z;

        if (acquisition_.reload_runtime_config(next) != SensorStatus::Ok) {
            const SensorStatus rollback =
                acquisition_.reload_runtime_config(previous);
            return result(
                command.operation_id,
                rollback == SensorStatus::Ok
                    ? ConfigurationResultCode::SensorFailed
                    : ConfigurationResultCode::RollbackFailed);
        }

        config_manager_.replace_device_config(next);
        if (!config_manager_.save()) {
            config_manager_.replace_device_config(previous);
            const SensorStatus rollback =
                acquisition_.reload_runtime_config(previous);
            return result(
                command.operation_id,
                rollback == SensorStatus::Ok
                    ? ConfigurationResultCode::StorageFailed
                    : ConfigurationResultCode::RollbackFailed);
        }

        if (!measurement_sink_.set_config_revision(
                config_manager_.generation())) {
            return result(command.operation_id,
                          ConfigurationResultCode::RollbackFailed);
        }
        return result(command.operation_id, ConfigurationResultCode::Applied);
    }

private:
    ConfigurationResult result(
        uint32_t operation_id, ConfigurationResultCode code) const {
        const DeviceConfig& current = config_manager_.current();
        uint16_t unused_odr = 0;
        uint8_t sensor_odr = UINT8_MAX;
        const auto configuration = runtime_configuration_from_device(current);
        (void)output_rate_to_sensor(
            configuration.output_rate_code, unused_odr, sensor_odr);
        return {
            operation_id,
            code,
            config_manager_.generation(),
            configuration,
            sensor_odr,
            2,
            acquisition_.stats().next_sample_seq,
        };
    }

    SensorRuntime& sensor_runtime_;
    Acquisition& acquisition_;
    ConfigManager& config_manager_;
    MeasurementSink& measurement_sink_;
};

}  // namespace runtime
