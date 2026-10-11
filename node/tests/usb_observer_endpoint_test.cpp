#include <algorithm>
#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstring>

#include "common/protocol_ids.h"
#include "observer/usb_observer_endpoint.h"
#include "transport/frame_decoder.h"
#include "transport/frame_encoder.h"

namespace {

class FakeUsbPort final : public IUsbObserverPort {
public:
    void poll() override { ++poll_calls; }
    bool connected() const override { return is_connected; }

    bool read_byte(uint8_t& byte) override {
        if (rx_offset >= rx_size) {
            return false;
        }
        byte = rx[rx_offset++];
        return true;
    }

    size_t write_some(const uint8_t* data, size_t size) override {
        max_write_request = std::max(max_write_request, size);
        const size_t available = sizeof(tx) - tx_size;
        const size_t count = std::min(size, available);
        std::memcpy(tx + tx_size, data, count);
        tx_size += count;
        return count;
    }

    void push(const uint8_t* data, size_t size) {
        assert(size <= sizeof(rx));
        std::memcpy(rx, data, size);
        rx_size = size;
        rx_offset = 0;
    }

    bool is_connected = true;
    size_t poll_calls = 0;
    size_t max_write_request = 0;
    uint8_t rx[128]{};
    size_t rx_size = 0;
    size_t rx_offset = 0;
    uint8_t tx[2048]{};
    size_t tx_size = 0;
};

using SnapshotSink = ObserverSnapshotSink<32>;
using Endpoint = UsbObserverEndpoint<32>;

class FixedIdentitySource final : public IUsbObserverIdentitySource {
public:
    explicit FixedIdentitySource(const UsbObserverIdentity& value) : value_(value) {}
    UsbObserverIdentity current() const override { return value_; }

private:
    UsbObserverIdentity value_{};
};

size_t command_frame(UsbObserverCommand command,
                     uint32_t sequence,
                     uint8_t* output) {
    const uint8_t payload = static_cast<uint8_t>(command);
    FrameEncoder encoder{};
    return encoder.encode_frame_to(
        FrameType::Command,
        1,
        HOST_NODE_ID,
        sequence,
        FRAME_FLAG_NONE,
        &payload,
        1,
        output,
        128
    );
}

DecodedFrame decode_response(const FakeUsbPort& port) {
    DecodedFrame frame{};
    FrameDecoder decoder{};
    assert(decoder.decode(port.tx, port.tx_size, frame) == FrameDecodeStatus::Ok);
    assert(frame.type == FrameType::Response);
    return frame;
}

UsbObserverIdentity identity() {
    UsbObserverIdentity value{};
    value.node_id = 7;
    value.firmware_major = 1;
    value.firmware_minor = 2;
    value.firmware_patch = 3;
    value.product_protocol_version = 4;
    value.board_revision = 2;
    value.timestamping_enabled = true;
    for (size_t i = 0; i < DEVICE_HARDWARE_ID_SIZE; ++i) {
        value.hardware_id.bytes[i] = static_cast<uint8_t>(0xA0 + i);
    }
    return value;
}

void run_until_response_complete(Endpoint& endpoint,
                                 FakeUsbPort& port,
                                 size_t expected_size) {
    for (size_t i = 0; i < 64 && port.tx_size < expected_size; ++i) {
        endpoint.update();
    }
    assert(port.tx_size == expected_size);
}

void test_capabilities_are_read_only_and_bounded() {
    FakeUsbPort port{};
    SnapshotSink snapshots{};
    FixedIdentitySource identity_source(identity());
    Endpoint endpoint(port, snapshots, identity_source);
    uint8_t request[128]{};
    const size_t request_size = command_frame(
        UsbObserverCommand::GetCapabilities, 42, request
    );
    port.push(request, request_size);

    endpoint.update();

    const DecodedFrame response = decode_response(port);
    assert(response.sequence == 42);
    assert(response.source == 7);
    assert(response.payload_length == 24);
    assert(response.payload[0] ==
           static_cast<uint8_t>(UsbObserverCommand::GetCapabilities));
    assert(response.payload[1] == static_cast<uint8_t>(StatusCode::Ok));
    assert(response.payload[4] == 2);
    assert(response.payload[5] == 1);
    assert(port.max_write_request <= USB_OBSERVER_TX_BUDGET);
}

void test_snapshot_response_keeps_latest_wire_subset() {
    FakeUsbPort port{};
    SnapshotSink snapshots{};
    FixedIdentitySource identity_source(identity());
    Endpoint endpoint(port, snapshots, identity_source);
    StoredSample samples[32]{};
    SampleDeviceTime times[32]{};
    for (size_t i = 0; i < 32; ++i) {
        samples[i].sample_seq = 100 + i;
        samples[i].x = static_cast<int32_t>(i);
        times[i].device_time_us = 1000 + i;
        times[i].timing_segment_id = 9;
        times[i].quality_flags = TIMING_QUALITY_LOCKED;
        times[i].source = TimestampSource::DrdyTimeUs64;
    }
    snapshots.on_timed_samples(samples, times, 32);

    uint8_t request[128]{};
    const size_t request_size = command_frame(
        UsbObserverCommand::ReadSnapshot, 9, request
    );
    port.push(request, request_size);

    const size_t expected_payload =
        Endpoint::kSnapshotHeaderSize +
        Endpoint::kMaxWireSamples * USB_OBSERVER_SAMPLE_WIRE_SIZE;
    const size_t expected_frame =
        FRAME_HEADER_SIZE + expected_payload + FRAME_CRC_SIZE;
    run_until_response_complete(endpoint, port, expected_frame);

    const DecodedFrame response = decode_response(port);
    assert(response.payload_length == expected_payload);
    assert(response.payload[0] ==
           static_cast<uint8_t>(UsbObserverCommand::ReadSnapshot));
    assert(response.payload[1] == static_cast<uint8_t>(StatusCode::Ok));
    assert(response.payload[3] == 1);
    assert(response.payload[8] == 32);
    assert(response.payload[10] == Endpoint::kMaxWireSamples);
    assert(response.payload[12] == 32 - Endpoint::kMaxWireSamples);

    uint64_t first_wire_sequence = 0;
    std::memcpy(
        &first_wire_sequence,
        response.payload + Endpoint::kSnapshotHeaderSize,
        sizeof(first_wire_sequence)
    );
    assert(first_wire_sequence == 100 + 32 - Endpoint::kMaxWireSamples);
    assert(port.max_write_request <= USB_OBSERVER_TX_BUDGET);
    assert(port.poll_calls > 1);
}

void test_no_snapshot_returns_no_data() {
    FakeUsbPort port{};
    SnapshotSink snapshots{};
    FixedIdentitySource identity_source(identity());
    Endpoint endpoint(port, snapshots, identity_source);
    uint8_t request[128]{};
    const size_t request_size = command_frame(
        UsbObserverCommand::ReadSnapshot, 3, request
    );
    port.push(request, request_size);

    endpoint.update();

    const DecodedFrame response = decode_response(port);
    assert(response.payload_length == 2);
    assert(response.payload[1] == static_cast<uint8_t>(StatusCode::NoData));
}

}  // namespace

int main() {
    test_capabilities_are_read_only_and_bounded();
    test_snapshot_response_keeps_latest_wire_subset();
    test_no_snapshot_returns_no_data();
    return 0;
}
