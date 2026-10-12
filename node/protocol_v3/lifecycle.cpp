#include "protocol_v3/lifecycle.h"

namespace ProtocolV3 {
namespace {

void write_u32(uint32_t value, uint8_t* output) {
    for (size_t index = 0; index < 4; ++index) {
        output[index] = static_cast<uint8_t>(value >> (8u * index));
    }
}

uint32_t read_u32(const uint8_t* input) {
    uint32_t value = 0;
    for (size_t index = 0; index < 4; ++index) {
        value |= static_cast<uint32_t>(input[index]) << (8u * index);
    }
    return value;
}

bool operation_for_message(
    uint8_t message, runtime::SensorRuntimeOperation& operation) {
    switch (message) {
        case kGetRuntimeStateMessageId:
            operation = runtime::SensorRuntimeOperation::GetState;
            return true;
        case kStartRecordingMessageId:
            operation = runtime::SensorRuntimeOperation::Start;
            return true;
        case kStopRecordingMessageId:
            operation = runtime::SensorRuntimeOperation::Stop;
            return true;
        case kEnterMaintenanceMessageId:
            operation = runtime::SensorRuntimeOperation::EnterMaintenance;
            return true;
        case kExitMaintenanceMessageId:
            operation = runtime::SensorRuntimeOperation::ExitMaintenance;
            return true;
        case kSetObservabilityMessageId:
            operation = runtime::SensorRuntimeOperation::SetObservabilityProfile;
            return true;
        default:
            return false;
    }
}

bool message_for_operation(
    runtime::SensorRuntimeOperation operation, uint8_t& message) {
    switch (operation) {
        case runtime::SensorRuntimeOperation::GetState:
            message = kGetRuntimeStateMessageId;
            return true;
        case runtime::SensorRuntimeOperation::Start:
            message = kStartRecordingMessageId;
            return true;
        case runtime::SensorRuntimeOperation::Stop:
            message = kStopRecordingMessageId;
            return true;
        case runtime::SensorRuntimeOperation::EnterMaintenance:
            message = kEnterMaintenanceMessageId;
            return true;
        case runtime::SensorRuntimeOperation::ExitMaintenance:
            message = kExitMaintenanceMessageId;
            return true;
        case runtime::SensorRuntimeOperation::SetObservabilityProfile:
            message = kSetObservabilityMessageId;
            return true;
        default:
            return false;
    }
}

bool known_result(uint8_t value) {
    return value <= static_cast<uint8_t>(runtime::SensorRuntimeResultCode::Faulted);
}

bool known_state(uint8_t value) {
    return value <= static_cast<uint8_t>(runtime::SensorRuntimeState::Faulted);
}

bool known_profile(uint8_t value) {
    return value <= static_cast<uint8_t>(runtime::ObservabilityProfile::Diagnostic);
}

bool known_sensor_status(uint8_t value) {
    return value <= static_cast<uint8_t>(SensorStatus::InternalError);
}

}  // namespace

LifecycleCodecStatus encode_lifecycle_request(
    const runtime::SensorRuntimeCommand& command,
    uint8_t* output,
    size_t output_capacity,
    size_t& output_size,
    uint8_t& message) {
    output_size = 0;
    if (command.operation_id == 0) {
        return LifecycleCodecStatus::ZeroOperationId;
    }
    if (!message_for_operation(command.operation, message)) {
        return LifecycleCodecStatus::UnknownMessage;
    }
    const bool has_profile = command.operation ==
        runtime::SensorRuntimeOperation::SetObservabilityProfile;
    const size_t required = has_profile
        ? kLifecycleProfileRequestBytes
        : kLifecycleOperationRequestBytes;
    if (output == nullptr || output_capacity < required) {
        return LifecycleCodecStatus::OutputTooSmall;
    }
    if (has_profile &&
        command.requested_profile != runtime::ObservabilityProfile::Production &&
        command.requested_profile != runtime::ObservabilityProfile::Diagnostic) {
        return LifecycleCodecStatus::UnknownObservabilityProfile;
    }
    write_u32(command.operation_id, output);
    if (has_profile) {
        output[4] = static_cast<uint8_t>(command.requested_profile);
    }
    output_size = required;
    return LifecycleCodecStatus::Ok;
}

LifecycleCodecStatus decode_lifecycle_request(
    uint8_t message,
    const uint8_t* payload,
    size_t payload_size,
    runtime::SensorRuntimeCommand& output) {
    runtime::SensorRuntimeOperation operation{};
    if (!operation_for_message(message, operation)) {
        return LifecycleCodecStatus::UnknownMessage;
    }
    const bool has_profile = operation ==
        runtime::SensorRuntimeOperation::SetObservabilityProfile;
    const size_t required = has_profile
        ? kLifecycleProfileRequestBytes
        : kLifecycleOperationRequestBytes;
    if (payload == nullptr || payload_size != required) {
        return LifecycleCodecStatus::InvalidPayloadLength;
    }
    const uint32_t operation_id = read_u32(payload);
    if (operation_id == 0) return LifecycleCodecStatus::ZeroOperationId;

    runtime::ObservabilityProfile profile =
        runtime::ObservabilityProfile::Production;
    if (has_profile) {
        if (!known_profile(payload[4])) {
            return LifecycleCodecStatus::UnknownObservabilityProfile;
        }
        profile = static_cast<runtime::ObservabilityProfile>(payload[4]);
    }
    output = {operation_id, operation, profile};
    return LifecycleCodecStatus::Ok;
}

LifecycleCodecStatus encode_lifecycle_response(
    const runtime::SensorRuntimeResult& result,
    uint8_t* output,
    size_t output_capacity) {
    if (result.operation_id == 0) return LifecycleCodecStatus::ZeroOperationId;
    if (!known_result(static_cast<uint8_t>(result.code))) {
        return LifecycleCodecStatus::UnknownResultCode;
    }
    if (!known_state(static_cast<uint8_t>(result.state))) {
        return LifecycleCodecStatus::UnknownRuntimeState;
    }
    if (!known_profile(static_cast<uint8_t>(result.observability))) {
        return LifecycleCodecStatus::UnknownObservabilityProfile;
    }
    if (!known_sensor_status(static_cast<uint8_t>(result.sensor_status))) {
        return LifecycleCodecStatus::UnknownSensorStatus;
    }
    if (output == nullptr || output_capacity < kLifecycleResponseBytes) {
        return LifecycleCodecStatus::OutputTooSmall;
    }
    output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
    write_u32(result.operation_id, output + 1);
    output[5] = static_cast<uint8_t>(result.code);
    output[6] = static_cast<uint8_t>(result.state);
    output[7] = static_cast<uint8_t>(result.observability);
    output[8] = static_cast<uint8_t>(result.sensor_status);
    return LifecycleCodecStatus::Ok;
}

LifecycleCodecStatus encode_prepare_firmware_update_request(
    uint32_t operation_id,
    uint8_t* output,
    size_t output_capacity) {
    if (operation_id == 0) return LifecycleCodecStatus::ZeroOperationId;
    if (output == nullptr ||
        output_capacity < kLifecycleOperationRequestBytes) {
        return LifecycleCodecStatus::OutputTooSmall;
    }
    write_u32(operation_id, output);
    return LifecycleCodecStatus::Ok;
}

LifecycleCodecStatus decode_prepare_firmware_update_request(
    const uint8_t* payload,
    size_t payload_size,
    uint32_t& operation_id) {
    if (payload == nullptr ||
        payload_size != kLifecycleOperationRequestBytes) {
        return LifecycleCodecStatus::InvalidPayloadLength;
    }
    operation_id = read_u32(payload);
    return operation_id == 0
        ? LifecycleCodecStatus::ZeroOperationId
        : LifecycleCodecStatus::Ok;
}

LifecycleCodecStatus encode_confirm_trial_boot_request(
    uint32_t operation_id,
    uint8_t* output,
    size_t output_capacity) {
    return encode_prepare_firmware_update_request(
        operation_id, output, output_capacity);
}

LifecycleCodecStatus decode_confirm_trial_boot_request(
    const uint8_t* payload,
    size_t payload_size,
    uint32_t& operation_id) {
    return decode_prepare_firmware_update_request(
        payload, payload_size, operation_id);
}

LifecycleCodecStatus encode_restart_node_request(
    uint32_t operation_id,
    uint8_t* output,
    size_t output_capacity) {
    return encode_prepare_firmware_update_request(
        operation_id, output, output_capacity);
}

LifecycleCodecStatus decode_restart_node_request(
    const uint8_t* payload,
    size_t payload_size,
    uint32_t& operation_id) {
    return decode_prepare_firmware_update_request(
        payload, payload_size, operation_id);
}

LifecycleCodecStatus decode_lifecycle_response(
    const uint8_t* payload,
    size_t payload_size,
    runtime::SensorRuntimeResult& output,
    ResponseStatus& remote_status) {
    if (payload == nullptr || payload_size == 0) {
        return LifecycleCodecStatus::InvalidPayloadLength;
    }
    if (payload[0] > static_cast<uint8_t>(ResponseStatus::InternalError)) {
        return LifecycleCodecStatus::UnknownResponseStatus;
    }
    remote_status = static_cast<ResponseStatus>(payload[0]);
    if (remote_status != ResponseStatus::Ok) {
        return LifecycleCodecStatus::RemoteError;
    }
    if (payload_size != kLifecycleResponseBytes) {
        return LifecycleCodecStatus::InvalidPayloadLength;
    }
    if (!known_result(payload[5])) return LifecycleCodecStatus::UnknownResultCode;
    if (!known_state(payload[6])) return LifecycleCodecStatus::UnknownRuntimeState;
    if (!known_profile(payload[7])) {
        return LifecycleCodecStatus::UnknownObservabilityProfile;
    }
    if (!known_sensor_status(payload[8])) {
        return LifecycleCodecStatus::UnknownSensorStatus;
    }
    const uint32_t operation_id = read_u32(payload + 1);
    if (operation_id == 0) return LifecycleCodecStatus::ZeroOperationId;
    output = {
        operation_id,
        static_cast<runtime::SensorRuntimeResultCode>(payload[5]),
        static_cast<runtime::SensorRuntimeState>(payload[6]),
        static_cast<runtime::ObservabilityProfile>(payload[7]),
        static_cast<SensorStatus>(payload[8]),
    };
    return LifecycleCodecStatus::Ok;
}

}  // namespace ProtocolV3
