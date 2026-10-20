#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstring>

#include "measurement/delivery_session.h"
#include "protocol_v3/dispatcher.h"
#include "protocol_v3/diagnostics.h"
#include "protocol_v3/lifecycle.h"
#include "protocol_v3/lifecycle_handler.h"
#include "protocol_v3/measurement.h"
#include "protocol_v3/measurement_handler.h"
#include "protocol_v3/telemetry.h"
#include "runtime/operation_mailbox.h"

namespace {
using namespace ProtocolV3;

NodeIdentity identity() {
    NodeIdentity value{};
    const uint8_t hardware_id[kHardwareIdBytes] = {1, 2, 3, 4, 5, 6, 7, 8};
    std::memcpy(value.hardware_id, hardware_id, sizeof(hardware_id));
    value.boot_epoch = 17;
    value.node_address = 7;
    value.board_revision = 2;
    value.firmware = {0, 5, 0};
    return value;
}

NodeCapabilities capabilities() {
    NodeCapabilities value{};
    value.service_flags = service_capability_bit(ServiceId::Identity);
    value.max_payload_bytes = kMaxPayloadBytes;
    value.output_rate_mask = kImplementedOutputRateMask;
    return value;
}

Frame request(uint8_t service, uint8_t message) {
    Frame value{};
    value.role = Role::Request;
    value.destination = 7;
    value.source = kHostAddress;
    value.service = service;
    value.message = message;
    value.transaction_id = 0x12345678u;
    return value;
}

Frame wire_round_trip(const Frame& input) {
    uint8_t wire[kMaxWireFrameBytes]{};
    size_t wire_size = 0;
    assert(encode(input, wire, sizeof(wire), wire_size) == Status::Ok);
    StreamDecoder decoder{};
    Frame decoded{};
    Status status = Status::NoFrame;
    for (size_t i = 0; i < wire_size; ++i) {
        status = decoder.push(wire[i], decoded);
    }
    assert(status == Status::Ok);
    return decoded;
}

void test_full_identity_transaction() {
    ApplicationDispatcher dispatcher(identity(), capabilities());
    const Frame decoded_request = wire_round_trip(
        request(kIdentityServiceId, kGetIdentityMessageId));

    Frame response{};
    assert(dispatcher.dispatch(decoded_request, response));
    const Frame decoded_response = wire_round_trip(response);
    assert(decoded_response.role == Role::Response);
    assert(decoded_response.destination == kHostAddress);
    assert(decoded_response.source == 7);
    assert(decoded_response.transaction_id == 0x12345678u);

    NodeIdentity decoded_identity{};
    ResponseStatus remote_status = ResponseStatus::InternalError;
    assert(decode_identity(decoded_response.payload, decoded_response.payload_size,
                           decoded_identity, remote_status) == IdentityCodecStatus::Ok);
    assert(decoded_identity.boot_epoch == 17);
    assert(remote_status == ResponseStatus::Ok);
}

void test_capabilities_and_errors() {
    ApplicationDispatcher dispatcher(identity(), capabilities());
    Frame response{};
    assert(dispatcher.dispatch(
        request(kIdentityServiceId, kGetCapabilitiesMessageId), response));
    assert(response.payload_size == kCapabilitiesResponseBytes);

    Frame malformed = request(kIdentityServiceId, kGetIdentityMessageId);
    malformed.payload[0] = 1;
    malformed.payload_size = 1;
    assert(dispatcher.dispatch(malformed, response));
    assert(response.payload_size == 1);
    assert(response.payload[0] == static_cast<uint8_t>(ResponseStatus::MalformedRequest));

    assert(dispatcher.dispatch(request(99, 1), response));
    assert(response.payload[0] == static_cast<uint8_t>(ResponseStatus::UnsupportedService));
    assert(dispatcher.dispatch(request(kIdentityServiceId, 99), response));
    assert(response.payload[0] == static_cast<uint8_t>(ResponseStatus::UnsupportedMessage));
}

void test_another_node_never_responds() {
    ApplicationDispatcher dispatcher(identity(), capabilities());
    Frame foreign = request(kIdentityServiceId, kGetIdentityMessageId);
    foreign.destination = 8;
    Frame response{};
    assert(!dispatcher.dispatch(foreign, response));
}

measurement::SampleRecord sample(int32_t value) {
    return {
        1000u + static_cast<uint64_t>(value) * 4000u,
        value,
        -value,
        value + 1,
        1,
        1,
        measurement::kTimingValidFlag,
        0,
    };
}

NodeCapabilities measurement_capabilities() {
    NodeCapabilities value = capabilities();
    value.service_flags |= service_capability_bit(ServiceId::Measurement);
    value.feature_flags |= kFeatureSampleJournal;
    value.journal_capacity_samples = 8;
    value.max_sample_chunk_samples = 4;
    return value;
}

void test_measurement_response_is_settled_by_queue_owner() {
    measurement::MeasurementJournal<8, 8> journal;
    journal.append_sample(0, sample(0));
    journal.append_sample(1, sample(1));
    measurement::DeliverySession<8, 4> session(identity().boot_epoch, journal);
    MeasurementServiceHandler<8, 4> handler(session);
    ApplicationDispatcher dispatcher(
        identity(), measurement_capabilities(), &handler);

    Frame pull = request(kMeasurementServiceId, kPullSamplesMessageId);
    const PullSamplesRequest pull_request{identity().boot_epoch, 0, 0, 2};
    assert(encode_pull_request(
               pull_request, pull.payload, sizeof(pull.payload)) ==
           MeasurementCodecStatus::Ok);
    pull.payload_size = kPullRequestBytes;

    Frame response{};
    DispatchResult dispatched = dispatcher.dispatch(pull, response);
    assert(dispatched.response_produced);
    assert(dispatched.completion.required);
    assert(journal.state().delivered == 0);
    assert(dispatcher.settle_response(dispatched.completion, true) ==
           CompletionSettlementStatus::Ok);
    assert(journal.state().delivered == 2);
    assert(dispatcher.settle_response(dispatched.completion, true) ==
           CompletionSettlementStatus::RejectedByHandler);

    dispatched = dispatcher.dispatch(pull, response);
    assert(dispatched.completion.required);
    assert(dispatcher.settle_response(dispatched.completion, false) ==
           CompletionSettlementStatus::Ok);
    assert(journal.state().delivered == 2);
}

void test_measurement_capability_and_handler_must_agree() {
    Frame response{};
    Frame state_request = request(kMeasurementServiceId, kGetStreamStateMessageId);

    ApplicationDispatcher unsupported(identity(), capabilities());
    assert(unsupported.dispatch(state_request, response));
    assert(response.payload[0] ==
           static_cast<uint8_t>(ResponseStatus::UnsupportedService));

    ApplicationDispatcher missing_handler(identity(), measurement_capabilities());
    assert(missing_handler.dispatch(state_request, response));
    assert(response.payload[0] == static_cast<uint8_t>(ResponseStatus::InternalError));
}

void test_lifecycle_request_is_routed_to_mailbox_and_reconciled() {
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
    Mailbox mailbox;
    SystemMailbox system_mailbox;
    runtime::DeferredSystemAction system_action;
    LifecycleServiceHandler<Mailbox, SystemMailbox, 2> lifecycle(
        mailbox, system_mailbox, system_action);
    NodeCapabilities caps = capabilities();
    caps.service_flags |= service_capability_bit(ServiceId::Lifecycle);
    ApplicationDispatcher dispatcher(identity(), caps, nullptr, &lifecycle);

    Frame start = request(kLifecycleServiceId, kStartRecordingMessageId);
    const runtime::SensorRuntimeCommand command{
        77, runtime::SensorRuntimeOperation::Start};
    size_t encoded_size = 0;
    uint8_t encoded_message = 0;
    assert(encode_lifecycle_request(
               command,
               start.payload,
               sizeof(start.payload),
               encoded_size,
               encoded_message) == LifecycleCodecStatus::Ok);
    assert(encoded_message == start.message);
    start.payload_size = encoded_size;

    Frame response{};
    assert(dispatcher.dispatch(start, response));
    runtime::SensorRuntimeResult result{};
    ResponseStatus remote = ResponseStatus::InternalError;
    assert(decode_lifecycle_response(
               response.payload,
               response.payload_size,
               result,
               remote) == LifecycleCodecStatus::Ok);
    assert(result.operation_id == 77);
    assert(result.code == runtime::SensorRuntimeResultCode::InProgress);

    runtime::SensorRuntimeCommand received{};
    assert(mailbox.try_receive_command(received));
    assert(received.operation_id == 77);
    assert(mailbox.try_send_result({
        77,
        runtime::SensorRuntimeResultCode::Ok,
        runtime::SensorRuntimeState::Recording,
        runtime::ObservabilityProfile::Production,
        SensorStatus::Ok,
    }));

    assert(dispatcher.dispatch(start, response));
    assert(decode_lifecycle_response(
               response.payload,
               response.payload_size,
               result,
               remote) == LifecycleCodecStatus::Ok);
    assert(result.code == runtime::SensorRuntimeResultCode::Ok);
    assert(result.state == runtime::SensorRuntimeState::Recording);
}

class FakeDiagnosticsHandler final : public IDiagnosticsServiceHandler {
public:
    DiagnosticsHandlerResult handle(
        uint8_t message, const uint8_t*, size_t request_size,
        uint8_t* output, size_t capacity) override {
        assert(message == kGetDiagnosticInfoMessageId);
        assert(request_size == 0);
        assert(capacity >= 2);
        output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
        output[1] = 0x5a;
        return {2};
    }
};

void test_diagnostics_requires_capability_and_routes_read_only_handler() {
    FakeDiagnosticsHandler diagnostics;
    Frame response{};
    ApplicationDispatcher unavailable(
        identity(), capabilities(), nullptr, nullptr, &diagnostics);
    assert(unavailable.dispatch(
        request(kDiagnosticsServiceId, kGetDiagnosticInfoMessageId), response));
    assert(response.payload_size == 1);
    assert(response.payload[0] ==
           static_cast<uint8_t>(ResponseStatus::UnsupportedService));

    NodeCapabilities caps = capabilities();
    caps.service_flags |= service_capability_bit(ServiceId::Diagnostics);
    ApplicationDispatcher dispatcher(
        identity(), caps, nullptr, nullptr, &diagnostics);
    assert(dispatcher.dispatch(
        request(kDiagnosticsServiceId, kGetDiagnosticInfoMessageId), response));
    assert(response.payload_size == 2);
    assert(response.payload[0] == static_cast<uint8_t>(ResponseStatus::Ok));
    assert(response.payload[1] == 0x5a);
}

void test_telemetry_requires_capability_and_routes_cached_snapshot() {
    telemetry::SnapshotCache cache;
    assert(cache.publish({123456, telemetry::kAdxl355TemperatureField, 1885}));
    TelemetryServiceHandler telemetry_handler(cache);
    Frame response{};
    const Frame snapshot_request =
        request(kTelemetryServiceId, kGetTelemetrySnapshotMessageId);

    ApplicationDispatcher unavailable(
        identity(), capabilities(), nullptr, nullptr, nullptr, nullptr, nullptr,
        &telemetry_handler);
    assert(unavailable.dispatch(snapshot_request, response));
    assert(response.payload_size == 1);
    assert(response.payload[0] ==
           static_cast<uint8_t>(ResponseStatus::UnsupportedService));

    NodeCapabilities caps = capabilities();
    caps.service_flags |= service_capability_bit(ServiceId::Telemetry);
    ApplicationDispatcher dispatcher(
        identity(), caps, nullptr, nullptr, nullptr, nullptr, nullptr,
        &telemetry_handler);
    assert(dispatcher.dispatch(snapshot_request, response));
    assert(response.payload_size == kTelemetrySnapshotResponseBytes);
    assert(response.payload[0] == static_cast<uint8_t>(ResponseStatus::Ok));
    assert(response.payload[1] == kTelemetrySchemaVersion);
}
}  // namespace

int main() {
    test_full_identity_transaction();
    test_capabilities_and_errors();
    test_another_node_never_responds();
    test_measurement_response_is_settled_by_queue_owner();
    test_measurement_capability_and_handler_must_agree();
    test_lifecycle_request_is_routed_to_mailbox_and_reconciled();
    test_diagnostics_requires_capability_and_routes_read_only_handler();
    test_telemetry_requires_capability_and_routes_cached_snapshot();
    return 0;
}
