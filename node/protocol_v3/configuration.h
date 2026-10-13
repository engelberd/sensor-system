#pragma once

#include <cstddef>
#include <cstdint>

#include "protocol_v3/application.h"
#include "protocol_v3/framing.h"

namespace ProtocolV3 {

constexpr uint8_t kConfigurationServiceId =
    static_cast<uint8_t>(ServiceId::Configuration);
constexpr uint8_t kGetConfigMessageId = 1;
constexpr uint8_t kValidateConfigMessageId = 2;
constexpr uint8_t kApplyConfigMessageId = 3;
constexpr uint8_t kResetConfigMessageId = 4;
constexpr uint8_t kConfigurationSchemaVersion = 1;

constexpr size_t kConfigurationValueBytes = 12;
constexpr size_t kObservedConfigBytes = 18;
constexpr size_t kGetConfigResponseBytes = 19;
constexpr size_t kValidateConfigResponseBytes = 3;
constexpr size_t kApplyConfigRequestBytes = 20;
constexpr size_t kResetConfigRequestBytes = 8;
constexpr size_t kConfigOperationResponseBytes = 32;

enum class OutputRateCode : uint8_t {
    Hz4000 = 0,
    Hz2000 = 1,
    Hz1000 = 2,
    Hz500 = 3,
    Hz250 = 4,
    Hz125 = 5,
    Hz62_5 = 6,
    Hz31_25 = 7,
    Hz15_625 = 8,
    Hz7_8125 = 9,
    Hz3_90625 = 10,
};

enum class ConfigurationFilterProfile : uint8_t {
    Light = 0,
    Balanced = 1,
    Aggressive = 2,
};

struct MeasurementConfiguration {
    OutputRateCode output_rate = OutputRateCode::Hz125;
    uint8_t range_g = 2;
    uint8_t high_pass_corner = 0;
    ConfigurationFilterProfile filter_profile =
        ConfigurationFilterProfile::Balanced;
    uint8_t fifo_watermark = 30;
    int16_t offset_x = 0;
    int16_t offset_y = 0;
    int16_t offset_z = 0;
};

struct ObservedConfiguration {
    uint32_t revision = 0;
    MeasurementConfiguration desired{};
    uint8_t sensor_odr_register = 0;
    uint8_t decimation_factor = 0;
};

enum class ConfigValidationCode : uint8_t {
    Valid = 0,
    UnsupportedOutputRate = 1,
    InvalidRange = 2,
    InvalidHighPassCorner = 3,
    InvalidFilterProfile = 4,
    InvalidFifoWatermark = 5,
    InvalidOffset = 6,
};

enum class ConfigField : uint8_t {
    None = 0,
    OutputRate = 1,
    Range = 2,
    HighPassCorner = 3,
    FilterProfile = 4,
    FifoWatermark = 5,
    OffsetX = 6,
    OffsetY = 7,
    OffsetZ = 8,
};

struct ConfigValidationResult {
    ConfigValidationCode code = ConfigValidationCode::Valid;
    ConfigField field = ConfigField::None;
};

struct ConfigOperationRequest {
    uint32_t operation_id = 0;
    uint32_t expected_revision = 0;
    MeasurementConfiguration configuration{};
};

enum class ConfigOperationCode : uint8_t {
    Applied = 0,
    RevisionConflict = 1,
    InvalidConfiguration = 2,
    StorageFailed = 3,
    SensorFailed = 4,
    RollbackFailed = 5,
    WrongMode = 6,
};

struct ConfigOperationResult {
    uint32_t operation_id = 0;
    ConfigOperationCode code = ConfigOperationCode::Applied;
    ObservedConfiguration observed{};
    uint64_t effective_sample_seq = 0;
};

enum class ConfigurationCodecStatus : uint8_t {
    Ok = 0,
    OutputTooSmall,
    InvalidPayloadLength,
    UnsupportedSchema,
    UnknownOutputRate,
    InvalidRange,
    InvalidHighPassCorner,
    UnknownFilterProfile,
    InvalidFifoWatermark,
    ZeroOperationId,
    ZeroRevision,
    UnknownSensorOdr,
    ZeroDecimationFactor,
    InconsistentObservedRate,
};

ConfigurationCodecStatus validate(const MeasurementConfiguration& value);
ConfigurationCodecStatus validate(const ObservedConfiguration& value);
ConfigValidationResult validate_for_node(const MeasurementConfiguration& value);

ConfigurationCodecStatus encode_configuration(
    const MeasurementConfiguration& value,
    uint8_t* output,
    size_t output_capacity);
ConfigurationCodecStatus decode_configuration(
    const uint8_t* payload,
    size_t payload_size,
    MeasurementConfiguration& output);
ConfigurationCodecStatus decode_apply_config_request(
    const uint8_t* payload,
    size_t payload_size,
    ConfigOperationRequest& output);
ConfigurationCodecStatus decode_reset_config_request(
    const uint8_t* payload,
    size_t payload_size,
    uint32_t& operation_id,
    uint32_t& expected_revision);

ConfigurationCodecStatus encode_get_config_success(
    const ObservedConfiguration& value,
    uint8_t* output,
    size_t output_capacity);
ConfigurationCodecStatus encode_validate_config_success(
    const ConfigValidationResult& value,
    uint8_t* output,
    size_t output_capacity);
ConfigurationCodecStatus encode_config_operation_success(
    const ConfigOperationResult& value,
    uint8_t* output,
    size_t output_capacity);

}  // namespace ProtocolV3
