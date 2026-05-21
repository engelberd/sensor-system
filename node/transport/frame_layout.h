#pragma once

#include <cstdint>

#include "transport/frame_types.h"

#pragma pack(push, 1)
struct FrameHeader {
    uint16_t magic;
    uint8_t version;
    uint8_t type;
    uint8_t flags;
    uint8_t destination;
    uint8_t source;
    uint16_t length;
    uint32_t sequence;
};
#pragma pack(pop)

static_assert(sizeof(FrameHeader) == FRAME_HEADER_SIZE, "FrameHeader size mismatch");