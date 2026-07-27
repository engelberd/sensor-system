#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>

#include "common/protocol_ids.h"
#include "interfaces/i_data_plane.h"
#include "pico/stdlib.h"
#include "system/runtime_context.h"
#include "transport/frame_decoder.h"
#include "transport/frame_encoder.h"
#include "transport/frame_layout.h"
#include "transport/rs485_port.h"

template <size_t BufferCapacity, typename ControllerT>
class Transport {
public:
    static constexpr size_t MAX_FRAME_SIZE =
        FRAME_HEADER_SIZE + FRAME_MAX_PAYLOAD_SIZE + FRAME_CRC_SIZE;
    static constexpr size_t RX_BUFFER_SIZE = MAX_FRAME_SIZE;
    static constexpr size_t TX_FRAME_BUFFER_SIZE = MAX_FRAME_SIZE;
    static constexpr size_t RESPONSE_PAYLOAD_BUFFER_SIZE = FRAME_MAX_PAYLOAD_SIZE;
    static constexpr size_t MAX_PENDING_RESPONSES = 4;

    Transport(Rs485Port& port,
              ControllerT& controller,
              IDataPlane& data_plane,
              volatile uint8_t* requested_action = nullptr,
              uint64_t boot_epoch = 0,
              bool time_sync_enabled = false)
        : port_(port),
          controller_(controller),
          data_plane_(data_plane),
          requested_action_(requested_action),
          boot_epoch_(boot_epoch),
          time_sync_enabled_(time_sync_enabled && boot_epoch != 0) {
    }

    bool init() {
        rx_size_ = 0;
        tx_seq_ = 1;
        tx_kind_ = TxKind::None;
        pending_response_head_ = 0;
        pending_response_tail_ = 0;
        pending_response_count_ = 0;
        return true;
    }

    void update() {
        port_.poll();
        handle_tx_completion();
        process_rx();
        process_tx();
    }

private:
    static constexpr uint8_t kMagicLo =
        static_cast<uint8_t>(FRAME_MAGIC & 0xFFu);
    static constexpr uint8_t kMagicHi =
        static_cast<uint8_t>((FRAME_MAGIC >> 8) & 0xFFu);

    enum class TxKind : uint8_t {
        None = 0,
        Response,
        BurstData
    };

    using DeferredActionT = typename ControllerT::DeferredAction;

    void handle_tx_completion() {
        if (tx_kind_ == TxKind::None) {
            return;
        }

        if (port_.tx_busy()) {
            return;
        }

        if (!port_.tx_completed()) {
            return;
        }

        port_.clear_tx_completed();

        if (tx_kind_ == TxKind::BurstData) {
            data_plane_.on_packet_transmitted();
        } else if (tx_kind_ == TxKind::Response) {
            execute_deferred_action(active_response_action_, active_response_baudrate_);
            active_response_action_ = DeferredActionT::None;
            active_response_baudrate_ = 0;
        }

        tx_kind_ = TxKind::None;
    }

    void process_rx() {
        uint8_t byte = 0;

        while (port_.read_byte(byte)) {
            if (rx_size_ >= RX_BUFFER_SIZE) {
                rx_size_ = 0;
            }

            rx_buffer_[rx_size_++] = byte;

            align_rx_buffer_to_magic();

            while (true) {
                if (rx_size_ < FRAME_HEADER_SIZE) {
                    break;
                }

                FrameHeader header{};
                std::memcpy(&header, rx_buffer_, sizeof(header));

                if (header.version != FRAME_PROTOCOL_VERSION) {
                    discard_rx_prefix(1);
                    continue;
                }

                if (header.length > FRAME_MAX_PAYLOAD_SIZE) {
                    discard_rx_prefix(1);
                    continue;
                }

                const size_t expected_size =
                    FRAME_HEADER_SIZE +
                    static_cast<size_t>(header.length) +
                    FRAME_CRC_SIZE;

                if (expected_size > RX_BUFFER_SIZE) {
                    discard_rx_prefix(1);
                    continue;
                }

                if (rx_size_ < expected_size) {
                    break;
                }

                if (!try_decode_one_frame(expected_size)) {
                    // decode failed and buffer was resynced; try again
                    continue;
                }

                // one frame consumed successfully; maybe another one is already buffered
            }
        }
    }

    bool try_decode_one_frame(size_t frame_size) {
        DecodedFrame frame{};
        const auto st = decoder_.decode(rx_buffer_, frame_size, frame);

        if (st != FrameDecodeStatus::Ok) {
            discard_rx_prefix(1);
            align_rx_buffer_to_magic();
            return false;
        }

        handle_frame(frame, time_us_64());
        discard_rx_prefix(frame_size);
        align_rx_buffer_to_magic();
        return true;
    }

