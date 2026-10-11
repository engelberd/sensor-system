#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstring>

#include "common/protocol_ids.h"
#include "controller/command_result.h"
#include "interfaces/i_data_plane.h"
#include "system/runtime_context.h"
#include "transport/frame_decoder.h"
#include "transport/frame_encoder.h"
#include "transport/transport.h"

namespace {

class FakePort {
public:
    void poll() {}

    bool read_byte(uint8_t& byte) {
        if (rx_offset_ >= rx_size_) {
            return false;
        }
        byte = rx_[rx_offset_++];
        return true;
    }

    bool start_tx(const uint8_t* data, size_t size) {
        if (tx_busy_ || data == nullptr || size > sizeof(tx_)) {
            return false;
        }
        std::memcpy(tx_, data, size);
        tx_size_ = size;
        tx_busy_ = true;
        tx_completed_ = false;
        return true;
    }

    bool tx_busy() const { return tx_busy_; }
    bool tx_completed() const { return tx_completed_; }
    void clear_tx_completed() { tx_completed_ = false; }

    void set_baudrate(uint32_t baudrate) {
        applied_baudrate = baudrate;
    }

    void push_rx(const uint8_t* data, size_t size) {
        assert(data != nullptr);
        assert(size <= sizeof(rx_));
        std::memcpy(rx_, data, size);
        rx_size_ = size;
        rx_offset_ = 0;
    }

    void complete_tx() {
        assert(tx_busy_);
        tx_busy_ = false;
        tx_completed_ = true;
    }

    const uint8_t* tx_data() const { return tx_; }
    size_t tx_size() const { return tx_size_; }

    uint32_t applied_baudrate = 0;

private:
    uint8_t rx_[128]{};
    size_t rx_size_ = 0;
    size_t rx_offset_ = 0;
    uint8_t tx_[128]{};
    size_t tx_size_ = 0;
    bool tx_busy_ = false;
    bool tx_completed_ = false;
};

class FakeDataPlane final : public IDataPlane {
public:
    bool burst_active() const override { return false; }
    uint8_t burst_destination() const override { return 0; }
    StatusCode start_burst(uint64_t, uint16_t, uint8_t) override {
        return StatusCode::NoData;
    }
    StatusCode commit_read_up_to(uint64_t) override { return StatusCode::Ok; }
    void finish_burst(BurstEndReason) override {}
    DataPlaneState state() const override { return {}; }
    bool try_build_current_packet_payload(
        uint8_t*, size_t, size_t& out_size
    ) override {
        out_size = 0;
        return false;
    }
    void on_packet_transmitted() override {}
};

class FakeController {
public:
    uint8_t node_id() const { return 1; }
    bool accepts_broadcast_command(const uint8_t*, size_t) const {
        return false;
    }

    CommandResult handle_command(uint8_t command,
                                 uint8_t,
                                 const uint8_t*,
                                 size_t,
                                 uint8_t* response,
                                 size_t& response_len) {
        response[0] = command;
        response[1] = static_cast<uint8_t>(StatusCode::Ok);
        response_len = 2;

        if (command == 0x02) {
            return {
                StatusCode::Ok,
                {DeferredCommandEffectType::Restart, 0}
            };
        }
        if (command == 0x29) {
            return {
                StatusCode::Ok,
                {DeferredCommandEffectType::ApplyRs485BaudRate, 57600}
            };
        }
        return {StatusCode::Ok, {}};
    }
};

using TestTransport = Transport<FakeController, FakePort>;

size_t encode_command(uint8_t command, uint32_t sequence, uint8_t* out) {
    FrameEncoder encoder{};
    return encoder.encode_frame_to(
        FrameType::Command,
        1,
        HOST_NODE_ID,
        sequence,
        0,
        &command,
        1,
        out,
        128
    );
}

void assert_ok_response(const FakePort& port,
                        uint8_t command,
                        uint32_t sequence) {
    FrameDecoder decoder{};
    DecodedFrame frame{};
    assert(
        decoder.decode(port.tx_data(), port.tx_size(), frame) ==
        FrameDecodeStatus::Ok
    );
    assert(frame.type == FrameType::Response);
    assert(frame.destination == HOST_NODE_ID);
    assert(frame.source == 1);
    assert(frame.sequence == sequence);
    assert(frame.payload_length == 2);
    assert(frame.payload[0] == command);
    assert(frame.payload[1] == static_cast<uint8_t>(StatusCode::Ok));
}

void test_restart_effect_runs_only_after_response_tx_completes() {
    FakePort port{};
    FakeController controller{};
    FakeDataPlane data_plane{};
    volatile uint8_t requested_action = RuntimeSystemActionNone;
    TestTransport transport(
        port,
        controller,
        data_plane,
        &requested_action
    );
    assert(transport.init());

    uint8_t frame[128]{};
    const size_t frame_size = encode_command(0x02, 41, frame);
    port.push_rx(frame, frame_size);
    transport.update();

    assert(requested_action == RuntimeSystemActionNone);
    assert_ok_response(port, 0x02, 41);

    port.complete_tx();
    transport.update();
    assert(requested_action == RuntimeSystemActionRestart);
}

void test_baudrate_effect_stays_attached_to_its_response() {
    FakePort port{};
    FakeController controller{};
    FakeDataPlane data_plane{};
    TestTransport transport(port, controller, data_plane);
    assert(transport.init());

    uint8_t frame[128]{};
    const size_t frame_size = encode_command(0x29, 42, frame);
    port.push_rx(frame, frame_size);
    transport.update();

    assert(port.applied_baudrate == 0);
    assert_ok_response(port, 0x29, 42);

    port.complete_tx();
    transport.update();
    assert(port.applied_baudrate == 57600);
}

}  // namespace

int main() {
    test_restart_effect_runs_only_after_response_tx_completes();
    test_baudrate_effect_stays_attached_to_its_response();
    return 0;
}
