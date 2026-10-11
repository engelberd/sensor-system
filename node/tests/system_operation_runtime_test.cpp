#include <cassert>

#include "runtime/deferred_system_action.h"
#include "runtime/system_operation_runtime.h"

namespace {

class FakeSensorRuntime {
public:
    runtime::SensorRuntimeSnapshot snapshot() const { return value; }
    runtime::SensorRuntimeSnapshot value{};
};

class FakeTrialBootControl final : public runtime::ITrialBootControl {
public:
    runtime::TrialBootConfirmationStatus confirm_trial_boot() override {
        ++calls;
        return result;
    }
    runtime::TrialBootConfirmationStatus result =
        runtime::TrialBootConfirmationStatus::Confirmed;
    uint32_t calls = 0;
};

class FakeFirmwareUpdateControl final : public runtime::IFirmwareUpdateControl {
public:
    runtime::FirmwareUpdatePreparationStatus prepare_firmware_update() override {
        ++calls;
        return result;
    }
    runtime::FirmwareUpdatePreparationStatus result =
        runtime::FirmwareUpdatePreparationStatus::Prepared;
    uint32_t calls = 0;
};

using Mailbox = runtime::OperationMailbox<
    runtime::SystemOperationCommand,
    runtime::SensorRuntimeResult,
    2,
    2>;

runtime::SensorRuntimeResult execute(
    FakeSensorRuntime& sensor,
    runtime::SensorRuntimeState state,
    uint32_t operation_id) {
    sensor.value.state = state;
    Mailbox mailbox;
    FakeFirmwareUpdateControl control;
    runtime::SystemOperationRuntime<FakeSensorRuntime, Mailbox> system(
        sensor, mailbox, &control);
    assert(mailbox.try_send_command({
        operation_id,
        runtime::SystemOperation::PrepareFirmwareUpdate}));
    assert(system.poll() ==
           runtime::OperationExecutorStatus::CommandExecutedAndPublished);
    runtime::SensorRuntimeResult result{};
    assert(mailbox.try_receive_result(result));
    return result;
}

void test_update_requires_maintenance() {
    FakeSensorRuntime sensor;
    const auto idle = execute(
        sensor, runtime::SensorRuntimeState::Idle, 1);
    assert(idle.code == runtime::SensorRuntimeResultCode::InvalidState);
    assert(idle.state == runtime::SensorRuntimeState::Idle);

    const auto recording = execute(
        sensor, runtime::SensorRuntimeState::Recording, 2);
    assert(recording.code == runtime::SensorRuntimeResultCode::InvalidState);

    const auto maintenance = execute(
        sensor, runtime::SensorRuntimeState::Maintenance, 3);
    assert(maintenance.code == runtime::SensorRuntimeResultCode::Ok);
    assert(maintenance.state == runtime::SensorRuntimeState::Maintenance);

    const auto faulted = execute(
        sensor, runtime::SensorRuntimeState::Faulted, 4);
    assert(faulted.code == runtime::SensorRuntimeResultCode::Faulted);
}

void test_deferred_action_is_bounded_and_consumed_once() {
    runtime::DeferredSystemAction action;
    assert(action.peek() == runtime::DeferredSystemActionKind::None);
    assert(action.request(runtime::DeferredSystemActionKind::EnterBootloader) ==
           runtime::DeferredSystemActionRequestStatus::Accepted);
    assert(action.request(runtime::DeferredSystemActionKind::EnterBootloader) ==
           runtime::DeferredSystemActionRequestStatus::AlreadyRequested);
    runtime::DeferredSystemActionKind consumed =
        runtime::DeferredSystemActionKind::None;
    assert(action.try_consume(consumed));
    assert(consumed == runtime::DeferredSystemActionKind::EnterBootloader);
    assert(!action.try_consume(consumed));
}

void test_trial_confirmation_requires_health_and_maps_storage_result() {
    FakeSensorRuntime sensor;
    sensor.value.state = runtime::SensorRuntimeState::Idle;
    sensor.value.last_sensor_status = SensorStatus::Ok;
    Mailbox mailbox;
    FakeTrialBootControl control;
    runtime::SystemOperationRuntime<FakeSensorRuntime, Mailbox> system(
        sensor, mailbox, nullptr, &control);
    assert(mailbox.try_send_command({
        10, runtime::SystemOperation::ConfirmTrialBoot}));
    assert(system.poll() ==
           runtime::OperationExecutorStatus::CommandExecutedAndPublished);
    runtime::SensorRuntimeResult result{};
    assert(mailbox.try_receive_result(result));
    assert(result.code == runtime::SensorRuntimeResultCode::Ok);
    assert(control.calls == 1);

    control.result = runtime::TrialBootConfirmationStatus::StorageError;
    assert(mailbox.try_send_command({
        11, runtime::SystemOperation::ConfirmTrialBoot}));
    assert(system.poll() ==
           runtime::OperationExecutorStatus::CommandExecutedAndPublished);
    assert(mailbox.try_receive_result(result));
    assert(result.code == runtime::SensorRuntimeResultCode::TransitionFailed);

    sensor.value.last_sensor_status = SensorStatus::InternalError;
    assert(mailbox.try_send_command({
        12, runtime::SystemOperation::ConfirmTrialBoot}));
    assert(system.poll() ==
           runtime::OperationExecutorStatus::CommandExecutedAndPublished);
    assert(mailbox.try_receive_result(result));
    assert(result.code == runtime::SensorRuntimeResultCode::Faulted);
    assert(control.calls == 2);
}

}  // namespace

int main() {
    test_update_requires_maintenance();
    test_deferred_action_is_bounded_and_consumed_once();
    test_trial_confirmation_requires_health_and_maps_storage_result();
    return 0;
}