    void execute_deferred_action(DeferredActionT action, uint32_t deferred_baudrate) {
        if (action == DeferredActionT::None) {
            return;
        }

        if (action == DeferredActionT::ApplyBaudRate) {
            if (deferred_baudrate != 0) {
                port_.set_baudrate(deferred_baudrate);
            }
            return;
        }

        if (requested_action_ == nullptr) {
            return;
        }

        if (action == DeferredActionT::EnterBootloader) {
            *requested_action_ = RuntimeSystemActionEnterBootloader;
        } else if (action == DeferredActionT::Restart) {
            *requested_action_ = RuntimeSystemActionRestart;
        }
    }

    void handle_frame(const DecodedFrame& frame, uint64_t t2_node_rx_us) {
        if (frame.type != FrameType::Command) {
            return;
        }

        // Runtime commands must originate from the host; ignore node-to-node or echo artifacts.
        if (frame.source != HOST_NODE_ID) {
            return;
        }

        if (frame.payload == nullptr || frame.payload_length < 1) {
            return;
        }

        const bool directed_to_node = frame.destination == controller_.node_id();
        const bool accepted_broadcast =
            frame.destination == BROADCAST_NODE_ID &&
            controller_.accepts_broadcast_command(frame.payload, frame.payload_length);
        if (!directed_to_node && !accepted_broadcast) {
            return;
        }

        uint8_t response_payload[RESPONSE_PAYLOAD_BUFFER_SIZE]{};
        size_t response_payload_len = 0;

        const uint8_t cmd = frame.payload[0];

        if (time_sync_enabled_ &&
            cmd == static_cast<uint8_t>(CommandType::TimeSync)) {
            (void)queue_time_sync_response(frame, t2_node_rx_us);
            return;
        }

        const StatusCode st = controller_.handle_command(
            cmd,
            frame.source,
            frame.payload,
            frame.payload_length,
            response_payload,
            response_payload_len
        );

        if (response_payload_len == 0 && st == StatusCode::NoData) {
            return;
        }

        // Jeśli kontroler nie zbudował payloadu dla błędu, dobuduj minimalny nagłówek.
        if (response_payload_len == 0) {
            if (sizeof(ResponsePayloadHeader) <= sizeof(response_payload)) {
                ResponsePayloadHeader resp{};
                resp.command = cmd;
                resp.status = static_cast<uint8_t>(st);
                std::memcpy(response_payload, &resp, sizeof(resp));
                response_payload_len = sizeof(resp);
            } else {
                return;
            }
        }

        if (!queue_response(
                frame.source,
                controller_.node_id(),
                frame.sequence,
                response_payload,
                response_payload_len,
                controller_.take_deferred_action(),
                controller_.take_deferred_baudrate())) {
            (void)st;
        }
    }

    bool queue_response(uint8_t destination,
                        uint8_t source,
                        uint32_t sequence,
                        const uint8_t* payload,
                        size_t payload_len,
                        DeferredActionT deferred_action,
                        uint32_t deferred_baudrate) {
        if (payload == nullptr || payload_len == 0) {
            return false;
        }

        if (pending_response_count_ >= MAX_PENDING_RESPONSES) {
            return false;
        }

        PendingResponseSlot& slot = pending_response_queue_[pending_response_tail_];
        const size_t frame_len = encoder_.encode_response_frame_to(
            destination,
            source,
            sequence,
            0,
            payload,
            static_cast<uint16_t>(payload_len),
            slot.frame,
            sizeof(slot.frame)
        );

        if (frame_len == 0) {
            return false;
        }

        slot.frame_len = frame_len;
        slot.deferred_action = deferred_action;
        slot.deferred_baudrate = deferred_baudrate;
        pending_response_tail_ =
            (pending_response_tail_ + 1) % MAX_PENDING_RESPONSES;
        ++pending_response_count_;
        return true;
    }

    bool queue_time_sync_response(const DecodedFrame& frame,
                                  uint64_t t2_node_rx_us) {
        if (pending_response_count_ >= MAX_PENDING_RESPONSES) {
            return false;
        }

        TimeSyncResponsePayload response{};
        response.command = static_cast<uint8_t>(CommandType::TimeSync);
        response.version = 1;
        response.boot_epoch = boot_epoch_;
        response.t2_node_rx_us = t2_node_rx_us;

        if (frame.payload == nullptr ||
            frame.payload_length != sizeof(TimeSyncCommandPayload)) {
            response.status = static_cast<uint8_t>(StatusCode::InvalidParam);
        } else {
            TimeSyncCommandPayload request{};
            std::memcpy(&request, frame.payload, sizeof(request));
            response.sync_id = request.sync_id;
            response.echoed_t1_host_monotonic_ns =
                request.t1_host_monotonic_ns;
            response.status = static_cast<uint8_t>(
                request.version == 1
                    ? StatusCode::Ok
                    : StatusCode::Unsupported
            );
        }

        PendingResponseSlot& slot =
            pending_response_queue_[pending_response_tail_];
        slot.frame_len = 0;
        slot.destination = frame.source;
        slot.source = controller_.node_id();
        slot.sequence = frame.sequence;
        slot.time_sync_payload = response;
        slot.late_time_sync = true;
        slot.deferred_action = DeferredActionT::None;
        slot.deferred_baudrate = 0;
        pending_response_tail_ =
            (pending_response_tail_ + 1) % MAX_PENDING_RESPONSES;
        ++pending_response_count_;
        return true;
    }

