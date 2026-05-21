#pragma once

#include <cstddef>
#include <cstdint>

#include "transport/frame_types.h"

enum class FrameDecodeStatus : uint8_t {
    Ok = 0,
    BufferTooSmall,
    BadMagic,
    BadVersion,
    BadType,
    LengthOutOfRange,
    SizeMismatch,
    BadCrc
};

struct DecodedFrame {
    FrameType type = FrameType::Error;
    uint8_t flags = 0;
    uint8_t destination = 0;
    uint8_t source = 0;
    uint16_t payload_length = 0;
    uint32_t sequence = 0;
    const uint8_t* payload = nullptr;
    size_t total_size = 0;
};

class FrameDecoder {
public:
    FrameDecodeStatus decode(const uint8_t* buffer,
                             size_t size,
                             DecodedFrame& out_frame) const;
};