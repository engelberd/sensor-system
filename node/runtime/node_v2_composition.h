#pragma once

#include <cstdint>

#include "protocol_v3/dispatcher.h"
#include "protocol_v3/diagnostics.h"
#include "runtime/core0_sensor_runtime.h"
#include "runtime/core1_protocol_runtime.h"
#include "runtime/node_v2_runtime_types.h"

namespace node_v2 {

// Owns the single SPSC data boundary. It is constructed before AcquisitionEngine
// so the engine can receive sink() without either object knowing the rest of the
// runtime composition.
class DataPlane {
public:
    explicit DataPlane(uint32_t config_revision)
        : ingress_(queue_), sink_(ingress_, config_revision) {}

    DataPlane(const DataPlane&) = delete;
    DataPlane& operator=(const DataPlane&) = delete;

    IngressQueue& queue() { return queue_; }
    SampleIngress& ingress() { return ingress_; }
    MeasurementSink& sink() { return sink_; }
    const MeasurementSink& sink() const { return sink_; }

private:
    IngressQueue queue_{};
    SampleIngress ingress_;
    MeasurementSink sink_;
};

template <typename Acquisition, typename Port, typename Observer,
          typename ConfigManager>
class RuntimeComposition {
public:
    using SensorRuntime = runtime::SensorRuntime<Acquisition, MeasurementSink>;
    using Core0Runtime = runtime::Core0SensorRuntime<
        SensorRuntime,
        OperationMailbox>;
    using SystemRuntime = runtime::SystemOperationRuntime<
        SensorRuntime,
        SystemOperationMailbox>;
    using ConfigurationRuntime = runtime::ConfigurationRuntime<
        SensorRuntime, Acquisition, ConfigManager, MeasurementSink>;
    using CommissioningRuntime = runtime::CommissioningRuntime<ConfigManager>;
    using Endpoint = TransportEndpoint<Port>;
    using Core1Runtime = runtime::Core1ProtocolRuntime<
        Core1MeasurementRuntime,
        LifecycleService,
        Endpoint,
        Observer>;

    RuntimeComposition(DataPlane& data_plane,
                       Acquisition& acquisition,
                       ConfigManager& config_manager,
                       runtime::ICommissioningBootSettings& commissioning_boot,
                       Port& port,
                       Observer& observer,
                       ProtocolV3::NodeIdentity identity,
                       ProtocolV3::NodeCapabilities capabilities,
                       uint64_t boot_epoch,
                       ProtocolV3::TransportEndpointPolicy transport_policy = {},
                       runtime::Core1ProtocolPolicy core1_policy = {
                           Profile::kIngressDrainBudgetPerPoll},
                       runtime::IFirmwareUpdateControl* firmware_update_control = nullptr,
                       runtime::ITrialBootControl* trial_boot_control = nullptr,
                       ProtocolV3::IDiagnosticsServiceHandler* diagnostics_handler = nullptr,
                       ProtocolV3::ITelemetryServiceHandler* telemetry_handler = nullptr)
        : data_plane_(data_plane),
          sensor_runtime_(acquisition, data_plane.sink()),
          core0_(sensor_runtime_, mailbox_, fault_latch_),
          configuration_runtime_(sensor_runtime_, acquisition, config_manager,
                                 data_plane.sink()),
          commissioning_runtime_(config_manager, commissioning_boot),
          system_runtime_(sensor_runtime_, system_mailbox_,
                          firmware_update_control, trial_boot_control),
          core1_measurement_(data_plane.queue(), boot_epoch, fault_latch_),
          lifecycle_(mailbox_, system_mailbox_, system_action_),
          configuration_(configuration_mailbox_,
                         initial_configuration(config_manager, acquisition)),
          commissioning_(commissioning_mailbox_, system_action_,
                         commissioning_identity(identity)),
          dispatcher_(identity, capabilities,
                      &core1_measurement_.handler(), &lifecycle_,
                      diagnostics_handler, &configuration_, &commissioning_,
                      telemetry_handler),
          endpoint_(port, dispatcher_, transport_policy),
          core1_(core1_measurement_, lifecycle_, endpoint_, observer,
                 core1_policy),
          identity_(identity),
          capabilities_(capabilities) {}

    RuntimeComposition(const RuntimeComposition&) = delete;
    RuntimeComposition& operator=(const RuntimeComposition&) = delete;

