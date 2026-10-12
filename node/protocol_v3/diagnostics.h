#pragma once

#include <cstddef>
#include <cstdint>

#include "diagnostics/diagnostic_types.h"
#include "protocol_v3/application.h"
#include "protocol_v3/framing.h"

namespace ProtocolV3 {

constexpr uint8_t kDiagnosticsServiceId =
    static_cast<uint8_t>(ServiceId::Diagnostics);
constexpr uint8_t kGetDiagnosticInfoMessageId = 1;
constexpr uint8_t kGetFaultSnapshotMessageId = 2;
constexpr uint8_t kGetPersistentDiagnosticRecordMessageId = 3;
constexpr uint8_t kReadDiagnosticEventsMessageId = 4;

constexpr size_t kDiagnosticInfoResponseBytes = 25;
constexpr size_t kFaultSnapshotResponseBytes = 70;
constexpr size_t kPersistentDiagnosticRecordResponseBytes = 84;
constexpr size_t kReadDiagnosticEventsRequestBytes = 5;
constexpr size_t kDiagnosticEventsResponseHeaderBytes = 12;
constexpr size_t kDiagnosticEventWireBytes = 28;
constexpr uint8_t kMaxDiagnosticEventsPerResponse = 16;

struct DiagnosticsHandlerResult {
    size_t payload_size = 0;
};

class IDiagnosticsServiceHandler {
public:
    virtual ~IDiagnosticsServiceHandler() = default;
    virtual DiagnosticsHandlerResult handle(
        uint8_t message, const uint8_t* request_payload,
        size_t request_payload_size, uint8_t* response_payload,
        size_t response_capacity) = 0;
};

namespace diagnostics_wire {

inline void write_u16(uint16_t value, uint8_t* output) {
    output[0] = static_cast<uint8_t>(value);
    output[1] = static_cast<uint8_t>(value >> 8u);
}

inline void write_u32(uint32_t value, uint8_t* output) {
    for (size_t index = 0; index < 4; ++index) {
        output[index] = static_cast<uint8_t>(value >> (8u * index));
    }
}

inline void write_u64(uint64_t value, uint8_t* output) {
    for (size_t index = 0; index < 8; ++index) {
        output[index] = static_cast<uint8_t>(value >> (8u * index));
    }
}

inline uint32_t read_u32(const uint8_t* input) {
    uint32_t value = 0;
    for (size_t index = 0; index < 4; ++index) {
        value |= static_cast<uint32_t>(input[index]) << (8u * index);
    }
    return value;
}

inline void encode_fault_fields(
    const DiagnosticFaultSnapshot& snapshot, uint8_t* output) {
    write_u32(snapshot.event_id, output + 0);
    write_u32(snapshot.time_ms, output + 4);
    write_u16(snapshot.code, output + 8);
    output[10] = snapshot.severity;
    output[11] = snapshot.reset_cause;
    write_u64(snapshot.sample_seq, output + 12);
    write_u32(snapshot.last_progress_ms, output + 20);
    write_u32(snapshot.fifo_no_data, output + 24);
    write_u32(snapshot.sensor_errors, output + 28);
    write_u32(snapshot.dropped_samples, output + 32);
    write_u32(snapshot.rx_overflow_count, output + 36);
    write_u32(snapshot.packet_overwrite_count, output + 40);
    write_u32(snapshot.debug_gpio_int1_edges, output + 44);
    write_u32(snapshot.debug_gpio_drdy_edges, output + 48);
    write_u32(snapshot.debug_config_snapshot, output + 52);
    write_u32(snapshot.debug_irq_snapshot, output + 56);
    write_u32(static_cast<uint32_t>(snapshot.arg0), output + 60);
    write_u32(static_cast<uint32_t>(snapshot.arg1), output + 64);
}

}  // namespace diagnostics_wire

// Read-only adapter. It is deliberately independent of lifecycle state so a
// faulted node remains inspectable without Start, Stop, reset, or USB access.
template <typename DiagnosticsSource>
class DiagnosticsServiceHandler final : public IDiagnosticsServiceHandler {
public:
    explicit DiagnosticsServiceHandler(DiagnosticsSource& source)
        : source_(source) {}

