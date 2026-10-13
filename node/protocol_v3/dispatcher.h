#pragma once

#include "protocol_v3/framing.h"
#include "protocol_v3/identity.h"

namespace ProtocolV3 {

class IMeasurementServiceHandler;
class ILifecycleServiceHandler;
class IDiagnosticsServiceHandler;
class IConfigurationServiceHandler;
class ICommissioningServiceHandler;

struct ResponseCompletion {
    bool required = false;
    uint8_t service = 0;
    uint64_t token = 0;
};

struct TransmissionCompletion {
    bool required = false;
    uint8_t service = 0;
    uint64_t token = 0;
};

struct DispatchResult {
    bool response_produced = false;
    ResponseCompletion completion{};
    TransmissionCompletion transmission{};

    explicit operator bool() const { return response_produced; }
};

enum class CompletionSettlementStatus : uint8_t {
    Ok = 0,
    NotRequired,
    UnsupportedService,
    RejectedByHandler,
};

enum class TransmissionSettlementStatus : uint8_t {
    Ok = 0,
    NotRequired,
    UnsupportedService,
    RejectedByHandler,
};

// Routes already decoded application frames. Transport timing, retries and
// UART ownership remain outside this class.
class ApplicationDispatcher {
public:
    ApplicationDispatcher(
        NodeIdentity identity, NodeCapabilities capabilities,
        IMeasurementServiceHandler* measurement_handler = nullptr,
        ILifecycleServiceHandler* lifecycle_handler = nullptr,
        IDiagnosticsServiceHandler* diagnostics_handler = nullptr,
        IConfigurationServiceHandler* configuration_handler = nullptr,
        ICommissioningServiceHandler* commissioning_handler = nullptr)
        : identity_(identity), capabilities_(capabilities),
          measurement_handler_(measurement_handler),
          lifecycle_handler_(lifecycle_handler),
          diagnostics_handler_(diagnostics_handler),
          configuration_handler_(configuration_handler),
          commissioning_handler_(commissioning_handler) {}

    ApplicationDispatcher(const ApplicationDispatcher&) = delete;
    ApplicationDispatcher& operator=(const ApplicationDispatcher&) = delete;

    // A request for another node produces no response, because only the
    // addressed node may drive a shared RS485 bus.
    DispatchResult dispatch(const Frame& request, Frame& response);
    DispatchResult dispatch_at(
        const Frame& request, Frame& response, uint64_t node_receive_us);

    // Finalizes timestamps whose meaning is tied to actual transmitter start.
    // Non-Time responses are left unchanged.
    bool prepare_transmission(Frame& response, uint64_t node_send_us) const;

    // Called by the owner of the bounded response queue after try_enqueue.
    CompletionSettlementStatus settle_response(
        const ResponseCompletion& completion, bool accepted);
    TransmissionSettlementStatus settle_transmission(
        const TransmissionCompletion& completion, bool transmitted);

private:
    void initialize_response(const Frame& request, Frame& response) const;
    void set_error(ResponseStatus status, Frame& response) const;
    void dispatch_identity(const Frame& request, Frame& response) const;
    void dispatch_time(
        const Frame& request, Frame& response, uint64_t node_receive_us) const;
    DispatchResult dispatch_measurement(const Frame& request, Frame& response);
    DispatchResult dispatch_lifecycle(const Frame& request, Frame& response);
    DispatchResult dispatch_diagnostics(const Frame& request, Frame& response);
    DispatchResult dispatch_configuration(const Frame& request, Frame& response);
    DispatchResult dispatch_commissioning(const Frame& request, Frame& response);

    NodeIdentity identity_{};
    NodeCapabilities capabilities_{};
    IMeasurementServiceHandler* measurement_handler_ = nullptr;
    ILifecycleServiceHandler* lifecycle_handler_ = nullptr;
    IDiagnosticsServiceHandler* diagnostics_handler_ = nullptr;
    IConfigurationServiceHandler* configuration_handler_ = nullptr;
    ICommissioningServiceHandler* commissioning_handler_ = nullptr;
};

}  // namespace ProtocolV3
