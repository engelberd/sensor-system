#pragma once

#include <cstddef>
#include <cstdint>

#include "protocol_v3/application.h"
#include "protocol_v3/framing.h"
#include "runtime/sensor_runtime.h"

namespace ProtocolV3 {

constexpr uint8_t kLifecycleServiceId =
    static_cast<uint8_t>(ServiceId::Lifecycle);
constexpr uint8_t kGetRuntimeStateMessageId = 1;
constexpr uint8_t kStartRecordingMessageId = 2;
constexpr uint8_t kStopRecordingMessageId = 3;
constexpr uint8_t kEnterMaintenanceMessageId = 4;
constexpr uint8_t kExitMaintenanceMessageId = 5;
constexpr uint8_t kSetObservabilityMessageId = 6;
constexpr uint8_t kPrepareFirmwareUpdateMessageId = 7;
constexpr uint8_t kConfirmTrialBootMessageId = 8;
constexpr uint8_t kRestartNodeMessageId = 9;

constexpr size_t kLifecycleOperationRequestBytes = 4;
constexpr size_t kLifecycleProfileRequestBytes = 5;
constexpr size_t kLifecycleResponseBytes = 9;

enum class LifecycleCodecStatus : uint8_t {
    Ok = 0,
    OutputTooSmall,
    InvalidPayloadLength,
    UnknownMessage,
    ZeroOperationId,
    UnknownResponseStatus,
    RemoteError,
    UnknownResultCode,
    UnknownRuntimeState,
    UnknownObservabilityProfile,
    UnknownSensorStatus,
};

LifecycleCodecStatus encode_lifecycle_request(
    const runtime::SensorRuntimeCommand& command,
    uint8_t* output,
    size_t output_capacity,
    size_t& output_size,
    uint8_t& message);

LifecycleCodecStatus decode_lifecycle_request(
    uint8_t message,
    const uint8_t* payload,
    size_t payload_size,
    runtime::SensorRuntimeCommand& output);

LifecycleCodecStatus encode_prepare_firmware_update_request(
    uint32_t operation_id,
    uint8_t* output,
    size_t output_capacity);

LifecycleCodecStatus decode_prepare_firmware_update_request(
    const uint8_t* payload,
    size_t payload_size,
    uint32_t& operation_id);

LifecycleCodecStatus encode_confirm_trial_boot_request(
    uint32_t operation_id,
    uint8_t* output,
    size_t output_capacity);

LifecycleCodecStatus decode_confirm_trial_boot_request(
    const uint8_t* payload,
    size_t payload_size,
    uint32_t& operation_id);

LifecycleCodecStatus encode_restart_node_request(
    uint32_t operation_id,
    uint8_t* output,
    size_t output_capacity);

LifecycleCodecStatus decode_restart_node_request(
    const uint8_t* payload,
    size_t payload_size,
    uint32_t& operation_id);

LifecycleCodecStatus encode_lifecycle_response(
    const runtime::SensorRuntimeResult& result,
    uint8_t* output,
    size_t output_capacity);

LifecycleCodecStatus decode_lifecycle_response(
    const uint8_t* payload,
    size_t payload_size,
    runtime::SensorRuntimeResult& output,
    ResponseStatus& remote_status);

}  // namespace ProtocolV3
