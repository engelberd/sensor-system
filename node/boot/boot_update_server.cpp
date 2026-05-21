#include "boot/boot_update_server.h"

#include <cstring>

#include "boot/boot_flash.h"
#include "boot/boot_maintenance.h"
#include "boot/boot_metadata.h"
#include "boot/boot_update_protocol.h"

namespace boot {
namespace {

#pragma pack(push, 1)
struct TxAckPacket {
    UpdatePacketHeader header;
    UpdateAckPayload payload;
    uint32_t crc32;
};
#pragma pack(pop)

static_assert(sizeof(TxAckPacket) ==
              sizeof(UpdatePacketHeader) + sizeof(UpdateAckPayload) + sizeof(uint32_t),
              "TxAckPacket layout mismatch");

} // namespace

BootUpdateServer::BootUpdateServer(BootUpdateEngine& engine, uint8_t node_id)
    : engine_(engine),
      node_id_(node_id) {
}

bool BootUpdateServer::read_exact(uint8_t* dst, size_t length, uint32_t timeout_ms) {
    if (dst == nullptr) {
        return false;
    }

    for (size_t i = 0; i < length; ++i) {
        if (!boot_uart_read_byte_with_timeout(dst[i], timeout_ms)) {
            return false;
        }
    }

    return true;
}

BootUpdateServer::ReceiveResult BootUpdateServer::receive_packet(UpdatePacketType& type,
                                                                 uint32_t& sequence,
                                                                 const uint8_t*& payload,
                                                                 uint16_t& payload_length) {
    payload = nullptr;
    payload_length = 0;
    sequence = 0;
    type = UpdatePacketType::Error;

    uint8_t window[4]{};

    // resync po magic
    while (true) {
        uint8_t b = 0;
        if (!boot_uart_read_byte_with_timeout(b, PACKET_TIMEOUT_MS)) {
            return ReceiveResult::Timeout;
        }

        window[0] = window[1];
        window[1] = window[2];
        window[2] = window[3];
        window[3] = b;

        uint32_t magic = 0;
        std::memcpy(&magic, window, sizeof(magic));

        if (magic == UPDATE_PACKET_MAGIC) {
            break;
        }
    }

    std::memcpy(rx_buffer_, window, 4);

    // doczytaj resztę headera
    if (!read_exact(rx_buffer_ + 4, UPDATE_HEADER_SIZE - 4, PACKET_TIMEOUT_MS)) {
        return ReceiveResult::Timeout;
    }

    UpdatePacketHeader header{};
    std::memcpy(&header, rx_buffer_, sizeof(header));

    if (!update_packet_type_is_valid(header.type)) {
        return ReceiveResult::BadPacket;
    }

    if (header.payload_length > UPDATE_MAX_PAYLOAD_SIZE) {
        return ReceiveResult::BadPacket;
    }

    const size_t tail_size = static_cast<size_t>(header.payload_length) + UPDATE_CRC_SIZE;
    if (!read_exact(rx_buffer_ + UPDATE_HEADER_SIZE, tail_size, PACKET_TIMEOUT_MS)) {
        return ReceiveResult::Timeout;
    }

    uint32_t received_crc = 0;
    std::memcpy(
        &received_crc,
        rx_buffer_ + UPDATE_HEADER_SIZE + header.payload_length,
        sizeof(received_crc)
    );

    const uint32_t computed_crc =
        update_packet_crc32(rx_buffer_, UPDATE_HEADER_SIZE + header.payload_length);

    if (received_crc != computed_crc) {
        return ReceiveResult::BadPacket;
    }

    if (header.destination != node_id_) {
        return ReceiveResult::Ignored;
    }

    type = static_cast<UpdatePacketType>(header.type);
    sequence = header.sequence;
    payload_length = header.payload_length;
    payload = rx_buffer_ + UPDATE_HEADER_SIZE;

    return ReceiveResult::Ok;
}

bool BootUpdateServer::send_ack(uint32_t sequence, UpdateStatus status, uint32_t value) {
    TxAckPacket pkt{};

    pkt.header.magic = UPDATE_PACKET_MAGIC;
    pkt.header.type = static_cast<uint8_t>(UpdatePacketType::Ack);
    pkt.header.destination = node_id_;
    pkt.header.payload_length = static_cast<uint16_t>(sizeof(UpdateAckPayload));
    pkt.header.sequence = sequence;

    pkt.payload.status = static_cast<uint8_t>(status);
    pkt.payload.reserved0 = 0u;
    pkt.payload.reserved1 = 0u;
    pkt.payload.value = value;

    pkt.crc32 = 0u;
    pkt.crc32 = update_packet_crc32(
        reinterpret_cast<const uint8_t*>(&pkt),
        sizeof(pkt) - sizeof(pkt.crc32)
    );

    boot_uart_write_bytes(reinterpret_cast<const uint8_t*>(&pkt), sizeof(pkt));
    return true;
}

bool BootUpdateServer::send_error(uint32_t sequence, UpdateStatus status, uint32_t value) {
    TxAckPacket pkt{};

    pkt.header.magic = UPDATE_PACKET_MAGIC;
    pkt.header.type = static_cast<uint8_t>(UpdatePacketType::Error);
    pkt.header.destination = node_id_;
    pkt.header.payload_length = static_cast<uint16_t>(sizeof(UpdateAckPayload));
    pkt.header.sequence = sequence;

    pkt.payload.status = static_cast<uint8_t>(status);
    pkt.payload.reserved0 = 0u;
    pkt.payload.reserved1 = 0u;
    pkt.payload.value = value;

    pkt.crc32 = 0u;
    pkt.crc32 = update_packet_crc32(
        reinterpret_cast<const uint8_t*>(&pkt),
        sizeof(pkt) - sizeof(pkt.crc32)
    );

    boot_uart_write_bytes(reinterpret_cast<const uint8_t*>(&pkt), sizeof(pkt));
    return true;
}

bool BootUpdateServer::run(BootMetadata& metadata) {
    engine_.reset();
    boot_console_puts("BOOT> update mode entered\r\n");

    while (true) {
        UpdatePacketType type{};
        uint32_t sequence = 0;
        const uint8_t* payload = nullptr;
        uint16_t payload_length = 0;

        const ReceiveResult receive_result =
            receive_packet(type, sequence, payload, payload_length);
        if (receive_result == ReceiveResult::Timeout) {
            send_error(0u, UpdateStatus::Timeout, static_cast<uint32_t>(engine_.state()));
            return false;
        }

        if (receive_result == ReceiveResult::Ignored) {
            continue;
        }

        if (receive_result != ReceiveResult::Ok) {
            send_error(0u, UpdateStatus::BadPacket, 0u);
            continue;
        }

        switch (type) {
            case UpdatePacketType::Hello: {
                UpdateHelloPayload hello{};
                hello.protocol_version = UPDATE_PROTOCOL_VERSION;
                hello.max_chunk_size = 1024u;
                (void)hello;
                const SlotId target_slot = engine_.preview_target_slot(metadata);

                // value = node_id:8, protocol_version:8, max_chunk:16
                const uint32_t packed =
                    (static_cast<uint32_t>(node_id_) << 24) |
                    (static_cast<uint32_t>(UPDATE_PROTOCOL_VERSION) << 16) |
                    static_cast<uint32_t>(1024u);

                TxAckPacket pkt{};
                pkt.header.magic = UPDATE_PACKET_MAGIC;
                pkt.header.type = static_cast<uint8_t>(UpdatePacketType::Ack);
                pkt.header.destination = node_id_;
                pkt.header.payload_length = static_cast<uint16_t>(sizeof(UpdateAckPayload));
                pkt.header.sequence = sequence;
                pkt.payload.status = static_cast<uint8_t>(UpdateStatus::Ok);
                pkt.payload.reserved0 = static_cast<uint8_t>(target_slot);
                pkt.payload.reserved1 = 0u;
                pkt.payload.value = packed;
                pkt.crc32 = update_packet_crc32(
                    reinterpret_cast<const uint8_t*>(&pkt),
                    sizeof(pkt) - sizeof(pkt.crc32)
                );
                boot_uart_write_bytes(reinterpret_cast<const uint8_t*>(&pkt), sizeof(pkt));
                break;
            }

            case UpdatePacketType::Begin: {
                if (payload_length != sizeof(UpdateBeginPayload)) {
                    send_error(sequence, UpdateStatus::BadLength, payload_length);
                    break;
                }

                UpdateBeginPayload begin{};
                std::memcpy(&begin, payload, sizeof(begin));

                const SlotId target_slot = engine_.preview_target_slot(metadata);
                if (target_slot == SlotId::None) {
                    send_error(sequence, UpdateStatus::InvalidSlot, 0u);
                    break;
                }

                SlotMetadata& target_md = boot_slot_metadata(metadata, target_slot);
                target_md.image_valid = 0u;
                target_md.image_size = 0u;
                target_md.image_crc32 = 0u;
                target_md.image_version = begin.image_version;
                if (!boot_metadata_save(metadata)) {
                    send_error(sequence, UpdateStatus::FlashError, 0u);
                    break;
                }

                const UpdateStatus st =
                    engine_.begin(metadata,
                                  begin.image_size,
                                  begin.image_crc32,
                                  begin.image_version);

                if (st == UpdateStatus::Ok) {
                    send_ack(sequence, st, static_cast<uint32_t>(engine_.target_slot()));
                } else {
                    send_error(sequence, st, static_cast<uint32_t>(engine_.target_slot()));
                }
                break;
            }

            case UpdatePacketType::Chunk: {
                if (payload_length < sizeof(UpdateChunkHeaderPayload)) {
                    send_error(sequence, UpdateStatus::BadLength, payload_length);
                    break;
                }

                UpdateChunkHeaderPayload ch{};
                std::memcpy(&ch, payload, sizeof(ch));

                const size_t expected_payload =
                    sizeof(UpdateChunkHeaderPayload) + static_cast<size_t>(ch.flash_length);

                if (payload_length != expected_payload) {
                    send_error(sequence, UpdateStatus::BadLength, payload_length);
                    break;
                }

                const uint8_t* data = payload + sizeof(UpdateChunkHeaderPayload);

                const UpdateStatus st =
                    engine_.write_chunk(ch.offset, data, ch.flash_length, ch.valid_length);

                if (st == UpdateStatus::Ok) {
                    send_ack(sequence, st, ch.offset + ch.valid_length);
                } else {
                    send_error(sequence, st, ch.offset);
                }
                break;
            }

            case UpdatePacketType::End: {
                if (payload_length != sizeof(UpdateEndPayload)) {
                    send_error(sequence, UpdateStatus::BadLength, payload_length);
                    break;
                }

                UpdateEndPayload endp{};
                std::memcpy(&endp, payload, sizeof(endp));

                const UpdateStatus st = engine_.finish(metadata, endp.expected_image_crc32);
                if (st != UpdateStatus::Ok) {
                    send_error(sequence, st, 0u);
                    break;
                }

                if (!boot_metadata_save(metadata)) {
                    send_error(sequence, UpdateStatus::FlashError, 0u);
                    break;
                }

                send_ack(sequence, UpdateStatus::Ok, static_cast<uint32_t>(engine_.target_slot()));
                boot_console_puts("BOOT> update completed\r\n");
                return true;
            }

            case UpdatePacketType::Abort: {
                engine_.abort();
                send_ack(sequence, UpdateStatus::Ok, 0u);
                boot_console_puts("BOOT> update aborted\r\n");
                return false;
            }

            case UpdatePacketType::Ack:
            case UpdatePacketType::Error:
            default:
                send_error(sequence, UpdateStatus::BadPacket, static_cast<uint32_t>(type));
                break;
        }
    }
}

} // namespace boot