    void process_tx() {
        if (tx_kind_ != TxKind::None) {
            return;
        }

        // 1. Odpowiedzi na komendy mają najwyższy priorytet
        if (pending_response_count_ > 0) {
            PendingResponseSlot& slot = pending_response_queue_[pending_response_head_];
            if (slot.late_time_sync) {
                slot.time_sync_payload.t3_node_tx_us = time_us_64();
                slot.frame_len = encoder_.encode_response_frame_to(
                    slot.destination,
                    slot.source,
                    slot.sequence,
                    0,
                    reinterpret_cast<const uint8_t*>(
                        &slot.time_sync_payload
                    ),
                    sizeof(slot.time_sync_payload),
                    slot.frame,
                    sizeof(slot.frame)
                );
                if (slot.frame_len == 0) {
                    slot.late_time_sync = false;
                    pending_response_head_ =
                        (pending_response_head_ + 1) % MAX_PENDING_RESPONSES;
                    --pending_response_count_;
                    return;
                }
            }
            if (port_.start_tx_dma(slot.frame, slot.frame_len)) {
                tx_kind_ = TxKind::Response;
                active_response_action_ = slot.deferred_action;
                active_response_baudrate_ = slot.deferred_baudrate;
                slot.frame_len = 0;
                slot.deferred_action = DeferredActionT::None;
                slot.deferred_baudrate = 0;
                slot.late_time_sync = false;
                slot.destination = 0;
                slot.source = 0;
                slot.sequence = 0;
                slot.time_sync_payload = {};
                pending_response_head_ =
                    (pending_response_head_ + 1) % MAX_PENDING_RESPONSES;
                --pending_response_count_;
            }
            return;
        }

        // 2. Dopiero potem burst data
        if (!data_plane_.burst_active()) {
            return;
        }

        uint8_t payload[FRAME_MAX_PAYLOAD_SIZE]{};
        size_t payload_len = 0;

        if (!data_plane_.try_build_current_packet_payload(
                payload,
                sizeof(payload),
                payload_len)) {
            return;
        }

        const size_t frame_len = encoder_.encode_frame_to(
            FrameType::Data,
            data_plane_.burst_destination(),
            controller_.node_id(),
            tx_seq_++,
            0,
            payload,
            static_cast<uint16_t>(payload_len),
            tx_frame_buffer_,
            sizeof(tx_frame_buffer_)
        );

        if (frame_len == 0) {
            return;
        }

        if (port_.start_tx_dma(tx_frame_buffer_, frame_len)) {
            tx_kind_ = TxKind::BurstData;
        }
    }

    void align_rx_buffer_to_magic() {
        while (rx_size_ >= 2) {
            if (rx_buffer_[0] == kMagicLo && rx_buffer_[1] == kMagicHi) {
                return;
            }
            discard_rx_prefix(1);
        }
    }

    void discard_rx_prefix(size_t count) {
        if (count == 0) {
            return;
        }

        if (count >= rx_size_) {
            rx_size_ = 0;
            return;
        }

        std::memmove(rx_buffer_, rx_buffer_ + count, rx_size_ - count);
        rx_size_ -= count;
    }

private:
    Rs485Port& port_;
    ControllerT& controller_;
    IDataPlane& data_plane_;
    volatile uint8_t* requested_action_ = nullptr;

    FrameDecoder decoder_{};
    FrameEncoder encoder_{};

    uint8_t rx_buffer_[RX_BUFFER_SIZE]{};
    size_t rx_size_ = 0;

    uint8_t tx_frame_buffer_[TX_FRAME_BUFFER_SIZE]{};
    uint32_t tx_seq_ = 1;
    TxKind tx_kind_ = TxKind::None;

    struct PendingResponseSlot {
        uint8_t frame[TX_FRAME_BUFFER_SIZE]{};
        size_t frame_len = 0;
        DeferredActionT deferred_action = DeferredActionT::None;
        uint32_t deferred_baudrate = 0;
        bool late_time_sync = false;
        uint8_t destination = 0;
        uint8_t source = 0;
        uint32_t sequence = 0;
        TimeSyncResponsePayload time_sync_payload{};
    };

    PendingResponseSlot pending_response_queue_[MAX_PENDING_RESPONSES]{};
    size_t pending_response_head_ = 0;
    size_t pending_response_tail_ = 0;
    size_t pending_response_count_ = 0;
    DeferredActionT active_response_action_ = DeferredActionT::None;
    uint32_t active_response_baudrate_ = 0;
    uint64_t boot_epoch_ = 0;
    bool time_sync_enabled_ = false;
};
