#pragma once

#include <cstdint>

#include "common/sensor_types.h"

namespace runtime {

enum class SensorRuntimeState : uint8_t {
    Idle = 0,
    Recording,
    Stopping,
    Maintenance,
    Faulted,
};

enum class ObservabilityProfile : uint8_t {
    Production = 0,
    Diagnostic,
};

enum class SensorRuntimeOperation : uint8_t {
    GetState = 0,
    Start,
    Stop,
    EnterMaintenance,
    ExitMaintenance,
    SetObservabilityProfile,
};

enum class SensorRuntimeResultCode : uint8_t {
    Ok = 0,
    InvalidOperation,
    InvalidState,
    InvalidArgument,
    InProgress,
    TransitionFailed,
    Faulted,
};

struct SensorRuntimeCommand {
    uint32_t operation_id = 0;
    SensorRuntimeOperation operation = SensorRuntimeOperation::GetState;
    ObservabilityProfile requested_profile =
        ObservabilityProfile::Production;
};

struct SensorRuntimeResult {
    uint32_t operation_id = 0;
    SensorRuntimeResultCode code = SensorRuntimeResultCode::Ok;
    SensorRuntimeState state = SensorRuntimeState::Idle;
    ObservabilityProfile observability = ObservabilityProfile::Production;
    SensorStatus sensor_status = SensorStatus::Ok;
};

struct SensorRuntimeSnapshot {
    SensorRuntimeState state = SensorRuntimeState::Idle;
    ObservabilityProfile observability = ObservabilityProfile::Production;
    SensorStatus last_sensor_status = SensorStatus::Ok;
    uint32_t start_count = 0;
    uint32_t stop_count = 0;
    uint32_t maintenance_entry_count = 0;
    uint32_t profile_change_count = 0;
    uint32_t update_count = 0;
    uint32_t fault_count = 0;
};

// Core-0 use-case owner. Acquisition and ingress adapter are injected so this
// state machine contains no board, protocol or transport policy.
template <typename Acquisition, typename MeasurementSink>
class SensorRuntime {
public:
    SensorRuntime(Acquisition& acquisition, MeasurementSink& measurement_sink)
        : acquisition_(acquisition), measurement_sink_(measurement_sink) {
        acquisition_.pause();
    }

    SensorRuntime(const SensorRuntime&) = delete;
    SensorRuntime& operator=(const SensorRuntime&) = delete;

    SensorRuntimeResult handle(const SensorRuntimeCommand& command) {
        switch (command.operation) {
            case SensorRuntimeOperation::GetState:
                return result(command.operation_id, SensorRuntimeResultCode::Ok);
            case SensorRuntimeOperation::Start:
                return start(command.operation_id);
            case SensorRuntimeOperation::Stop:
                return stop(command.operation_id);
            case SensorRuntimeOperation::EnterMaintenance:
                return enter_maintenance(command.operation_id);
            case SensorRuntimeOperation::ExitMaintenance:
                return exit_maintenance(command.operation_id);
            case SensorRuntimeOperation::SetObservabilityProfile:
                return set_observability_profile(
                    command.operation_id,
                    command.requested_profile
                );
            default:
                return result(
                    command.operation_id,
                    SensorRuntimeResultCode::InvalidOperation
                );
        }
    }

    void poll() {
        service_ingress_health();
        if (state_ == SensorRuntimeState::Stopping) {
            progress_stop();
            return;
        }
        if (state_ != SensorRuntimeState::Recording) return;

        acquisition_.update();
        saturating_increment(update_count_);
        service_ingress_health();
    }

    SensorRuntimeSnapshot snapshot() const {
        return {
            state_,
            observability_,
            last_sensor_status_,
            start_count_,
            stop_count_,
            maintenance_entry_count_,
            profile_change_count_,
            update_count_,
            fault_count_,
        };
    }

    void fail(SensorStatus status = SensorStatus::InternalError) {
        if (state_ != SensorRuntimeState::Faulted) enter_fault(status);
    }

private:
    static void saturating_increment(uint32_t& value) {
        if (value != UINT32_MAX) ++value;
    }

    SensorRuntimeResult result(uint32_t operation_id,
                               SensorRuntimeResultCode code) const {
        return {
            operation_id,
            code,
            state_,
            observability_,
            last_sensor_status_,
        };
    }

    static bool known_profile(ObservabilityProfile profile) {
        return profile == ObservabilityProfile::Production ||
               profile == ObservabilityProfile::Diagnostic;
    }

    SensorRuntimeResult start(uint32_t operation_id) {
        if (state_ == SensorRuntimeState::Faulted) {
            return result(operation_id, SensorRuntimeResultCode::Faulted);
        }
        if (state_ == SensorRuntimeState::Recording) {
            return result(operation_id, SensorRuntimeResultCode::Ok);
        }
        if (state_ == SensorRuntimeState::Stopping ||
            state_ == SensorRuntimeState::Maintenance) {
            return result(operation_id, SensorRuntimeResultCode::InvalidState);
        }
        if (measurement_sink_.state().faulted) {
            enter_fault(SensorStatus::InternalError);
            return result(operation_id, SensorRuntimeResultCode::Faulted);
        }

        const SensorStatus status = acquisition_.prepare_for_runtime();
        last_sensor_status_ = status;
        if (status != SensorStatus::Ok) {
            enter_fault(status);
            return result(
                operation_id,
                SensorRuntimeResultCode::TransitionFailed
            );
        }

        acquisition_.resume();
        state_ = SensorRuntimeState::Recording;
        saturating_increment(start_count_);
        return result(operation_id, SensorRuntimeResultCode::Ok);
    }

