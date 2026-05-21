#pragma once

#include "boot/boot_metadata.h"
#include "boot/boot_update_engine.h"

namespace boot {

class BootUpdateServer {
public:
    explicit BootUpdateServer(BootUpdateEngine& engine, uint8_t node_id);

    // returns true if image was fully received, validated, metadata saved
    bool run(BootMetadata& metadata);

private:
    enum class ReceiveResult : uint8_t {
        Ok = 0,
        Ignored,
        Timeout,
        BadPacket
    };

    ReceiveResult receive_packet(UpdatePacketType& type,
                                 uint32_t& sequence,
                                 const uint8_t*& payload,
                                 uint16_t& payload_length);

    bool send_ack(uint32_t sequence, UpdateStatus status, uint32_t value);
    bool send_error(uint32_t sequence, UpdateStatus status, uint32_t value);

    bool read_exact(uint8_t* dst, size_t length, uint32_t timeout_ms);

    BootUpdateEngine& engine_;
    uint8_t node_id_ = 1;

    static constexpr uint32_t PACKET_TIMEOUT_MS = 10000u;
    static constexpr size_t RX_BUFFER_SIZE = UPDATE_HEADER_SIZE + UPDATE_MAX_PAYLOAD_SIZE + UPDATE_CRC_SIZE;

    uint8_t rx_buffer_[RX_BUFFER_SIZE]{};
};

} // namespace boot
