#include <cassert>
#include <cstdint>

#include "protocol_v3/configuration.h"

namespace {

ProtocolV3::MeasurementConfiguration example() {
    return {
        ProtocolV3::OutputRateCode::Hz125,
        2,
        0,
        ProtocolV3::ConfigurationFilterProfile::Balanced,
        30,
        -12,
        34,
        -56,
    };
}

void compact_value_matches_host_layout() {
    const uint8_t expected[ProtocolV3::kConfigurationValueBytes] = {
        1, 5, 2, 0, 1, 30, 0xf4, 0xff, 34, 0, 0xc8, 0xff,
    };
    uint8_t encoded[ProtocolV3::kConfigurationValueBytes]{};
    assert(ProtocolV3::encode_configuration(
               example(), encoded, sizeof(encoded)) ==
           ProtocolV3::ConfigurationCodecStatus::Ok);
    for (size_t i = 0; i < sizeof(expected); ++i) {
        assert(encoded[i] == expected[i]);
    }

    ProtocolV3::MeasurementConfiguration decoded{};
    assert(ProtocolV3::decode_configuration(
               encoded, sizeof(encoded), decoded) ==
           ProtocolV3::ConfigurationCodecStatus::Ok);
    assert(decoded.output_rate == ProtocolV3::OutputRateCode::Hz125);
    assert(decoded.offset_x == -12);
    assert(decoded.offset_y == 34);
    assert(decoded.offset_z == -56);
}

void node_policy_reports_the_specific_invalid_field() {
    auto value = example();
    value.output_rate = ProtocolV3::OutputRateCode::Hz31_25;
    auto result = ProtocolV3::validate_for_node(value);
    assert(result.code ==
           ProtocolV3::ConfigValidationCode::UnsupportedOutputRate);
    assert(result.field == ProtocolV3::ConfigField::OutputRate);

    value = example();
    value.fifo_watermark = 31;
    result = ProtocolV3::validate_for_node(value);
    assert(result.code ==
           ProtocolV3::ConfigValidationCode::InvalidFifoWatermark);
    assert(result.field == ProtocolV3::ConfigField::FifoWatermark);
}

void apply_request_is_correlated_and_revision_guarded() {
    uint8_t payload[ProtocolV3::kApplyConfigRequestBytes] = {
        0x78, 0x56, 0x34, 0x12,
        9, 0, 0, 0,
        1, 5, 2, 0, 1, 30, 0xf4, 0xff, 34, 0, 0xc8, 0xff,
    };
    ProtocolV3::ConfigOperationRequest request{};
    assert(ProtocolV3::decode_apply_config_request(
               payload, sizeof(payload), request) ==
           ProtocolV3::ConfigurationCodecStatus::Ok);
    assert(request.operation_id == 0x12345678u);
    assert(request.expected_revision == 9u);

    payload[0] = payload[1] = payload[2] = payload[3] = 0;
    assert(ProtocolV3::decode_apply_config_request(
               payload, sizeof(payload), request) ==
           ProtocolV3::ConfigurationCodecStatus::ZeroOperationId);
}

void observed_state_must_tell_the_rate_truth() {
    ProtocolV3::ObservedConfiguration observed{
        7, example(), 4, 2,
    };
    uint8_t payload[ProtocolV3::kGetConfigResponseBytes]{};
    assert(ProtocolV3::encode_get_config_success(
               observed, payload, sizeof(payload)) ==
           ProtocolV3::ConfigurationCodecStatus::Ok);

    observed.sensor_odr_register = 5;
    assert(ProtocolV3::encode_get_config_success(
               observed, payload, sizeof(payload)) ==
           ProtocolV3::ConfigurationCodecStatus::InconsistentObservedRate);
}

}  // namespace

int main() {
    compact_value_matches_host_layout();
    node_policy_reports_the_specific_invalid_field();
    apply_request_is_correlated_and_revision_guarded();
    observed_state_must_tell_the_rate_truth();
    return 0;
}
