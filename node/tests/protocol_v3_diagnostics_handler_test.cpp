#include <cassert>
#include <cstddef>
#include <cstdint>

#include "protocol_v3/diagnostics.h"

namespace {

struct FakeDiagnostics {
    DiagnosticInfoSnapshot info_value{};
    DiagnosticFaultSnapshot fault_value{};
    PersistentDiagnosticRecord persistent_value{};
    DiagnosticEvent events[2]{};
    bool has_fault = false;
    bool has_persistent = false;

    DiagnosticInfoSnapshot info() const { return info_value; }
    bool has_fault_snapshot() const { return has_fault; }
    DiagnosticFaultSnapshot fault_snapshot() const { return fault_value; }
    bool persistent_record(PersistentDiagnosticRecord& output) const {
        if (!has_persistent) return false;
        output = persistent_value;
        return true;
    }
    size_t read_events(uint32_t start, DiagnosticEvent* output, size_t maximum,
                       uint32_t& next, uint32_t& first) const {
        first = 7;
        next = 9;
        size_t count = 0;
        for (const auto& event : events) {
            if (event.event_id >= start && count < maximum) {
                output[count++] = event;
            }
        }
        return count;
    }
};

uint16_t read_u16(const uint8_t* input) {
    return static_cast<uint16_t>(input[0]) |
        static_cast<uint16_t>(static_cast<uint16_t>(input[1]) << 8u);
}

uint32_t read_u32(const uint8_t* input) {
    uint32_t value = 0;
    for (size_t index = 0; index < 4; ++index) {
        value |= static_cast<uint32_t>(input[index]) << (8u * index);
    }
    return value;
}

uint64_t read_u64(const uint8_t* input) {
    uint64_t value = 0;
    for (size_t index = 0; index < 8; ++index) {
        value |= static_cast<uint64_t>(input[index]) << (8u * index);
    }
    return value;
}

void test_info_and_absent_snapshots() {
    FakeDiagnostics source{};
    source.info_value = {7, 9, 3, 8, 36, 512, 2, 1, 0};
    ProtocolV3::DiagnosticsServiceHandler<FakeDiagnostics> handler(source);
    uint8_t output[ProtocolV3::kMaxPayloadBytes]{};

    auto result = handler.handle(
        ProtocolV3::kGetDiagnosticInfoMessageId, nullptr, 0,
        output, sizeof(output));
    assert(result.payload_size == ProtocolV3::kDiagnosticInfoResponseBytes);
    assert(output[0] == static_cast<uint8_t>(ProtocolV3::ResponseStatus::Ok));
    assert(output[1] == 1);
    assert(read_u16(output + 3) == 2);
    assert(read_u16(output + 5) == 512);
    assert(read_u32(output + 7) == 3);
    assert(read_u32(output + 11) == 7);
    assert(read_u32(output + 15) == 9);
    assert(read_u32(output + 19) == 8);
    assert(read_u16(output + 23) == 36);

    result = handler.handle(
        ProtocolV3::kGetFaultSnapshotMessageId, nullptr, 0,
        output, sizeof(output));
    assert(result.payload_size == 2);
    assert(output[1] == 0);

    result = handler.handle(
        ProtocolV3::kGetPersistentDiagnosticRecordMessageId, nullptr, 0,
        output, sizeof(output));
    assert(result.payload_size == 2);
    assert(output[1] == 0);
}

void test_fault_and_persistent_snapshots() {
    FakeDiagnostics source{};
    source.has_fault = true;
    source.fault_value.event_id = 12;
    source.fault_value.time_ms = 34;
    source.fault_value.code = 36;
    source.fault_value.severity = 3;
    source.fault_value.reset_cause = 1;
    source.fault_value.sample_seq = 0x0102030405060708ULL;
    source.fault_value.sensor_errors = 5;
    source.fault_value.arg0 = -7;
    source.fault_value.arg1 = 9;
    source.has_persistent = true;
    source.persistent_value.generation = 4;
    source.persistent_value.boot_counter = 10;
    source.persistent_value.firmware_version = 0x000506;
    source.persistent_value.event_id = 12;
    source.persistent_value.event_code = 36;
    source.persistent_value.sample_seq = 99;
    source.persistent_value.arg0 = -2;
    source.persistent_value.arg1 = 3;
    ProtocolV3::DiagnosticsServiceHandler<FakeDiagnostics> handler(source);
    uint8_t output[ProtocolV3::kMaxPayloadBytes]{};

    auto result = handler.handle(
        ProtocolV3::kGetFaultSnapshotMessageId, nullptr, 0,
        output, sizeof(output));
    assert(result.payload_size == ProtocolV3::kFaultSnapshotResponseBytes);
    assert(output[1] == 1);
    assert(read_u32(output + 2) == 12);
    assert(read_u16(output + 10) == 36);
    assert(read_u64(output + 14) == 0x0102030405060708ULL);
    assert(static_cast<int32_t>(read_u32(output + 62)) == -7);
    assert(static_cast<int32_t>(read_u32(output + 66)) == 9);

    result = handler.handle(
        ProtocolV3::kGetPersistentDiagnosticRecordMessageId, nullptr, 0,
        output, sizeof(output));
    assert(result.payload_size ==
           ProtocolV3::kPersistentDiagnosticRecordResponseBytes);
    assert(output[1] == 1);
    assert(read_u32(output + 2) == 4);
    assert(read_u32(output + 10) == 0x000506);
    assert(read_u64(output + 28) == 99);
    assert(static_cast<int32_t>(read_u32(output + 76)) == -2);
}

void test_bounded_event_page_and_errors() {
    FakeDiagnostics source{};
    source.events[0] = {7, 100, 32, 2, 0, 50, -1, 2};
    source.events[1] = {8, 110, 36, 3, 1, 60, 3, -4};
    ProtocolV3::DiagnosticsServiceHandler<FakeDiagnostics> handler(source);
    uint8_t output[ProtocolV3::kMaxPayloadBytes]{};
    uint8_t request[ProtocolV3::kReadDiagnosticEventsRequestBytes]{};
    ProtocolV3::diagnostics_wire::write_u32(8, request);
    request[4] = 2;

    auto result = handler.handle(
        ProtocolV3::kReadDiagnosticEventsMessageId,
        request, sizeof(request), output, sizeof(output));
    assert(result.payload_size ==
           ProtocolV3::kDiagnosticEventsResponseHeaderBytes +
               ProtocolV3::kDiagnosticEventWireBytes);
    assert(output[1] == 1);
    assert(read_u32(output + 4) == 7);
    assert(read_u32(output + 8) == 9);
    const uint8_t* event = output + ProtocolV3::kDiagnosticEventsResponseHeaderBytes;
    assert(read_u32(event) == 8);
    assert(read_u16(event + 8) == 36);
    assert(static_cast<int32_t>(read_u32(event + 24)) == -4);

    result = handler.handle(
        ProtocolV3::kReadDiagnosticEventsMessageId,
        request, sizeof(request) - 1, output, sizeof(output));
    assert(result.payload_size == 1);
    assert(output[0] == static_cast<uint8_t>(
        ProtocolV3::ResponseStatus::MalformedRequest));
    result = handler.handle(99, nullptr, 0, output, sizeof(output));
    assert(result.payload_size == 1);
    assert(output[0] == static_cast<uint8_t>(
        ProtocolV3::ResponseStatus::UnsupportedMessage));
}

}  // namespace

int main() {
    test_info_and_absent_snapshots();
    test_fault_and_persistent_snapshots();
    test_bounded_event_page_and_errors();
    return 0;
}
