#include <cassert>
#include <cstddef>
#include <cstdint>

#include "protocol_v3/lifecycle.h"
#include "protocol_v3/lifecycle_handler.h"
#include "runtime/operation_mailbox.h"

namespace {
using namespace ProtocolV3;

using Mailbox = runtime::OperationMailbox<
    runtime::SensorRuntimeCommand,
    runtime::SensorRuntimeResult,
    2,
    2>;
using SystemMailbox = runtime::OperationMailbox<
    runtime::SystemOperationCommand,
    runtime::SensorRuntimeResult,
    2,
    2>;
using Handler = LifecycleServiceHandler<Mailbox, SystemMailbox, 2>;

LifecycleHandlerResult call(
    Handler& handler,
    const runtime::SensorRuntimeCommand& command,
    uint8_t* response,
    size_t response_capacity) {
    uint8_t request[kLifecycleProfileRequestBytes]{};
    uint8_t message = 0;
    size_t request_size = 0;
    assert(encode_lifecycle_request(
               command, request, sizeof(request), request_size, message) ==
           LifecycleCodecStatus::Ok);
    return handler.handle(
        message, request, request_size, response, response_capacity);
}

runtime::SensorRuntimeResult decode_success(
    const uint8_t* payload, size_t payload_size) {
    runtime::SensorRuntimeResult result{};
    ResponseStatus remote = ResponseStatus::InternalError;
    assert(decode_lifecycle_response(
               payload, payload_size, result, remote) ==
           LifecycleCodecStatus::Ok);
    assert(remote == ResponseStatus::Ok);
    return result;
}

LifecycleHandlerResult call_prepare_update(
    Handler& handler,
    uint32_t operation_id,
    uint8_t* response,
    size_t response_capacity) {
    uint8_t request[kLifecycleOperationRequestBytes]{};
    assert(encode_prepare_firmware_update_request(
               operation_id, request, sizeof(request)) ==
           LifecycleCodecStatus::Ok);
    return handler.handle(
        kPrepareFirmwareUpdateMessageId,
        request,
        sizeof(request),
        response,
        response_capacity);
}

LifecycleHandlerResult call_confirm_trial(
    Handler& handler,
    uint32_t operation_id,
    uint8_t* response,
    size_t response_capacity) {
    uint8_t request[kLifecycleOperationRequestBytes]{};
    assert(encode_confirm_trial_boot_request(
               operation_id, request, sizeof(request)) ==
           LifecycleCodecStatus::Ok);
    return handler.handle(
        kConfirmTrialBootMessageId,
        request,
        sizeof(request),
        response,
        response_capacity);
}

LifecycleHandlerResult call_restart(
    Handler& handler,
    uint32_t operation_id,
    uint8_t* response,
    size_t response_capacity) {
    uint8_t request[kLifecycleOperationRequestBytes]{};
    assert(encode_restart_node_request(
               operation_id, request, sizeof(request)) ==
           LifecycleCodecStatus::Ok);
    return handler.handle(
        kRestartNodeMessageId,
        request,
        sizeof(request),
        response,
        response_capacity);
}

void test_codec_round_trip_and_validation() {
    const runtime::SensorRuntimeCommand command{
        0x12345678u,
        runtime::SensorRuntimeOperation::SetObservabilityProfile,
        runtime::ObservabilityProfile::Diagnostic,
    };
    uint8_t payload[kLifecycleProfileRequestBytes]{};
    uint8_t message = 0;
    size_t size = 0;
    assert(encode_lifecycle_request(
               command, payload, sizeof(payload), size, message) ==
           LifecycleCodecStatus::Ok);
    assert(message == kSetObservabilityMessageId);
    assert(size == kLifecycleProfileRequestBytes);

    runtime::SensorRuntimeCommand decoded{};
    assert(decode_lifecycle_request(message, payload, size, decoded) ==
           LifecycleCodecStatus::Ok);
    assert(decoded.operation_id == command.operation_id);
    assert(decoded.operation == command.operation);
    assert(decoded.requested_profile == command.requested_profile);

    payload[4] = 99;
    assert(decode_lifecycle_request(message, payload, size, decoded) ==
           LifecycleCodecStatus::UnknownObservabilityProfile);
    assert(decode_lifecycle_request(99, payload, size, decoded) ==
           LifecycleCodecStatus::UnknownMessage);
}

void test_retry_does_not_duplicate_and_terminal_result_is_cached() {
    Mailbox mailbox;
    SystemMailbox system_mailbox;
    runtime::DeferredSystemAction system_action;
    Handler handler(mailbox, system_mailbox, system_action);
    const runtime::SensorRuntimeCommand start{
        10, runtime::SensorRuntimeOperation::Start};
    uint8_t response[kMaxPayloadBytes]{};

    LifecycleHandlerResult handled = call(
        handler, start, response, sizeof(response));
    assert(handled.payload_size == kLifecycleResponseBytes);
    assert(decode_success(response, handled.payload_size).code ==
           runtime::SensorRuntimeResultCode::InProgress);
    assert(mailbox.commands_waiting_approx() == 1);

    handled = call(handler, start, response, sizeof(response));
    assert(decode_success(response, handled.payload_size).code ==
           runtime::SensorRuntimeResultCode::InProgress);
    assert(mailbox.commands_waiting_approx() == 1);

    const runtime::SensorRuntimeCommand stop{
        11, runtime::SensorRuntimeOperation::Stop};
    handled = call(handler, stop, response, sizeof(response));
    assert(handled.payload_size == 1);
    assert(response[0] == static_cast<uint8_t>(ResponseStatus::Busy));

    runtime::SensorRuntimeCommand executed{};
    assert(mailbox.try_receive_command(executed));
    assert(executed.operation_id == start.operation_id);
    assert(mailbox.try_send_result({
        start.operation_id,
        runtime::SensorRuntimeResultCode::Ok,
        runtime::SensorRuntimeState::Recording,
        runtime::ObservabilityProfile::Production,
        SensorStatus::Ok,
    }));

    handled = call(handler, start, response, sizeof(response));
    const runtime::SensorRuntimeResult completed =
        decode_success(response, handled.payload_size);
    assert(completed.code == runtime::SensorRuntimeResultCode::Ok);
    assert(completed.state == runtime::SensorRuntimeState::Recording);
    assert(mailbox.commands_waiting_approx() == 0);

    handled = call(handler, start, response, sizeof(response));
    assert(decode_success(response, handled.payload_size).code ==
           runtime::SensorRuntimeResultCode::Ok);
    assert(mailbox.commands_waiting_approx() == 0);

    const runtime::SensorRuntimeCommand conflicting{
        start.operation_id, runtime::SensorRuntimeOperation::Stop};
    handled = call(handler, conflicting, response, sizeof(response));
    assert(handled.payload_size == 1);
    assert(response[0] == static_cast<uint8_t>(ResponseStatus::Conflict));
}

void test_in_progress_result_is_reconciled_until_terminal() {
    Mailbox mailbox;
    SystemMailbox system_mailbox;
    runtime::DeferredSystemAction system_action;
    Handler handler(mailbox, system_mailbox, system_action);
    const runtime::SensorRuntimeCommand stop{
        20, runtime::SensorRuntimeOperation::Stop};
    uint8_t response[kMaxPayloadBytes]{};
    LifecycleHandlerResult handled = call(
        handler, stop, response, sizeof(response));
    assert(handled.payload_size == kLifecycleResponseBytes);

    runtime::SensorRuntimeCommand executed{};
    assert(mailbox.try_receive_command(executed));
    assert(mailbox.try_send_result({
        stop.operation_id,
        runtime::SensorRuntimeResultCode::InProgress,
        runtime::SensorRuntimeState::Stopping,
        runtime::ObservabilityProfile::Production,
        SensorStatus::Ok,
    }));

    handled = call(handler, stop, response, sizeof(response));
    runtime::SensorRuntimeResult result =
        decode_success(response, handled.payload_size);
    assert(result.code == runtime::SensorRuntimeResultCode::InProgress);
    assert(result.state == runtime::SensorRuntimeState::Stopping);
    assert(mailbox.commands_waiting_approx() == 1);

    assert(mailbox.try_receive_command(executed));
    assert(mailbox.try_send_result({
        stop.operation_id,
        runtime::SensorRuntimeResultCode::Ok,
        runtime::SensorRuntimeState::Idle,
        runtime::ObservabilityProfile::Production,
        SensorStatus::Ok,
    }));
    assert(handler.poll_result());

    handled = call(handler, stop, response, sizeof(response));
    result = decode_success(response, handled.payload_size);
    assert(result.code == runtime::SensorRuntimeResultCode::Ok);
    assert(result.state == runtime::SensorRuntimeState::Idle);
    assert(handler.state().completed_operations == 1);
    assert(handler.state().in_progress_results == 1);
}

void test_unexpected_result_fails_closed() {
    Mailbox mailbox;
    SystemMailbox system_mailbox;
    runtime::DeferredSystemAction system_action;
    Handler handler(mailbox, system_mailbox, system_action);
    assert(mailbox.try_send_result({
        99,
        runtime::SensorRuntimeResultCode::Ok,
        runtime::SensorRuntimeState::Idle,
        runtime::ObservabilityProfile::Production,
        SensorStatus::Ok,
    }));
    assert(handler.poll_result());
    assert(handler.state().faulted);
    assert(handler.state().unexpected_results == 1);

    uint8_t response[kMaxPayloadBytes]{};
    const LifecycleHandlerResult handled = call(
        handler,
        {1, runtime::SensorRuntimeOperation::GetState},
        response,
        sizeof(response));
    assert(handled.payload_size == 1);
    assert(response[0] == static_cast<uint8_t>(ResponseStatus::InternalError));
}

void test_update_action_is_armed_only_after_successful_transmission() {
    Mailbox mailbox;
    SystemMailbox system_mailbox;
    runtime::DeferredSystemAction system_action;
    Handler handler(mailbox, system_mailbox, system_action);
    uint8_t response[kMaxPayloadBytes]{};

    LifecycleHandlerResult handled = call_prepare_update(
        handler, 50, response, sizeof(response));
    assert(handled.payload_size == kLifecycleResponseBytes);
    assert(!handled.post_transmit_required);
    assert(decode_success(response, handled.payload_size).code ==
           runtime::SensorRuntimeResultCode::InProgress);

    runtime::SystemOperationCommand command{};
    assert(system_mailbox.try_receive_command(command));
    assert(command.operation_id == 50);
    assert(system_mailbox.try_send_result({
        50,
        runtime::SensorRuntimeResultCode::Ok,
        runtime::SensorRuntimeState::Maintenance,
        runtime::ObservabilityProfile::Production,
        SensorStatus::Ok,
    }));

    handled = call_prepare_update(handler, 50, response, sizeof(response));
    assert(decode_success(response, handled.payload_size).code ==
           runtime::SensorRuntimeResultCode::Ok);
    assert(handled.post_transmit_required);
    const uint64_t token = handled.post_transmit_token;
    assert(token != 0);

    assert(handler.settle_transmission(token, false));
    assert(system_action.peek() == runtime::DeferredSystemActionKind::None);
    handled = call_prepare_update(handler, 50, response, sizeof(response));
    assert(handled.post_transmit_required);
    assert(handled.post_transmit_token == token);

    assert(handler.settle_transmission(token, true));
    assert(system_action.peek() ==
           runtime::DeferredSystemActionKind::EnterBootloader);
    handled = call_prepare_update(handler, 50, response, sizeof(response));
    assert(!handled.post_transmit_required);
    assert(handler.state().update_actions_armed == 1);
    assert(system_mailbox.commands_waiting_approx() == 0);
}

void test_rejected_update_never_creates_post_transmit_action() {
    Mailbox mailbox;
    SystemMailbox system_mailbox;
    runtime::DeferredSystemAction system_action;
    Handler handler(mailbox, system_mailbox, system_action);
    uint8_t response[kMaxPayloadBytes]{};
    (void)call_prepare_update(handler, 60, response, sizeof(response));
    runtime::SystemOperationCommand command{};
    assert(system_mailbox.try_receive_command(command));
    assert(system_mailbox.try_send_result({
        60,
        runtime::SensorRuntimeResultCode::InvalidState,
        runtime::SensorRuntimeState::Idle,
        runtime::ObservabilityProfile::Production,
        SensorStatus::Ok,
    }));
    const LifecycleHandlerResult handled = call_prepare_update(
        handler, 60, response, sizeof(response));
    assert(!handled.post_transmit_required);
    assert(decode_success(response, handled.payload_size).code ==
           runtime::SensorRuntimeResultCode::InvalidState);
    assert(system_action.peek() == runtime::DeferredSystemActionKind::None);
}

void test_faulted_update_and_restart_arm_distinct_post_transmit_actions() {
    Mailbox mailbox;
    SystemMailbox system_mailbox;
    runtime::DeferredSystemAction system_action;
    Handler handler(mailbox, system_mailbox, system_action);
    uint8_t response[kMaxPayloadBytes]{};

    (void)call_prepare_update(handler, 70, response, sizeof(response));
    runtime::SystemOperationCommand command{};
    assert(system_mailbox.try_receive_command(command));
    assert(command.operation == runtime::SystemOperation::PrepareFirmwareUpdate);
    assert(system_mailbox.try_send_result({
        70,
        runtime::SensorRuntimeResultCode::Ok,
        runtime::SensorRuntimeState::Faulted,
        runtime::ObservabilityProfile::Production,
        SensorStatus::InvalidSample,
    }));
    auto handled = call_prepare_update(handler, 70, response, sizeof(response));
    assert(handled.post_transmit_required);
    assert(handler.settle_transmission(handled.post_transmit_token, true));
    assert(system_action.peek() ==
           runtime::DeferredSystemActionKind::EnterBootloader);
    runtime::DeferredSystemActionKind consumed{};
    assert(system_action.try_consume(consumed));

    handled = call_restart(handler, 71, response, sizeof(response));
    assert(decode_success(response, handled.payload_size).code ==
           runtime::SensorRuntimeResultCode::InProgress);
    assert(system_mailbox.try_receive_command(command));
    assert(command.operation == runtime::SystemOperation::RestartNode);
    assert(system_mailbox.try_send_result({
        71,
        runtime::SensorRuntimeResultCode::Ok,
        runtime::SensorRuntimeState::Faulted,
        runtime::ObservabilityProfile::Production,
        SensorStatus::InvalidSample,
    }));
    handled = call_restart(handler, 71, response, sizeof(response));
    assert(handled.post_transmit_required);
    assert(handler.settle_transmission(handled.post_transmit_token, true));
    assert(system_action.peek() ==
           runtime::DeferredSystemActionKind::RestartNode);
    assert(handler.state().update_actions_armed == 1);
    assert(handler.state().restart_actions_armed == 1);
}

void test_trial_confirmation_is_a_cached_system_operation_without_reboot() {
    Mailbox mailbox;
    SystemMailbox system_mailbox;
    runtime::DeferredSystemAction system_action;
    Handler handler(mailbox, system_mailbox, system_action);
    uint8_t response[kMaxPayloadBytes]{};

    auto handled = call_confirm_trial(handler, 80, response, sizeof(response));
    assert(decode_success(response, handled.payload_size).code ==
           runtime::SensorRuntimeResultCode::InProgress);
    runtime::SystemOperationCommand command{};
    assert(system_mailbox.try_receive_command(command));
    assert(command.operation == runtime::SystemOperation::ConfirmTrialBoot);
    assert(system_mailbox.try_send_result({
        80,
        runtime::SensorRuntimeResultCode::Ok,
        runtime::SensorRuntimeState::Idle,
        runtime::ObservabilityProfile::Production,
        SensorStatus::Ok,
    }));

    handled = call_confirm_trial(handler, 80, response, sizeof(response));
    assert(decode_success(response, handled.payload_size).code ==
           runtime::SensorRuntimeResultCode::Ok);
    assert(!handled.post_transmit_required);
    assert(system_action.peek() == runtime::DeferredSystemActionKind::None);

    handled = call_confirm_trial(handler, 80, response, sizeof(response));
    assert(decode_success(response, handled.payload_size).code ==
           runtime::SensorRuntimeResultCode::Ok);
    assert(system_mailbox.commands_waiting_approx() == 0);
}

}  // namespace

int main() {
    test_codec_round_trip_and_validation();
    test_retry_does_not_duplicate_and_terminal_result_is_cached();
    test_in_progress_result_is_reconciled_until_terminal();
    test_unexpected_result_fails_closed();
    test_update_action_is_armed_only_after_successful_transmission();
    test_rejected_update_never_creates_post_transmit_action();
    test_faulted_update_and_restart_arm_distinct_post_transmit_actions();
    test_trial_confirmation_is_a_cached_system_operation_without_reboot();
    return 0;
}
