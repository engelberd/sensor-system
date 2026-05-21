#include "transport/frame_decoder.h"

#include <cstring>

#include "transport/crc16.h"
#include "transport/frame_layout.h"

namespace {
bool is_valid_frame_type(uint8_t raw_type) {
    switch (static_cast<FrameType>(raw_type)) {
        case FrameType::Data:
        case FrameType::Command:
        case FrameType::Response:
        case FrameType::Event:
        case FrameType::Error:
            return true;
        default:
            return false;
    }
}
} // namespace

FrameDecodeStatus FrameDecoder::decode(const uint8_t* buffer,
                                       size_t size,
                                       DecodedFrame& out_frame) const {
    out_frame = {};

    if (buffer == nullptr || size < FRAME_MIN_SIZE) {
        return FrameDecodeStatus::BufferTooSmall;
    }

    FrameHeader header{};
    std::memcpy(&header, buffer, sizeof(header));

    if (header.magic != FRAME_MAGIC) {
        return FrameDecodeStatus::BadMagic;
    }

    if (header.version != FRAME_PROTOCOL_VERSION) {
        return FrameDecodeStatus::BadVersion;
    }

    if (!is_valid_frame_type(header.type)) {
        return FrameDecodeStatus::BadType;
    }

    if (header.length > FRAME_MAX_PAYLOAD_SIZE) {
        return FrameDecodeStatus::LengthOutOfRange;
    }

    const size_t expected_size =
        FRAME_HEADER_SIZE + static_cast<size_t>(header.length) + FRAME_CRC_SIZE;

    if (size != expected_size) {
        return FrameDecodeStatus::SizeMismatch;
    }

    uint16_t received_crc = 0;
    std::memcpy(
        &received_crc,
        buffer + FRAME_HEADER_SIZE + header.length,
        sizeof(received_crc)
    );

    const uint16_t calculated_crc =
        crc16_ccitt(buffer, FRAME_HEADER_SIZE + header.length);

    if (received_crc != calculated_crc) {
        return FrameDecodeStatus::BadCrc;
    }

    out_frame.type = static_cast<FrameType>(header.type);
    out_frame.flags = header.flags;
    out_frame.destination = header.destination;
    out_frame.source = header.source;
    out_frame.payload_length = header.length;
    out_frame.sequence = header.sequence;
    out_frame.payload = buffer + FRAME_HEADER_SIZE;
    out_frame.total_size = expected_size;

    return FrameDecodeStatus::Ok;
}