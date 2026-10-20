#include <cassert>
#include <cstdint>

#include "protocol_v3/telemetry.h"

namespace {

uint16_t read_u16(const uint8_t* input) {
    return static_cast<uint16_t>(input[0]) |
        static_cast<uint16_t>(static_cast<uint16_t>(input[1]) << 8u);
}

uint64_t read_u64(const uint8_t* input) {
    uint64_t value = 0;
    for (size_t index = 0; index < 8; ++index) {
        value |= static_cast<uint64_t>(input[index]) << (8u * index);
    }
    return value;
}

void test_empty_cache_is_busy_and_snapshot_has_exact_layout() {
    telemetry::SnapshotCache cache;
    ProtocolV3::TelemetryServiceHandler handler(cache);
    uint8_t output[ProtocolV3::kTelemetrySnapshotResponseBytes]{};

    auto result = handler.handle(
        ProtocolV3::kGetTelemetrySnapshotMessageId,
        nullptr, 0, output, sizeof(output));
    assert(result.payload_size == 1);
    assert(output[0] == static_cast<uint8_t>(ProtocolV3::ResponseStatus::Busy));

    assert(cache.publish({123456, telemetry::kAdxl355TemperatureField, 1885}));
    result = handler.handle(
        ProtocolV3::kGetTelemetrySnapshotMessageId,
        nullptr, 0, output, sizeof(output));
    assert(result.payload_size == ProtocolV3::kTelemetrySnapshotResponseBytes);
    assert(output[0] == static_cast<uint8_t>(ProtocolV3::ResponseStatus::Ok));
    assert(output[1] == ProtocolV3::kTelemetrySchemaVersion);
    assert(read_u64(output + 2) == 123456);
    assert(read_u16(output + 10) == telemetry::kAdxl355TemperatureField);
    assert(read_u16(output + 12) == 1885);
}

void test_rejects_payload_unknown_message_and_small_output() {
    telemetry::SnapshotCache cache;
    ProtocolV3::TelemetryServiceHandler handler(cache);
    uint8_t output[ProtocolV3::kTelemetrySnapshotResponseBytes]{};
    uint8_t request = 1;

    auto result = handler.handle(
        ProtocolV3::kGetTelemetrySnapshotMessageId,
        &request, 1, output, sizeof(output));
    assert(result.payload_size == 1);
    assert(output[0] == static_cast<uint8_t>(
        ProtocolV3::ResponseStatus::MalformedRequest));
    result = handler.handle(99, nullptr, 0, output, sizeof(output));
    assert(output[0] == static_cast<uint8_t>(
        ProtocolV3::ResponseStatus::UnsupportedMessage));
    assert(cache.publish({1, telemetry::kAdxl355TemperatureField, 1885}));
    result = handler.handle(
        ProtocolV3::kGetTelemetrySnapshotMessageId,
        nullptr, 0, output, 1);
    assert(result.payload_size == 1);
    assert(output[0] == static_cast<uint8_t>(
        ProtocolV3::ResponseStatus::InternalError));
}

}  // namespace

int main() {
    test_empty_cache_is_busy_and_snapshot_has_exact_layout();
    test_rejects_payload_unknown_message_and_small_output();
    return 0;
}
