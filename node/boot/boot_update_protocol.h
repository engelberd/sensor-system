#pragma once

#include <cstddef>
#include <cstdint>

namespace boot {

// ============================================================
// Packet constants
// ============================================================

static constexpr uint32_t UPDATE_PACKET_MAGIC = 0x55505444u; // 'UPTD'
static constexpr uint16_t UPDATE_PROTOCOL_VERSION = 2u;
static constexpr uint8_t UPDATE_BROADCAST_NODE_ID = 0xFFu;

static constexpr size_t UPDATE_MAX_PAYLOAD_SIZE = 1024u + 32u;
static constexpr size_t UPDATE_HEADER_SIZE = 12u;
static constexpr size_t UPDATE_CRC_SIZE = 4u;
static constexpr size_t UPDATE_MIN_PACKET_SIZE = UPDATE_HEADER_SIZE + UPDATE_CRC_SIZE;

// ============================================================
// Packet types
// ============================================================

enum class UpdatePacketType : uint8_t {
    Hello = 1,
    Begin = 2,
    Chunk = 3,
    End   = 4,
    Abort = 5,

    Ack   = 100,
    Error = 101
};

// ============================================================
// Status
// ============================================================

enum class UpdateStatus : uint8_t {
    Ok = 0,
    BadPacket = 1,
    BadState = 2,
    BadOffset = 3,
    BadLength = 4,
    BadCrc = 5,
    FlashError = 6,
    ImageTooLarge = 7,
    InvalidSlot = 8,
    InternalError = 9,
    Timeout = 10
};

// ============================================================
// Packet header
// ============================================================

#pragma pack(push, 1)
struct UpdatePacketHeader {
    uint32_t magic;
    uint8_t type;
    uint8_t destination;
    uint16_t payload_length;
    uint32_t sequence;
};
#pragma pack(pop)

// ============================================================
// Payloads
// ============================================================

#pragma pack(push, 1)

struct UpdateHelloPayload {
    uint16_t protocol_version;
    uint16_t max_chunk_size;
};

struct UpdateBeginPayload {
    uint32_t image_size;
    uint32_t image_crc32;
    uint32_t image_version;
};

struct UpdateChunkHeaderPayload {
    uint32_t offset;

    // liczba bajtów przesyłanych i zapisywanych do flash
    // musi być wielokrotnością 256
    uint16_t flash_length;

    // liczba rzeczywistych bajtów obrazu w tym chunku
    // valid_length <= flash_length
    uint16_t valid_length;
    // potem idzie data[flash_length]
};

struct UpdateEndPayload {
    uint32_t expected_image_crc32;
};

struct UpdateAckPayload {
    uint8_t status;     // UpdateStatus
    uint8_t reserved0;
    uint16_t reserved1;
    uint32_t value;
};

#pragma pack(pop)

// ============================================================
// Helpers
// ============================================================

uint32_t update_packet_crc32(const uint8_t* data, size_t length);

bool update_packet_type_is_valid(uint8_t raw_type);

} // namespace boot
