#include "protocol_v3/dispatcher.h"

#include "protocol_v3/measurement_handler.h"
#include "protocol_v3/lifecycle.h"
#include "protocol_v3/lifecycle_handler.h"
#include "protocol_v3/time.h"

namespace ProtocolV3 {

DispatchResult ApplicationDispatcher::dispatch(
    const Frame& request, Frame& response) {
    return dispatch_at(request, response, 0);
}

DispatchResult ApplicationDispatcher::dispatch_at(
    const Frame& request, Frame& response, uint64_t node_receive_us) {
    if (request.role != Role::Request || request.source != kHostAddress ||
        request.destination != identity_.node_address) {
        return {};
    }

    initialize_response(request, response);
    if (request.service == kIdentityServiceId) {
        dispatch_identity(request, response);
        return {true, {}};
    }
    if (request.service == kMeasurementServiceId) {
        return dispatch_measurement(request, response);
    }
    if (request.service == kTimeServiceId) {
        dispatch_time(request, response, node_receive_us);
        return {true, {}};
    }
    if (request.service == kLifecycleServiceId) {
        return dispatch_lifecycle(request, response);
    }

    set_error(ResponseStatus::UnsupportedService, response);
    return {true, {}};
}

bool ApplicationDispatcher::prepare_transmission(
    Frame& response, uint64_t node_send_us) const {
    if (response.service != kTimeServiceId) return true;
    if (response.message != kObserveClockMessageId ||
        response.payload_size != kObserveClockResponseBytes ||
        response.payload[0] != static_cast<uint8_t>(ResponseStatus::Ok)) {
        return true;
    }
    return stamp_time_transmit(
               response.payload, response.payload_size, node_send_us) ==
           TimeCodecStatus::Ok;
}

TransmissionSettlementStatus ApplicationDispatcher::settle_transmission(
    const TransmissionCompletion& completion, bool transmitted) {
    if (!completion.required) return TransmissionSettlementStatus::NotRequired;
    if (completion.service != kLifecycleServiceId) {
        return TransmissionSettlementStatus::UnsupportedService;
    }
    if (lifecycle_handler_ == nullptr) {
        return TransmissionSettlementStatus::RejectedByHandler;
    }
    return lifecycle_handler_->settle_transmission(
               completion.token, transmitted)
        ? TransmissionSettlementStatus::Ok
        : TransmissionSettlementStatus::RejectedByHandler;
}

DispatchResult ApplicationDispatcher::dispatch_lifecycle(
    const Frame& request, Frame& response) {
    if (!capabilities_.supports_service(ServiceId::Lifecycle)) {
        set_error(ResponseStatus::UnsupportedService, response);
        return {true, {}, {}};
    }
    if (lifecycle_handler_ == nullptr) {
        set_error(ResponseStatus::InternalError, response);
        return {true, {}, {}};
    }
    const LifecycleHandlerResult handled = lifecycle_handler_->handle(
        request.message,
        request.payload,
        request.payload_size,
        response.payload,
        sizeof(response.payload));
    if (handled.payload_size == 0 ||
        handled.payload_size > sizeof(response.payload)) {
        set_error(ResponseStatus::InternalError, response);
        return {true, {}, {}};
    }
    response.payload_size = handled.payload_size;
    DispatchResult result{true, {}, {}};
    if (handled.post_transmit_required) {
        result.transmission = {
            true,
            kLifecycleServiceId,
            handled.post_transmit_token,
        };
    }
    return result;
}

CompletionSettlementStatus ApplicationDispatcher::settle_response(
    const ResponseCompletion& completion, bool accepted) {
    if (!completion.required) return CompletionSettlementStatus::NotRequired;
    if (completion.service != kMeasurementServiceId) {
        return CompletionSettlementStatus::UnsupportedService;
    }
    if (measurement_handler_ == nullptr) {
        return CompletionSettlementStatus::RejectedByHandler;
    }
    const measurement::CompletionStatus status = accepted
        ? measurement_handler_->complete(completion.token)
        : measurement_handler_->abort(completion.token);
    return status == measurement::CompletionStatus::Ok
        ? CompletionSettlementStatus::Ok
        : CompletionSettlementStatus::RejectedByHandler;
}

void ApplicationDispatcher::initialize_response(
    const Frame& request, Frame& response) const {
    response = {};
    response.role = Role::Response;
    response.destination = kHostAddress;
    response.source = identity_.node_address;
    response.service = request.service;
    response.message = request.message;
    response.transaction_id = request.transaction_id;
}

void ApplicationDispatcher::set_error(
    ResponseStatus status, Frame& response) const {
    response.payload[0] = static_cast<uint8_t>(status);
    response.payload_size = 1;
}

void ApplicationDispatcher::dispatch_identity(
    const Frame& request, Frame& response) const {
    if (request.payload_size != 0) {
        set_error(ResponseStatus::MalformedRequest, response);
        return;
    }

    IdentityCodecStatus status = IdentityCodecStatus::Ok;
    if (request.message == kGetIdentityMessageId) {
        status = encode_identity_success(
            identity_, response.payload, sizeof(response.payload));
        response.payload_size = kIdentityResponseBytes;
    } else if (request.message == kGetCapabilitiesMessageId) {
        status = encode_capabilities_success(
            capabilities_, response.payload, sizeof(response.payload));
        response.payload_size = kCapabilitiesResponseBytes;
    } else {
        set_error(ResponseStatus::UnsupportedMessage, response);
        return;
    }

    if (status != IdentityCodecStatus::Ok) {
        set_error(ResponseStatus::InternalError, response);
    }
}

void ApplicationDispatcher::dispatch_time(
    const Frame& request, Frame& response, uint64_t node_receive_us) const {
    if (!capabilities_.supports_service(ServiceId::Time)) {
        set_error(ResponseStatus::UnsupportedService, response);
        return;
    }
    if (request.message != kObserveClockMessageId) {
        set_error(ResponseStatus::UnsupportedMessage, response);
        return;
    }
    if (validate_time_request(request.payload_size) != TimeCodecStatus::Ok) {
        set_error(ResponseStatus::MalformedRequest, response);
        return;
    }
    const TimeExchangeResponse value{
        identity_.boot_epoch,
        node_receive_us,
        node_receive_us,
    };
    if (encode_time_success(
            value, response.payload, sizeof(response.payload)) !=
        TimeCodecStatus::Ok) {
        set_error(ResponseStatus::InternalError, response);
        return;
    }
    response.payload_size = kObserveClockResponseBytes;
}

DispatchResult ApplicationDispatcher::dispatch_measurement(
    const Frame& request, Frame& response) {
    if (!capabilities_.supports_service(ServiceId::Measurement)) {
        set_error(ResponseStatus::UnsupportedService, response);
        return {true, {}};
    }
    if (measurement_handler_ == nullptr) {
        set_error(ResponseStatus::InternalError, response);
        return {true, {}};
    }

    const MeasurementHandlerResult handled = measurement_handler_->handle(
        request.message, request.payload, request.payload_size,
        response.payload, sizeof(response.payload));
    if (handled.payload_size == 0 || handled.payload_size > sizeof(response.payload)) {
        if (handled.completion_required) {
            measurement_handler_->abort(handled.delivery_token);
        }
        set_error(ResponseStatus::InternalError, response);
        return {true, {}};
    }
    response.payload_size = handled.payload_size;

    DispatchResult result{true, {}};
    if (handled.completion_required) {
        result.completion = {
            true,
            kMeasurementServiceId,
            handled.delivery_token,
        };
    }
    return result;
}

}  // namespace ProtocolV3
