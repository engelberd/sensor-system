#pragma once

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <cstring>

#include "interfaces/i_usb_observer_port.h"
#include "observer/observer_snapshot_sink.h"
#include "observer/usb_observer_protocol.h"
#include "transport/frame_decoder.h"
#include "transport/frame_encoder.h"
#include "transport/frame_layout.h"
#include "transport/status_codes.h"

template <size_t SnapshotCapacity>
class UsbObserverEndpoint {
public:
    static constexpr size_t kSnapshotHeaderSize = 24;
    static constexpr size_t kMaxFrameSize =
        FRAME_HEADER_SIZE + FRAME_MAX_PAYLOAD_SIZE + FRAME_CRC_SIZE;
    static constexpr size_t kMaxWireSamples =
        (FRAME_MAX_PAYLOAD_SIZE - kSnapshotHeaderSize) /
        USB_OBSERVER_SAMPLE_WIRE_SIZE;

    UsbObserverEndpoint(IUsbObserverPort& port,
                        ObserverSnapshotSink<SnapshotCapacity>& snapshots,
                        const IUsbObserverIdentitySource& identity_source)
        : port_(port),
          snapshots_(snapshots),
          identity_source_(identity_source) {}

    void update() {
        port_.poll();

        if (!port_.connected()) {
            reset_connection_state();
            return;
        }

        transmit_bounded();
        if (tx_offset_ < tx_size_) {
            return;
        }

        receive_bounded();
        transmit_bounded();
    }

private:
    static constexpr size_t kCapabilitiesPayloadSize = 24;
    static constexpr size_t kStatsPayloadSize = 18;
    static constexpr size_t kRxBufferSize =
        FRAME_HEADER_SIZE + USB_OBSERVER_MAX_REQUEST_PAYLOAD + FRAME_CRC_SIZE;

    static void write_u16(uint8_t* output, size_t& offset, uint16_t value) {
        output[offset++] = static_cast<uint8_t>(value);
        output[offset++] = static_cast<uint8_t>(value >> 8);
    }

    static void write_u32(uint8_t* output, size_t& offset, uint32_t value) {
        for (uint8_t shift = 0; shift < 32; shift += 8) {
            output[offset++] = static_cast<uint8_t>(value >> shift);
        }
    }

    static void write_u64(uint8_t* output, size_t& offset, uint64_t value) {
        for (uint8_t shift = 0; shift < 64; shift += 8) {
            output[offset++] = static_cast<uint8_t>(value >> shift);
        }
    }

    static void write_i32(uint8_t* output, size_t& offset, int32_t value) {
        write_u32(output, offset, static_cast<uint32_t>(value));
    }

    void reset_connection_state() {
        rx_size_ = 0;
        tx_size_ = 0;
        tx_offset_ = 0;
    }

    void transmit_bounded() {
        if (tx_offset_ >= tx_size_) {
            return;
        }

        const size_t remaining = tx_size_ - tx_offset_;
        const size_t budgeted = std::min(remaining, USB_OBSERVER_TX_BUDGET);
        tx_offset_ += port_.write_some(tx_buffer_ + tx_offset_, budgeted);

        if (tx_offset_ == tx_size_) {
            tx_offset_ = 0;
            tx_size_ = 0;
        }
    }

    void receive_bounded() {
        size_t processed = 0;
        uint8_t byte = 0;
        while (processed < USB_OBSERVER_RX_BUDGET && port_.read_byte(byte)) {
            ++processed;

            if (rx_size_ == kRxBufferSize) {
                discard_rx_prefix(1);
            }
            rx_buffer_[rx_size_++] = byte;
            align_to_magic();

            if (try_process_frame()) {
                return;
            }
        }
    }

    void align_to_magic() {
        const uint8_t magic_low = static_cast<uint8_t>(FRAME_MAGIC);
        const uint8_t magic_high = static_cast<uint8_t>(FRAME_MAGIC >> 8);

        while (rx_size_ >= 2 &&
               (rx_buffer_[0] != magic_low || rx_buffer_[1] != magic_high)) {
            discard_rx_prefix(1);
        }
    }

    bool try_process_frame() {
        if (rx_size_ < FRAME_HEADER_SIZE) {
            return false;
        }

        FrameHeader header{};
        std::memcpy(&header, rx_buffer_, sizeof(header));
        if (header.version != FRAME_PROTOCOL_VERSION ||
            header.length > USB_OBSERVER_MAX_REQUEST_PAYLOAD) {
            discard_rx_prefix(1);
            align_to_magic();
            return false;
        }

        const size_t expected_size =
            FRAME_HEADER_SIZE + header.length + FRAME_CRC_SIZE;
        if (rx_size_ < expected_size) {
            return false;
        }

        DecodedFrame frame{};
        const FrameDecodeStatus decode_status =
            decoder_.decode(rx_buffer_, expected_size, frame);
        discard_rx_prefix(expected_size);

        if (decode_status != FrameDecodeStatus::Ok ||
            frame.type != FrameType::Command ||
            frame.payload_length != 1) {
            return false;
        }

        build_response(frame);
        return tx_size_ != 0;
    }

    void discard_rx_prefix(size_t count) {
        if (count >= rx_size_) {
            rx_size_ = 0;
            return;
        }

        std::memmove(rx_buffer_, rx_buffer_ + count, rx_size_ - count);
        rx_size_ -= count;
    }