    DiagnosticsHandlerResult handle(
        uint8_t message, const uint8_t* request_payload,
        size_t request_payload_size, uint8_t* response_payload,
        size_t response_capacity) override {
        if (message == kGetDiagnosticInfoMessageId) {
            return get_info(request_payload_size, response_payload,
                            response_capacity);
        }
        if (message == kGetFaultSnapshotMessageId) {
            return get_fault(request_payload_size, response_payload,
                             response_capacity);
        }
        if (message == kGetPersistentDiagnosticRecordMessageId) {
            return get_persistent(request_payload_size, response_payload,
                                  response_capacity);
        }
        if (message == kReadDiagnosticEventsMessageId) {
            return read_events(request_payload, request_payload_size,
                               response_payload, response_capacity);
        }
        return error(ResponseStatus::UnsupportedMessage, response_payload,
                     response_capacity);
    }

private:
    static DiagnosticsHandlerResult error(
        ResponseStatus status, uint8_t* output, size_t capacity) {
        if (output == nullptr || capacity == 0) return {};
        output[0] = static_cast<uint8_t>(status);
        return {1};
    }

    DiagnosticsHandlerResult get_info(
        size_t request_size, uint8_t* output, size_t capacity) const {
        if (request_size != 0) {
            return error(ResponseStatus::MalformedRequest, output, capacity);
        }
        if (output == nullptr || capacity < kDiagnosticInfoResponseBytes) {
            return error(ResponseStatus::InternalError, output, capacity);
        }
        const DiagnosticInfoSnapshot info = source_.info();
        output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
        output[1] = info.reset_cause;
        output[2] = info.live_usb_enabled;
        diagnostics_wire::write_u16(info.stored_event_count, output + 3);
        diagnostics_wire::write_u16(info.event_capacity, output + 5);
        diagnostics_wire::write_u32(info.dropped_event_count, output + 7);
        diagnostics_wire::write_u32(info.first_event_id, output + 11);
        diagnostics_wire::write_u32(info.next_event_id, output + 15);
        diagnostics_wire::write_u32(info.last_error_event_id, output + 19);
        diagnostics_wire::write_u16(info.last_error_code, output + 23);
        return {kDiagnosticInfoResponseBytes};
    }

    DiagnosticsHandlerResult get_fault(
        size_t request_size, uint8_t* output, size_t capacity) const {
        if (request_size != 0) {
            return error(ResponseStatus::MalformedRequest, output, capacity);
        }
        if (output == nullptr || capacity < 2) {
            return error(ResponseStatus::InternalError, output, capacity);
        }
        output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
        output[1] = source_.has_fault_snapshot() ? 1u : 0u;
        if (output[1] == 0u) return {2};
        if (capacity < kFaultSnapshotResponseBytes) {
            return error(ResponseStatus::InternalError, output, capacity);
        }
        diagnostics_wire::encode_fault_fields(source_.fault_snapshot(), output + 2);
        return {kFaultSnapshotResponseBytes};
    }

