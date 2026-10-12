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

template <typename Acquisition, typename Port, typename Observer>
class RuntimeComposition {
public:
    using SensorRuntime = runtime::SensorRuntime<Acquisition, MeasurementSink>;
    using Core0Runtime = runtime::Core0SensorRuntime<
        SensorRuntime,
        OperationMailbox>;
    using SystemRuntime = runtime::SystemOperationRuntime<
        SensorRuntime,
        SystemOperationMailbox>;
    using Endpoint = TransportEndpoint<Port>;
    using Core1Runtime = runtime::Core1ProtocolRuntime<
        Core1MeasurementRuntime,
        LifecycleService,
        Endpoint,
        Observer>;

    RuntimeComposition(DataPlane& data_plane,
                       Acquisition& acquisition,
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
                       ProtocolV3::IDiagnosticsServiceHandler* diagnostics_handler = nullptr)
        : data_plane_(data_plane),
          sensor_runtime_(acquisition, data_plane.sink()),
          core0_(sensor_runtime_, mailbox_, fault_latch_),
          system_runtime_(sensor_runtime_, system_mailbox_,
                          firmware_update_control, trial_boot_control),
          core1_measurement_(data_plane.queue(), boot_epoch, fault_latch_),
          lifecycle_(mailbox_, system_mailbox_, system_action_),
          dispatcher_(identity, capabilities,
                      &core1_measurement_.handler(), &lifecycle_,
                      diagnostics_handler),
          endpoint_(port, dispatcher_, transport_policy),
          core1_(core1_measurement_, lifecycle_, endpoint_, observer,
                 core1_policy),
          identity_(identity),
          capabilities_(capabilities) {}

    RuntimeComposition(const RuntimeComposition&) = delete;
    RuntimeComposition& operator=(const RuntimeComposition&) = delete;

    bool configuration_valid() const {
        return ProtocolV3::validate(identity_) ==
                   ProtocolV3::IdentityCodecStatus::Ok &&
               ProtocolV3::validate(capabilities_) ==
                   ProtocolV3::IdentityCodecStatus::Ok &&
               endpoint_.valid_policy() && core1_.policy().valid() &&
               !data_plane_.sink().state().faulted;
    }

    Core0Runtime& core0() { return core0_; }
    void poll_core0() {
        core0_.poll();
        (void)system_runtime_.poll();
    }
    Core1Runtime& core1() { return core1_; }
    Core1MeasurementRuntime& measurement() { return core1_measurement_; }
    LifecycleService& lifecycle() { return lifecycle_; }
    ProtocolV3::ApplicationDispatcher& dispatcher() { return dispatcher_; }
    Endpoint& endpoint() { return endpoint_; }
    OperationMailbox& mailbox() { return mailbox_; }
    SystemOperationMailbox& system_mailbox() { return system_mailbox_; }
    runtime::DeferredSystemAction& system_action() { return system_action_; }
    runtime::RuntimeFaultLatch& fault_latch() { return fault_latch_; }

private:
    DataPlane& data_plane_;
    SensorRuntime sensor_runtime_;
    OperationMailbox mailbox_{};
    SystemOperationMailbox system_mailbox_{};
    runtime::DeferredSystemAction system_action_{};
    runtime::RuntimeFaultLatch fault_latch_{};
    Core0Runtime core0_;
    SystemRuntime system_runtime_;
    Core1MeasurementRuntime core1_measurement_;
    LifecycleService lifecycle_;
    ProtocolV3::ApplicationDispatcher dispatcher_;
    Endpoint endpoint_;
    Core1Runtime core1_;
    ProtocolV3::NodeIdentity identity_{};
    ProtocolV3::NodeCapabilities capabilities_{};
};

}  // namespace node_v2