    void build_response(const DecodedFrame& request) {
        const UsbObserverIdentity identity = identity_source_.current();
        const auto command = static_cast<UsbObserverCommand>(request.payload[0]);
        size_t payload_size = 0;

        switch (command) {
            case UsbObserverCommand::GetCapabilities:
                payload_size = encode_capabilities(payload_buffer_);
                break;
            case UsbObserverCommand::ReadSnapshot:
                payload_size = encode_snapshot(payload_buffer_);
                break;
            case UsbObserverCommand::GetStats:
                payload_size = encode_stats(payload_buffer_);
                break;
            default:
                payload_buffer_[0] = request.payload[0];
                payload_buffer_[1] = static_cast<uint8_t>(StatusCode::Unsupported);
                payload_size = 2;
                break;
        }

        tx_size_ = encoder_.encode_response_frame_to(
            request.source,
            identity.node_id,
            request.sequence,
            FRAME_FLAG_NONE,
            payload_buffer_,
            static_cast<uint16_t>(payload_size),
            tx_buffer_,
            sizeof(tx_buffer_)
        );
        tx_offset_ = 0;
    }

    size_t encode_capabilities(uint8_t* payload) const {
        const UsbObserverIdentity identity = identity_source_.current();
        size_t offset = 0;
        payload[offset++] = static_cast<uint8_t>(UsbObserverCommand::GetCapabilities);
        payload[offset++] = static_cast<uint8_t>(StatusCode::Ok);
        payload[offset++] = USB_OBSERVER_FORMAT_VERSION;
        payload[offset++] = identity.product_protocol_version;
        payload[offset++] = identity.board_revision;
        payload[offset++] = identity.timestamping_enabled ? 1 : 0;
        payload[offset++] = identity.node_id;
        payload[offset++] = 0;
        payload[offset++] = identity.firmware_major;
        payload[offset++] = identity.firmware_minor;
        payload[offset++] = identity.firmware_patch;
        payload[offset++] = 0;
        write_u16(payload, offset, static_cast<uint16_t>(SnapshotCapacity));
        write_u16(payload, offset, static_cast<uint16_t>(kMaxWireSamples));
        for (size_t i = 0; i < DEVICE_HARDWARE_ID_SIZE; ++i) {
            payload[offset++] = identity.hardware_id.bytes[i];
        }
        return offset;
    }

    size_t encode_stats(uint8_t* payload) const {
        const ObserverSnapshotStats stats = snapshots_.stats();
        size_t offset = 0;
        payload[offset++] = static_cast<uint8_t>(UsbObserverCommand::GetStats);
        payload[offset++] = static_cast<uint8_t>(StatusCode::Ok);
        payload[offset++] = USB_OBSERVER_FORMAT_VERSION;
        payload[offset++] = 0;
        write_u32(payload, offset, stats.published_batches);
        write_u32(payload, offset, stats.truncated_batches);
        write_u32(payload, offset, stats.read_retries);
        write_u16(payload, offset, static_cast<uint16_t>(SnapshotCapacity));
        return offset;
    }

    size_t encode_snapshot(uint8_t* payload) {
        if (!snapshots_.read_latest(snapshot_buffer_)) {
            payload[0] = static_cast<uint8_t>(UsbObserverCommand::ReadSnapshot);
            payload[1] = static_cast<uint8_t>(StatusCode::NoData);
            return 2;
        }

        const size_t returned_count =
            std::min(snapshot_buffer_.sample_count, kMaxWireSamples);
        const size_t first_index = snapshot_buffer_.sample_count - returned_count;
        const ObserverSnapshotStats stats = snapshots_.stats();

        size_t offset = 0;
        payload[offset++] = static_cast<uint8_t>(UsbObserverCommand::ReadSnapshot);
        payload[offset++] = static_cast<uint8_t>(StatusCode::Ok);
        payload[offset++] = USB_OBSERVER_FORMAT_VERSION;
        payload[offset++] = first_index == 0 ? 0 : 1;
        write_u32(payload, offset, snapshot_buffer_.generation);
        write_u16(payload, offset, static_cast<uint16_t>(snapshot_buffer_.sample_count));
        write_u16(payload, offset, static_cast<uint16_t>(returned_count));
        write_u16(payload, offset, static_cast<uint16_t>(first_index));
        write_u16(payload, offset, static_cast<uint16_t>(USB_OBSERVER_SAMPLE_WIRE_SIZE));
        write_u32(payload, offset, stats.published_batches);
        write_u32(payload, offset, stats.truncated_batches);

        for (size_t i = first_index; i < snapshot_buffer_.sample_count; ++i) {
            const StoredSample& sample = snapshot_buffer_.samples[i];
            const SampleDeviceTime& time = snapshot_buffer_.times[i];
            write_u64(payload, offset, sample.sample_seq);
            write_i32(payload, offset, sample.x);
            write_i32(payload, offset, sample.y);
            write_i32(payload, offset, sample.z);
            write_u64(payload, offset, time.device_time_us);
            write_u32(payload, offset, time.timing_segment_id);
            write_u16(payload, offset, time.quality_flags);
            payload[offset++] = static_cast<uint8_t>(time.source);
            payload[offset++] = time.reserved;
        }

        return offset;
    }

    IUsbObserverPort& port_;
    ObserverSnapshotSink<SnapshotCapacity>& snapshots_;
    const IUsbObserverIdentitySource& identity_source_;
    FrameDecoder decoder_{};
    FrameEncoder encoder_{};
    uint8_t rx_buffer_[kRxBufferSize]{};
    size_t rx_size_ = 0;
    uint8_t payload_buffer_[FRAME_MAX_PAYLOAD_SIZE]{};
    typename ObserverSnapshotSink<SnapshotCapacity>::Snapshot snapshot_buffer_{};
    uint8_t tx_buffer_[kMaxFrameSize]{};
    size_t tx_size_ = 0;
    size_t tx_offset_ = 0;
};
