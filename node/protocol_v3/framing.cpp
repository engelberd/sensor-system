#include "protocol_v3/framing.h"

#include <cstring>

namespace ProtocolV3 {
namespace {

bool valid_route(Role role, uint8_t destination, uint8_t source) {
    if (role == Role::Request) {
        return source == kHostAddress && destination != kHostAddress;
    }
    return role == Role::Response && destination == kHostAddress &&
           source != kHostAddress && source != kBroadcastAddress;
}

void write_u32_le(uint32_t value, uint8_t* output) {
    output[0] = static_cast<uint8_t>(value);
    output[1] = static_cast<uint8_t>(value >> 8u);
    output[2] = static_cast<uint8_t>(value >> 16u);
    output[3] = static_cast<uint8_t>(value >> 24u);
}

uint32_t read_u32_le(const uint8_t* input) {
    return static_cast<uint32_t>(input[0]) |
           (static_cast<uint32_t>(input[1]) << 8u) |
           (static_cast<uint32_t>(input[2]) << 16u) |
           (static_cast<uint32_t>(input[3]) << 24u);
}

size_t cobs_encode(const uint8_t* input, size_t input_size, uint8_t* output) {
    size_t code_index = 0;
    size_t write_index = 1;
    uint8_t code = 1;
    for (size_t i = 0; i < input_size; ++i) {
        if (input[i] == 0) {
            output[code_index] = code;
            code_index = write_index++;
            code = 1;
        } else {
            output[write_index++] = input[i];
            ++code;
            if (code == 0xffu) {
                output[code_index] = code;
                code_index = write_index++;
                code = 1;
            }
        }
    }
    output[code_index] = code;
    return write_index;
}

Status cobs_decode(const uint8_t* input, size_t input_size,
                   uint8_t* output, size_t& output_size) {
    size_t read_index = 0;
    size_t write_index = 0;
    while (read_index < input_size) {
        const uint8_t code = input[read_index++];
        if (code == 0) return Status::MalformedCobs;
        const size_t count = static_cast<size_t>(code) - 1u;
        if (read_index + count > input_size ||
            write_index + count > kMaxRawFrameBytes) {
            return Status::MalformedCobs;
        }
        std::memcpy(output + write_index, input + read_index, count);
        read_index += count;
        write_index += count;
        if (code != 0xffu && read_index < input_size) {
            if (write_index == kMaxRawFrameBytes) return Status::MalformedCobs;
            output[write_index++] = 0;
        }
    }
    output_size = write_index;
    return Status::Ok;
}

}  // namespace

uint32_t crc32c(const uint8_t* data, size_t size) {
    uint32_t crc = 0xffffffffu;
    for (size_t i = 0; i < size; ++i) {
        crc ^= data[i];
        for (uint8_t bit = 0; bit < 8; ++bit) {
            const uint32_t mask = 0u - (crc & 1u);
            crc = (crc >> 1u) ^ (0x82f63b78u & mask);
        }
    }
    return ~crc;
}

Status validate(const Frame& frame) {
    if (frame.payload_size > kMaxPayloadBytes) return Status::PayloadTooLarge;
    return valid_route(frame.role, frame.destination, frame.source)
               ? Status::Ok : Status::InvalidRoute;
}

Status encode(const Frame& frame, uint8_t* output, size_t output_capacity,
              size_t& output_size) {
    output_size = 0;
    const Status status = validate(frame);
    if (status != Status::Ok) return status;
    if (output == nullptr || output_capacity < kMaxWireFrameBytes) {
        return Status::OutputTooSmall;
    }
    uint8_t raw[kMaxRawFrameBytes]{};
    raw[0] = kFramingVersion;
    raw[1] = static_cast<uint8_t>(frame.role);
    raw[2] = frame.destination;
    raw[3] = frame.source;
    raw[4] = frame.service;
    raw[5] = frame.message;
    write_u32_le(frame.transaction_id, raw + 6);
    std::memcpy(raw + kHeaderBytes, frame.payload, frame.payload_size);
    const size_t crc_offset = kHeaderBytes + frame.payload_size;
    write_u32_le(crc32c(raw, crc_offset), raw + crc_offset);
    output_size = cobs_encode(raw, crc_offset + kCrcBytes, output);
    output[output_size++] = 0;
    return Status::Ok;
}

Status StreamDecoder::push(uint8_t byte, Frame& output) {
    if (byte != 0) {
        if (discarding_) return Status::NoFrame;
        if (encoded_size_ == kMaxCobsBodyBytes) {
            encoded_size_ = 0;
            discarding_ = true;
            return Status::NoFrame;
        }
        encoded_[encoded_size_++] = byte;
        return Status::NoFrame;
    }
    if (discarding_) {
        discarding_ = false;
        return Status::EncodedFrameTooLarge;
    }
    if (encoded_size_ == 0) return Status::NoFrame;
    const Status status = decode_candidate(output);
    encoded_size_ = 0;
    return status;
}

void StreamDecoder::discard_partial() {
    encoded_size_ = 0;
    discarding_ = false;
}

Status StreamDecoder::decode_candidate(Frame& output) {
    uint8_t raw[kMaxRawFrameBytes]{};
    size_t raw_size = 0;
    Status status = cobs_decode(encoded_, encoded_size_, raw, raw_size);
    if (status != Status::Ok) return status;
    if (raw_size < kHeaderBytes + kCrcBytes) return Status::FrameTooShort;
    if (raw[0] != kFramingVersion) return Status::UnsupportedVersion;
    Role role{};
    if (raw[1] == static_cast<uint8_t>(Role::Request)) role = Role::Request;
    else if (raw[1] == static_cast<uint8_t>(Role::Response)) role = Role::Response;
    else return Status::InvalidRole;
    if (!valid_route(role, raw[2], raw[3])) return Status::InvalidRoute;
    const size_t crc_offset = raw_size - kCrcBytes;
    if (crc32c(raw, crc_offset) != read_u32_le(raw + crc_offset)) {
        return Status::CrcMismatch;
    }
    const size_t payload_size = crc_offset - kHeaderBytes;
    if (payload_size > kMaxPayloadBytes) return Status::PayloadTooLarge;
    output = {};
    output.role = role;
    output.destination = raw[2];
    output.source = raw[3];
    output.service = raw[4];
    output.message = raw[5];
    output.transaction_id = read_u32_le(raw + 6);
    output.payload_size = payload_size;
    std::memcpy(output.payload, raw + kHeaderBytes, payload_size);
    return Status::Ok;
}

}  // namespace ProtocolV3