    bool configuration_valid() const {
        const bool identity_valid = identity_.node_address == 0 ||
            ProtocolV3::validate(identity_) ==
                ProtocolV3::IdentityCodecStatus::Ok;
        return identity_valid &&
               ProtocolV3::validate(capabilities_) ==
                   ProtocolV3::IdentityCodecStatus::Ok &&
               endpoint_.valid_policy() && core1_.policy().valid() &&
               !data_plane_.sink().state().faulted;
    }

    // Keeps protocol 5 alive when sensor initialization fails. Diagnostics,
    // configuration repair, firmware update and RS485 restart remain
    // reachable; measurement and Start fail closed through SensorRuntime.
    void enter_service_safe_fault(SensorStatus status) {
        sensor_runtime_.fail(status);
    }

    Core0Runtime& core0() { return core0_; }
    void poll_core0() {
        core0_.poll();
        (void)configuration_executor_.poll(
            configuration_mailbox_,
            [this](const runtime::ConfigurationCommand& command) {
                return configuration_runtime_.handle(command);
            });
        (void)commissioning_executor_.poll(
            commissioning_mailbox_,
            [this](const runtime::CommissioningCommand& command) {
                return commissioning_runtime_.handle(command);
            });
        (void)system_runtime_.poll();
    }
    Core1Runtime& core1() { return core1_; }
    Core1MeasurementRuntime& measurement() { return core1_measurement_; }
    LifecycleService& lifecycle() { return lifecycle_; }
    ConfigurationService& configuration() { return configuration_; }
    CommissioningService& commissioning() { return commissioning_; }
    ProtocolV3::ApplicationDispatcher& dispatcher() { return dispatcher_; }
    Endpoint& endpoint() { return endpoint_; }
    OperationMailbox& mailbox() { return mailbox_; }
    SystemOperationMailbox& system_mailbox() { return system_mailbox_; }
    ConfigurationMailbox& configuration_mailbox() {
        return configuration_mailbox_;
    }
    CommissioningMailbox& commissioning_mailbox() {
        return commissioning_mailbox_;
    }
    runtime::DeferredSystemAction& system_action() { return system_action_; }
    runtime::RuntimeFaultLatch& fault_latch() { return fault_latch_; }

private:
    static ProtocolV3::CommissioningIdentity commissioning_identity(
        const ProtocolV3::NodeIdentity& identity) {
        ProtocolV3::CommissioningIdentity output{};
        for (size_t index = 0; index < ProtocolV3::kHardwareIdBytes; ++index) {
            output.hardware_id[index] = identity.hardware_id[index];
        }
        output.board_revision = identity.board_revision;
        output.firmware = identity.firmware;
        output.framing_version = identity.framing_version;
        output.application_version = identity.application_version;
        return output;
    }

    static runtime::ConfigurationResult initial_configuration(
        const ConfigManager& config_manager,
        const Acquisition& acquisition) {
        const auto config = runtime::runtime_configuration_from_device(
            config_manager.current());
        uint16_t unused = 0;
        uint8_t sensor_odr = UINT8_MAX;
        (void)runtime::output_rate_to_sensor(
            config.output_rate_code, unused, sensor_odr);
        return {
            0,
            runtime::ConfigurationResultCode::Applied,
            config_manager.generation(),
            config,
            sensor_odr,
            2,
            acquisition.stats().next_sample_seq,
        };
    }

    DataPlane& data_plane_;
    SensorRuntime sensor_runtime_;
    OperationMailbox mailbox_{};
    ConfigurationMailbox configuration_mailbox_{};
    CommissioningMailbox commissioning_mailbox_{};
    SystemOperationMailbox system_mailbox_{};
    runtime::DeferredSystemAction system_action_{};
    runtime::RuntimeFaultLatch fault_latch_{};
    Core0Runtime core0_;
    ConfigurationRuntime configuration_runtime_;
    CommissioningRuntime commissioning_runtime_;
    runtime::OperationExecutor<
        ConfigurationMailbox,
        runtime::ConfigurationCommand,
        runtime::ConfigurationResult> configuration_executor_{};
    runtime::OperationExecutor<
        CommissioningMailbox,
        runtime::CommissioningCommand,
        runtime::CommissioningResult> commissioning_executor_{};
    SystemRuntime system_runtime_;
    Core1MeasurementRuntime core1_measurement_;
    LifecycleService lifecycle_;
    ConfigurationService configuration_;
    CommissioningService commissioning_;
    ProtocolV3::ApplicationDispatcher dispatcher_;
    Endpoint endpoint_;
    Core1Runtime core1_;
    ProtocolV3::NodeIdentity identity_{};
    ProtocolV3::NodeCapabilities capabilities_{};
};

}  // namespace node_v2