    DiagnosticsHandlerResult get_persistent(
        size_t request_size, uint8_t* output, size_t capacity) const {
        if (request_size != 0) {
            return error(ResponseStatus::MalformedRequest, output, capacity);
        }
        if (output == nullptr || capacity < 2) {
            return error(ResponseStatus::InternalError, output, capacity);
        }
        PersistentDiagnosticRecord record{};
        output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
        output[1] = source_.persistent_record(record) ? 1u : 0u;
        if (output[1] == 0u) return {2};
        if (capacity < kPersistentDiagnosticRecordResponseBytes) {
            return error(ResponseStatus::InternalError, output, capacity);
        }
        diagnostics_wire::write_u32(record.generation, output + 2);
        diagnostics_wire::write_u32(record.boot_counter, output + 6);
        diagnostics_wire::write_u32(record.firmware_version, output + 10);
        diagnostics_wire::write_u32(record.event_id, output + 14);
        diagnostics_wire::write_u32(record.time_ms, output + 18);
        diagnostics_wire::write_u16(record.event_code, output + 22);
        output[24] = record.severity;
        output[25] = record.repeat_count;
        output[26] = record.reset_cause;
        output[27] = 0;
        diagnostics_wire::write_u64(record.sample_seq, output + 28);
        diagnostics_wire::write_u32(record.last_progress_ms, output + 36);
        diagnostics_wire::write_u32(record.fifo_no_data, output + 40);
        diagnostics_wire::write_u32(record.sensor_errors, output + 44);
        diagnostics_wire::write_u32(record.dropped_samples, output + 48);
        diagnostics_wire::write_u32(record.rx_overflow_count, output + 52);
        diagnostics_wire::write_u32(record.packet_overwrite_count, output + 56);
        diagnostics_wire::write_u32(record.debug_gpio_int1_edges, output + 60);
        diagnostics_wire::write_u32(record.debug_gpio_drdy_edges, output + 64);
        diagnostics_wire::write_u32(record.debug_config_snapshot, output + 68);
        diagnostics_wire::write_u32(record.debug_irq_snapshot, output + 72);
        diagnostics_wire::write_u32(static_cast<uint32_t>(record.arg0), output + 76);
        diagnostics_wire::write_u32(static_cast<uint32_t>(record.arg1), output + 80);
        return {kPersistentDiagnosticRecordResponseBytes};
    }

    DiagnosticsHandlerResult read_events(
        const uint8_t* request, size_t request_size,
        uint8_t* output, size_t capacity) const {
        if (request == nullptr || request_size != kReadDiagnosticEventsRequestBytes) {
            return error(ResponseStatus::MalformedRequest, output, capacity);
        }
        const uint8_t requested = request[4];
        if (requested == 0 || requested > kMaxDiagnosticEventsPerResponse) {
            return error(ResponseStatus::MalformedRequest, output, capacity);
        }
        const size_t required = kDiagnosticEventsResponseHeaderBytes +
            static_cast<size_t>(requested) * kDiagnosticEventWireBytes;
        if (output == nullptr || capacity < required) {
            return error(ResponseStatus::InternalError, output, capacity);
        }
        DiagnosticEvent events[kMaxDiagnosticEventsPerResponse]{};
        uint32_t next_event_id = 0;
        uint32_t first_event_id = 0;
        const size_t count = source_.read_events(
            diagnostics_wire::read_u32(request), events, requested,
            next_event_id, first_event_id);
        output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
        output[1] = static_cast<uint8_t>(count);
        output[2] = 0;
        output[3] = 0;
        diagnostics_wire::write_u32(first_event_id, output + 4);
        diagnostics_wire::write_u32(next_event_id, output + 8);
        for (size_t index = 0; index < count; ++index) {
            uint8_t* encoded = output + kDiagnosticEventsResponseHeaderBytes +
                index * kDiagnosticEventWireBytes;
            diagnostics_wire::write_u32(events[index].event_id, encoded + 0);
            diagnostics_wire::write_u32(events[index].time_ms, encoded + 4);
            diagnostics_wire::write_u16(events[index].code, encoded + 8);
            encoded[10] = events[index].severity;
            encoded[11] = events[index].repeat_count;
            diagnostics_wire::write_u64(events[index].sample_seq, encoded + 12);
            diagnostics_wire::write_u32(
                static_cast<uint32_t>(events[index].arg0), encoded + 20);
            diagnostics_wire::write_u32(
                static_cast<uint32_t>(events[index].arg1), encoded + 24);
        }
        return {kDiagnosticEventsResponseHeaderBytes +
                count * kDiagnosticEventWireBytes};
    }

    DiagnosticsSource& source_;
};

}  // namespace ProtocolV3
