#pragma once

#include <cstdint>

#include "runtime/operation_mailbox.h"
#include "runtime/sensor_runtime.h"

namespace runtime {

enum class SystemOperation : uint8_t {
    PrepareFirmwareUpdate = 0,
    ConfirmTrialBoot,
    RestartNode,
};

struct SystemOperationCommand {
    uint32_t operation_id = 0;
    SystemOperation operation = SystemOperation::PrepareFirmwareUpdate;
};

enum class TrialBootConfirmationStatus : uint8_t {
    Confirmed = 0,
    AlreadyConfirmed,
    NotRunningTrial,
    StorageError,
};

enum class FirmwareUpdatePreparationStatus : uint8_t {
    Prepared = 0,
    StorageError,
};

class IFirmwareUpdateControl {
public:
    virtual ~IFirmwareUpdateControl() = default;
    virtual FirmwareUpdatePreparationStatus prepare_firmware_update() = 0;
};

class ITrialBootControl {
public:
    virtual ~ITrialBootControl() = default;
    virtual TrialBootConfirmationStatus confirm_trial_boot() = 0;
};

// Core-0 owner of system-operation admission. It never reboots the MCU. It
// only proves that the sensor lifecycle is safe for the requested transition.
template <typename SensorRuntimeT, typename MailboxT>
class SystemOperationRuntime {
public:
    SystemOperationRuntime(
        SensorRuntimeT& sensor_runtime, MailboxT& mailbox,
        IFirmwareUpdateControl* firmware_update_control = nullptr,
        ITrialBootControl* trial_boot_control = nullptr)
        : sensor_runtime_(sensor_runtime), mailbox_(mailbox),
          firmware_update_control_(firmware_update_control),
          trial_boot_control_(trial_boot_control) {}

    OperationExecutorStatus poll() {
        return executor_.poll(mailbox_, [this](const SystemOperationCommand& command) {
            return handle(command);
        });
    }

private:
    SensorRuntimeResult handle(const SystemOperationCommand& command) const {
        const SensorRuntimeSnapshot snapshot = sensor_runtime_.snapshot();
        SensorRuntimeResultCode code = SensorRuntimeResultCode::InvalidOperation;
        if (command.operation == SystemOperation::PrepareFirmwareUpdate) {
            code = (snapshot.state == SensorRuntimeState::Maintenance ||
                    snapshot.state == SensorRuntimeState::Faulted)
                ? firmware_update_control_ == nullptr
                    ? SensorRuntimeResultCode::InvalidOperation
                    : firmware_update_control_->prepare_firmware_update() ==
                              FirmwareUpdatePreparationStatus::Prepared
                        ? SensorRuntimeResultCode::Ok
                        : SensorRuntimeResultCode::TransitionFailed
                : SensorRuntimeResultCode::InvalidState;
        } else if (command.operation == SystemOperation::ConfirmTrialBoot) {
            if (snapshot.state == SensorRuntimeState::Faulted ||
                snapshot.last_sensor_status != SensorStatus::Ok) {
                code = SensorRuntimeResultCode::Faulted;
            } else if (trial_boot_control_ == nullptr) {
                code = SensorRuntimeResultCode::InvalidOperation;
            } else {
                switch (trial_boot_control_->confirm_trial_boot()) {
                    case TrialBootConfirmationStatus::Confirmed:
                    case TrialBootConfirmationStatus::AlreadyConfirmed:
                        code = SensorRuntimeResultCode::Ok;
                        break;
                    case TrialBootConfirmationStatus::NotRunningTrial:
                        code = SensorRuntimeResultCode::InvalidState;
                        break;
                    case TrialBootConfirmationStatus::StorageError:
                    default:
                        code = SensorRuntimeResultCode::TransitionFailed;
                        break;
                }
            }
        } else if (command.operation == SystemOperation::RestartNode) {
            code = snapshot.state == SensorRuntimeState::Idle ||
                           snapshot.state == SensorRuntimeState::Maintenance ||
                           snapshot.state == SensorRuntimeState::Faulted
                ? SensorRuntimeResultCode::Ok
                : SensorRuntimeResultCode::InvalidState;
        }
        return {
            command.operation_id,
            code,
            snapshot.state,
            snapshot.observability,
            snapshot.last_sensor_status,
        };
    }

    SensorRuntimeT& sensor_runtime_;
    MailboxT& mailbox_;
    IFirmwareUpdateControl* firmware_update_control_ = nullptr;
    ITrialBootControl* trial_boot_control_ = nullptr;
    OperationExecutor<MailboxT, SystemOperationCommand, SensorRuntimeResult>
        executor_{};
};

}  // namespace runtime
