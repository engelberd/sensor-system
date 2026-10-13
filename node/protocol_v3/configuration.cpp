#include "protocol_v3/configuration.h"

namespace ProtocolV3 {
namespace {

void write_u16(uint16_t value, uint8_t* output) {
    output[0] = static_cast<uint8_t>(value);
    output[1] = static_cast<uint8_t>(value >> 8u);
}

void write_u32(uint32_t value, uint8_t* output) {
    for (size_t i = 0; i < 4; ++i) {
        output[i] = static_cast<uint8_t>(value >> (8u * i));
    }
}

void write_u64(uint64_t value, uint8_t* output) {
    for (size_t i = 0; i < 8; ++i) {
        output[i] = static_cast<uint8_t>(value >> (8u * i));
    }
}

uint16_t read_u16(const uint8_t* input) {
    return static_cast<uint16_t>(input[0]) |
           static_cast<uint16_t>(static_cast<uint16_t>(input[1]) << 8u);
}

uint32_t read_u32(const uint8_t* input) {
    uint32_t value = 0;
    for (size_t i = 0; i < 4; ++i) {
        value |= static_cast<uint32_t>(input[i]) << (8u * i);
    }
    return value;
}

bool known_output_rate(OutputRateCode value) {
    return static_cast<uint8_t>(value) <=
           static_cast<uint8_t>(OutputRateCode::Hz3_90625);
}

bool known_sensor_odr(uint8_t value) { return value <= 0x0au; }

bool rates_match(OutputRateCode output, uint8_t sensor_odr, uint8_t factor) {
    if (factor != 2u) return false;
    const uint8_t output_code = static_cast<uint8_t>(output);
    return output_code >= 1u && sensor_odr == output_code - 1u;
}

ConfigurationCodecStatus encode_observed(
    const ObservedConfiguration& value,
    uint8_t* output,
    size_t output_capacity) {
    const auto status = validate(value);
    if (status != ConfigurationCodecStatus::Ok) return status;
    if (output == nullptr || output_capacity < kObservedConfigBytes) {
        return ConfigurationCodecStatus::OutputTooSmall;
    }
    write_u32(value.revision, output);
    const auto encoded = encode_configuration(
        value.desired, output + 4, output_capacity - 4);
    if (encoded != ConfigurationCodecStatus::Ok) return encoded;
    output[16] = value.sensor_odr_register;
    output[17] = value.decimation_factor;
    return ConfigurationCodecStatus::Ok;
}

}  // namespace

ConfigurationCodecStatus validate(const MeasurementConfiguration& value) {
    if (!known_output_rate(value.output_rate)) {
        return ConfigurationCodecStatus::UnknownOutputRate;
    }
    if (value.range_g != 2u && value.range_g != 4u && value.range_g != 8u) {
        return ConfigurationCodecStatus::InvalidRange;
    }
    if (value.high_pass_corner > 7u) {
        return ConfigurationCodecStatus::InvalidHighPassCorner;
    }
    if (static_cast<uint8_t>(value.filter_profile) > 2u) {
        return ConfigurationCodecStatus::UnknownFilterProfile;
    }
    if (value.fifo_watermark < 3u || value.fifo_watermark > 96u ||
        value.fifo_watermark % 3u != 0u) {
        return ConfigurationCodecStatus::InvalidFifoWatermark;
    }
    return ConfigurationCodecStatus::Ok;
}

ConfigurationCodecStatus validate(const ObservedConfiguration& value) {
    const auto status = validate(value.desired);
    if (status != ConfigurationCodecStatus::Ok) return status;
    if (value.revision == 0) return ConfigurationCodecStatus::ZeroRevision;
    if (!known_sensor_odr(value.sensor_odr_register)) {
        return ConfigurationCodecStatus::UnknownSensorOdr;
    }
    if (value.decimation_factor == 0) {
        return ConfigurationCodecStatus::ZeroDecimationFactor;
    }
    if (!rates_match(value.desired.output_rate,
                     value.sensor_odr_register,
                     value.decimation_factor)) {
        return ConfigurationCodecStatus::InconsistentObservedRate;
    }
    return ConfigurationCodecStatus::Ok;
}

ConfigValidationResult validate_for_node(const MeasurementConfiguration& value) {
    const uint8_t rate = static_cast<uint8_t>(value.output_rate);
    if (rate < static_cast<uint8_t>(OutputRateCode::Hz2000) ||
        rate > static_cast<uint8_t>(OutputRateCode::Hz62_5)) {
        return {ConfigValidationCode::UnsupportedOutputRate,
                ConfigField::OutputRate};
    }
    const auto status = validate(value);
    switch (status) {
        case ConfigurationCodecStatus::Ok:
            return {};
        case ConfigurationCodecStatus::InvalidRange:
            return {ConfigValidationCode::InvalidRange, ConfigField::Range};
        case ConfigurationCodecStatus::InvalidHighPassCorner:
            return {ConfigValidationCode::InvalidHighPassCorner,
                    ConfigField::HighPassCorner};
        case ConfigurationCodecStatus::UnknownFilterProfile:
            return {ConfigValidationCode::InvalidFilterProfile,
                    ConfigField::FilterProfile};
        case ConfigurationCodecStatus::InvalidFifoWatermark:
            return {ConfigValidationCode::InvalidFifoWatermark,
                    ConfigField::FifoWatermark};
        default:
            return {ConfigValidationCode::UnsupportedOutputRate,
                    ConfigField::OutputRate};
    }
}

ConfigurationCodecStatus encode_configuration(
    const MeasurementConfiguration& value,
    uint8_t* output,
    size_t output_capacity) {
    const auto status = validate(value);
    if (status != ConfigurationCodecStatus::Ok) return status;
    if (output == nullptr || output_capacity < kConfigurationValueBytes) {
        return ConfigurationCodecStatus::OutputTooSmall;
    }
    output[0] = kConfigurationSchemaVersion;
    output[1] = static_cast<uint8_t>(value.output_rate);
    output[2] = value.range_g;
    output[3] = value.high_pass_corner;
    output[4] = static_cast<uint8_t>(value.filter_profile);
    output[5] = value.fifo_watermark;
    write_u16(static_cast<uint16_t>(value.offset_x), output + 6);
    write_u16(static_cast<uint16_t>(value.offset_y), output + 8);
    write_u16(static_cast<uint16_t>(value.offset_z), output + 10);
    return ConfigurationCodecStatus::Ok;
}

ConfigurationCodecStatus decode_configuration(
    const uint8_t* payload,
    size_t payload_size,
    MeasurementConfiguration& output) {
    if (payload == nullptr || payload_size != kConfigurationValueBytes) {
        return ConfigurationCodecStatus::InvalidPayloadLength;
    }
    if (payload[0] != kConfigurationSchemaVersion) {
        return ConfigurationCodecStatus::UnsupportedSchema;
    }
    output.output_rate = static_cast<OutputRateCode>(payload[1]);
    output.range_g = payload[2];
    output.high_pass_corner = payload[3];
    output.filter_profile =
        static_cast<ConfigurationFilterProfile>(payload[4]);
    output.fifo_watermark = payload[5];
    output.offset_x = static_cast<int16_t>(read_u16(payload + 6));
    output.offset_y = static_cast<int16_t>(read_u16(payload + 8));
    output.offset_z = static_cast<int16_t>(read_u16(payload + 10));
    return validate(output);
}

ConfigurationCodecStatus decode_apply_config_request(
    const uint8_t* payload,
    size_t payload_size,
    ConfigOperationRequest& output) {
    if (payload == nullptr || payload_size != kApplyConfigRequestBytes) {
        return ConfigurationCodecStatus::InvalidPayloadLength;
    }
    output.operation_id = read_u32(payload);
    output.expected_revision = read_u32(payload + 4);
    if (output.operation_id == 0) {
        return ConfigurationCodecStatus::ZeroOperationId;
    }
    if (output.expected_revision == 0) {
        return ConfigurationCodecStatus::ZeroRevision;
    }
    return decode_configuration(payload + 8, kConfigurationValueBytes,
                                output.configuration);
}

ConfigurationCodecStatus decode_reset_config_request(
    const uint8_t* payload,
    size_t payload_size,
    uint32_t& operation_id,
    uint32_t& expected_revision) {
    if (payload == nullptr || payload_size != kResetConfigRequestBytes) {
        return ConfigurationCodecStatus::InvalidPayloadLength;
    }
    operation_id = read_u32(payload);
    expected_revision = read_u32(payload + 4);
    if (operation_id == 0) return ConfigurationCodecStatus::ZeroOperationId;
    if (expected_revision == 0) return ConfigurationCodecStatus::ZeroRevision;
    return ConfigurationCodecStatus::Ok;
}

ConfigurationCodecStatus encode_get_config_success(
    const ObservedConfiguration& value,
    uint8_t* output,
    size_t output_capacity) {
    if (output == nullptr || output_capacity < kGetConfigResponseBytes) {
        return ConfigurationCodecStatus::OutputTooSmall;
    }
    output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
    return encode_observed(value, output + 1, output_capacity - 1);
}

ConfigurationCodecStatus encode_validate_config_success(
    const ConfigValidationResult& value,
    uint8_t* output,
    size_t output_capacity) {
    if (output == nullptr || output_capacity < kValidateConfigResponseBytes) {
        return ConfigurationCodecStatus::OutputTooSmall;
    }
    output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
    output[1] = static_cast<uint8_t>(value.code);
    output[2] = static_cast<uint8_t>(value.field);
    return ConfigurationCodecStatus::Ok;
}

ConfigurationCodecStatus encode_config_operation_success(
    const ConfigOperationResult& value,
    uint8_t* output,
    size_t output_capacity) {
    if (value.operation_id == 0) {
        return ConfigurationCodecStatus::ZeroOperationId;
    }
    if (output == nullptr || output_capacity < kConfigOperationResponseBytes) {
        return ConfigurationCodecStatus::OutputTooSmall;
    }
    output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
    write_u32(value.operation_id, output + 1);
    output[5] = static_cast<uint8_t>(value.code);
    const auto status = encode_observed(
        value.observed, output + 6, output_capacity - 6);
    if (status != ConfigurationCodecStatus::Ok) return status;
    write_u64(value.effective_sample_seq, output + 24);
    return ConfigurationCodecStatus::Ok;
}

}  // namespace ProtocolV3
