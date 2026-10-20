#pragma once

#include <cstddef>
#include <cstdint>

#include "protocol_v3/application.h"
#include "telemetry/telemetry_snapshot.h"

namespace ProtocolV3 {

constexpr uint8_t kTelemetryServiceId =
    static_cast<uint8_t>(ServiceId::Telemetry);
constexpr uint8_t kGetTelemetrySnapshotMessageId = 1;
constexpr uint8_t kTelemetrySchemaVersion = 1;
constexpr size_t kTelemetrySnapshotResponseBytes = 14;

struct TelemetryHandlerResult { size_t payload_size = 0; };

class ITelemetryServiceHandler {
public:
    virtual ~ITelemetryServiceHandler() = default;
    virtual TelemetryHandlerResult handle(
        uint8_t message,
        const uint8_t* request_payload,
        size_t request_payload_size,
        uint8_t* response_payload,
        size_t response_capacity) = 0;
};

inline void write_telemetry_u16(uint16_t value, uint8_t* output) {
    output[0] = static_cast<uint8_t>(value);
    output[1] = static_cast<uint8_t>(value >> 8u);
}

inline void write_telemetry_u64(uint64_t value, uint8_t* output) {
    for (size_t index = 0; index < 8; ++index) {
        output[index] = static_cast<uint8_t>(value >> (8u * index));
    }
}

template <typename SnapshotSource>
class TelemetryServiceHandler final : public ITelemetryServiceHandler {
public:
    explicit TelemetryServiceHandler(SnapshotSource& source)
        : source_(source) {}

    TelemetryHandlerResult handle(
        uint8_t message,
        const uint8_t*,
        size_t request_payload_size,
        uint8_t* output,
        size_t capacity) override {
        if (output == nullptr || capacity == 0) return {};
        if (message != kGetTelemetrySnapshotMessageId) {
            return error(ResponseStatus::UnsupportedMessage, output);
        }
        if (request_payload_size != 0) {
            return error(ResponseStatus::MalformedRequest, output);
        }
        if (capacity < kTelemetrySnapshotResponseBytes) {
            return error(ResponseStatus::InternalError, output);
        }

        telemetry::Snapshot snapshot{};
        if (!source_.try_read(snapshot)) {
            return error(ResponseStatus::Busy, output);
        }
        if (snapshot.captured_device_time_us == 0 ||
            snapshot.valid_fields != telemetry::kAdxl355TemperatureField) {
            return error(ResponseStatus::InternalError, output);
        }

        output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
        output[1] = kTelemetrySchemaVersion;
        write_telemetry_u64(snapshot.captured_device_time_us, output + 2);
        write_telemetry_u16(snapshot.valid_fields, output + 10);
        write_telemetry_u16(snapshot.adxl355_temperature_raw, output + 12);
        return {kTelemetrySnapshotResponseBytes};
    }

private:
    static TelemetryHandlerResult error(
        ResponseStatus status, uint8_t* output) {
        output[0] = static_cast<uint8_t>(status);
        return {1};
    }

    SnapshotSource& source_;
};

}  // namespace ProtocolV3
