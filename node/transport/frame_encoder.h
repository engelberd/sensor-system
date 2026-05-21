#pragma once

#include <cstddef>
#include <cstdint>

#include "transport/frame_types.h"

class FrameEncoder {
public:
    size_t encode_frame_to(FrameType type,
                        uint8_t destination,
                        uint8_t source,
                        uint32_t sequence,
                        uint8_t flags,
                        const uint8_t* payload,
                        uint16_t payload_size,
                        uint8_t* out_buffer,
                        size_t max_size);

    size_t encode_response_frame_to(uint8_t destination,
                                    uint8_t source,
                                    uint32_t sequence,
                                    uint8_t flags,
                                    const uint8_t* payload,
                                    uint16_t payload_size,
                                    uint8_t* out_buffer,
                                    size_t max_size);
};