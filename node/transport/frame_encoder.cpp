#include "transport/frame_encoder.h"

#include <cstring>

#include "transport/crc16.h"
#include "transport/frame_layout.h"

size_t FrameEncoder::encode_frame_to(FrameType type,
                                     uint8_t destination,
                                     uint8_t source,
                                     uint32_t sequence,
                                     uint8_t flags,
                                     const uint8_t* payload,
                                     uint16_t payload_size,
                                     uint8_t* out_buffer,
                                     size_t max_size) {
    if (out_buffer == nullptr) {
        return 0;
    }

    if (payload_size > FRAME_MAX_PAYLOAD_SIZE) {
        return 0;
    }

    const size_t total_size =
        FRAME_HEADER_SIZE + static_cast<size_t>(payload_size) + FRAME_CRC_SIZE;

    if (total_size > max_size) {
        return 0;
    }

    FrameHeader header{};
    header.magic = FRAME_MAGIC;
    header.version = FRAME_PROTOCOL_VERSION;
    header.type = static_cast<uint8_t>(type);
    header.flags = flags;
    header.destination = destination;
    header.source = source;
    header.length = payload_size;
    header.sequence = sequence;

    size_t offset = 0;
    std::memcpy(out_buffer + offset, &header, sizeof(header));
    offset += sizeof(header);

    if (payload_size > 0 && payload != nullptr) {
        std::memcpy(out_buffer + offset, payload, payload_size);
    }
    offset += payload_size;

    const uint16_t crc = crc16_ccitt(out_buffer, offset);
    std::memcpy(out_buffer + offset, &crc, sizeof(crc));
    offset += sizeof(crc);

    return offset;
}

size_t FrameEncoder::encode_response_frame_to(uint8_t destination,
                                              uint8_t source,
                                              uint32_t sequence,
                                              uint8_t flags,
                                              const uint8_t* payload,
                                              uint16_t payload_size,
                                              uint8_t* out_buffer,
                                              size_t max_size) {
    return encode_frame_to(
        FrameType::Response,
        destination,
        source,
        sequence,
        flags,
        payload,
        payload_size,
        out_buffer,
        max_size
    );
}