#include <cassert>

#include "protocol_v3/configuration_handler.h"
#include "runtime/operation_mailbox.h"

namespace {

using Mailbox = runtime::OperationMailbox<
    runtime::ConfigurationCommand,
    runtime::ConfigurationResult,
    2,
    2>;
using Handler = ProtocolV3::ConfigurationServiceHandler<Mailbox, 2>;

runtime::ConfigurationResult initial() {
    runtime::ConfigurationResult value{};
    value.revision = 3;
    value.configuration.output_rate_code = 5;
    value.configuration.range_g = 2;
    value.configuration.filter_profile = 1;
    value.configuration.fifo_watermark = 30;
    value.sensor_odr_register = 4;
    value.decimation_factor = 2;
    value.effective_sample_seq = 77;
    return value;
}

void get_and_validate_are_read_only() {
    Mailbox mailbox;
    Handler handler(mailbox, initial());
    uint8_t response[ProtocolV3::kMaxPayloadBytes]{};
    auto handled = handler.handle(
        ProtocolV3::kGetConfigMessageId, nullptr, 0,
        response, sizeof(response));
    assert(handled.payload_size == ProtocolV3::kGetConfigResponseBytes);
    assert(response[0] == 0);
    assert(response[1] == 3);
    assert(mailbox.commands_waiting_approx() == 0);

    ProtocolV3::MeasurementConfiguration invalid{};
    invalid.output_rate = ProtocolV3::OutputRateCode::Hz31_25;
    invalid.fifo_watermark = 30;
    uint8_t request[ProtocolV3::kConfigurationValueBytes]{};
    assert(ProtocolV3::encode_configuration(
               invalid, request, sizeof(request)) ==
           ProtocolV3::ConfigurationCodecStatus::Ok);
    handled = handler.handle(
        ProtocolV3::kValidateConfigMessageId, request, sizeof(request),
        response, sizeof(response));
    assert(handled.payload_size == ProtocolV3::kValidateConfigResponseBytes);
    assert(response[1] == static_cast<uint8_t>(
        ProtocolV3::ConfigValidationCode::UnsupportedOutputRate));
    assert(mailbox.commands_waiting_approx() == 0);
}

void apply_is_idempotent_and_returns_only_core_zero_result() {
    Mailbox mailbox;
    Handler handler(mailbox, initial());
    ProtocolV3::MeasurementConfiguration desired{};
    desired.output_rate = ProtocolV3::OutputRateCode::Hz250;
    desired.range_g = 4;
    desired.fifo_watermark = 30;
    uint8_t request[ProtocolV3::kApplyConfigRequestBytes]{};
    request[0] = 9;
    request[4] = 3;
    assert(ProtocolV3::encode_configuration(
               desired, request + 8, sizeof(request) - 8) ==
           ProtocolV3::ConfigurationCodecStatus::Ok);
    uint8_t response[ProtocolV3::kMaxPayloadBytes]{};
    auto handled = handler.handle(
        ProtocolV3::kApplyConfigMessageId, request, sizeof(request),
        response, sizeof(response));
    assert(handled.payload_size == 1);
    assert(response[0] == static_cast<uint8_t>(ProtocolV3::ResponseStatus::Busy));

    runtime::ConfigurationCommand command{};
    assert(mailbox.try_receive_command(command));
    assert(command.operation_id == 9);
    assert(command.configuration.output_rate_code == 4);
    runtime::ConfigurationResult result = initial();
    result.operation_id = 9;
    result.revision = 4;
    result.configuration = command.configuration;
    result.sensor_odr_register = 3;
    assert(mailbox.try_send_result(result));

    handled = handler.handle(
        ProtocolV3::kApplyConfigMessageId, request, sizeof(request),
        response, sizeof(response));
    assert(handled.payload_size == ProtocolV3::kConfigOperationResponseBytes);
    assert(response[0] == 0);
    assert(response[1] == 9);
    assert(response[6] == 4);
    assert(mailbox.commands_waiting_approx() == 0);

    request[4] = 2;
    handled = handler.handle(
        ProtocolV3::kApplyConfigMessageId, request, sizeof(request),
        response, sizeof(response));
    assert(handled.payload_size == 1);
    assert(response[0] == static_cast<uint8_t>(
        ProtocolV3::ResponseStatus::Conflict));
}

}  // namespace

int main() {
    get_and_validate_are_read_only();
    apply_is_idempotent_and_returns_only_core_zero_result();
    return 0;
}