    SensorRuntimeResult stop(uint32_t operation_id) {
        if (state_ == SensorRuntimeState::Faulted) {
            return result(operation_id, SensorRuntimeResultCode::Faulted);
        }
        if (state_ == SensorRuntimeState::Idle ||
            state_ == SensorRuntimeState::Maintenance) {
            return result(operation_id, SensorRuntimeResultCode::Ok);
        }
        if (state_ == SensorRuntimeState::Stopping) {
            progress_stop();
            return result(
                operation_id,
                state_ == SensorRuntimeState::Idle
                    ? SensorRuntimeResultCode::Ok
                    : state_ == SensorRuntimeState::Faulted
                        ? SensorRuntimeResultCode::Faulted
                        : SensorRuntimeResultCode::InProgress
            );
        }

        const SensorStatus status = acquisition_.finish_recording();
        last_sensor_status_ = status;
        if (status != SensorStatus::Ok) {
            enter_fault(status);
            return result(operation_id, SensorRuntimeResultCode::TransitionFailed);
        }
        state_ = SensorRuntimeState::Stopping;
        progress_stop();
        return result(
            operation_id,
            state_ == SensorRuntimeState::Idle
                ? SensorRuntimeResultCode::Ok
                : state_ == SensorRuntimeState::Faulted
                    ? SensorRuntimeResultCode::Faulted
                    : SensorRuntimeResultCode::InProgress
        );
    }

    SensorRuntimeResult enter_maintenance(uint32_t operation_id) {
        if (state_ == SensorRuntimeState::Faulted) {
            return result(operation_id, SensorRuntimeResultCode::Faulted);
        }
        if (state_ == SensorRuntimeState::Recording ||
            state_ == SensorRuntimeState::Stopping) {
            return result(operation_id, SensorRuntimeResultCode::InvalidState);
        }
        if (state_ == SensorRuntimeState::Maintenance) {
            return result(operation_id, SensorRuntimeResultCode::Ok);
        }
        state_ = SensorRuntimeState::Maintenance;
        saturating_increment(maintenance_entry_count_);
        return result(operation_id, SensorRuntimeResultCode::Ok);
    }

    SensorRuntimeResult exit_maintenance(uint32_t operation_id) {
        if (state_ == SensorRuntimeState::Faulted) {
            return result(operation_id, SensorRuntimeResultCode::Faulted);
        }
        if (state_ == SensorRuntimeState::Recording ||
            state_ == SensorRuntimeState::Stopping) {
            return result(operation_id, SensorRuntimeResultCode::InvalidState);
        }
        if (state_ == SensorRuntimeState::Idle) {
            return result(operation_id, SensorRuntimeResultCode::Ok);
        }
        state_ = SensorRuntimeState::Idle;
        return result(operation_id, SensorRuntimeResultCode::Ok);
    }

    SensorRuntimeResult set_observability_profile(
        uint32_t operation_id,
        ObservabilityProfile profile
    ) {
        if (state_ == SensorRuntimeState::Faulted) {
            return result(operation_id, SensorRuntimeResultCode::Faulted);
        }
        if (!known_profile(profile)) {
            return result(operation_id, SensorRuntimeResultCode::InvalidArgument);
        }
        if (observability_ != profile) {
            observability_ = profile;
            saturating_increment(profile_change_count_);
        }
        return result(operation_id, SensorRuntimeResultCode::Ok);
    }

    void service_ingress_health() {
        (void)measurement_sink_.flush_pending();
        if (measurement_sink_.state().faulted &&
            state_ != SensorRuntimeState::Faulted) {
            enter_fault(SensorStatus::InternalError);
        }
    }

    void progress_stop() {
        if (state_ != SensorRuntimeState::Stopping) return;
        (void)measurement_sink_.flush_pending();
        if (measurement_sink_.state().faulted) {
            enter_fault(SensorStatus::InternalError);
            return;
        }
        if (measurement_sink_.settled()) {
            state_ = SensorRuntimeState::Idle;
            saturating_increment(stop_count_);
        }
    }

    void enter_fault(SensorStatus status) {
        acquisition_.pause();
        last_sensor_status_ = status;
        state_ = SensorRuntimeState::Faulted;
        saturating_increment(fault_count_);
    }

    Acquisition& acquisition_;
    MeasurementSink& measurement_sink_;
    SensorRuntimeState state_ = SensorRuntimeState::Idle;
    ObservabilityProfile observability_ = ObservabilityProfile::Production;
    SensorStatus last_sensor_status_ = SensorStatus::Ok;
    uint32_t start_count_ = 0;
    uint32_t stop_count_ = 0;
    uint32_t maintenance_entry_count_ = 0;
    uint32_t profile_change_count_ = 0;
    uint32_t update_count_ = 0;
    uint32_t fault_count_ = 0;
};

}  // namespace runtime
