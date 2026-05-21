#pragma once

#include <cstddef>
#include <cstdint>

static constexpr uint16_t FRAME_MAGIC = 0xAA55;
static constexpr uint8_t FRAME_PROTOCOL_VERSION = 2;
static constexpr size_t FRAME_MAX_PAYLOAD_SIZE = 1024;

static constexpr size_t FRAME_HEADER_SIZE = 13;
static constexpr size_t FRAME_CRC_SIZE = 2;
static constexpr size_t FRAME_MIN_SIZE = FRAME_HEADER_SIZE + FRAME_CRC_SIZE;

enum class FrameType : uint8_t {
    Data     = 0x01,
    Command  = 0x02,
    Response = 0x03,
    Event    = 0x04,
    Error    = 0x05
};

enum FrameFlags : uint8_t {
    FRAME_FLAG_NONE         = 0,
    FRAME_FLAG_ACK_REQUEST  = 1u << 0,
    FRAME_FLAG_ACK_RESPONSE = 1u << 1
};