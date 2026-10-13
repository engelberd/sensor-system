#pragma once

#include <cstddef>
#include <cstdint>

namespace ProtocolV3 {

constexpr uint8_t kFramingVersion = 3;
constexpr uint8_t kHostAddress = 0;
constexpr uint8_t kCommissioningAddress = 254;
constexpr uint8_t kBroadcastAddress = 255;
constexpr size_t kHeaderBytes = 10;
constexpr size_t kCrcBytes = 4;
constexpr size_t kMaxPayloadBytes = 512;
constexpr size_t kMaxRawFrameBytes = kHeaderBytes + kMaxPayloadBytes + kCrcBytes;
constexpr size_t kMaxCobsBodyBytes =
    kMaxRawFrameBytes + ((kMaxRawFrameBytes + 253u) / 254u);
constexpr size_t kMaxWireFrameBytes = kMaxCobsBodyBytes + 1u;

enum class Role : uint8_t { Request = 1, Response = 2 };

enum class Status : uint8_t {
    Ok = 0, NoFrame, PayloadTooLarge, OutputTooSmall,
    EncodedFrameTooLarge, MalformedCobs, FrameTooShort,
    UnsupportedVersion, InvalidRole, InvalidRoute, CrcMismatch,
};

struct Frame {
    Role role = Role::Request;
    uint8_t destination = 0;
    uint8_t source = 0;
    uint8_t service = 0;
    uint8_t message = 0;
    uint32_t transaction_id = 0;
    uint8_t payload[kMaxPayloadBytes]{};
    size_t payload_size = 0;
};

uint32_t crc32c(const uint8_t* data, size_t size);
Status validate(const Frame& frame);
Status encode(const Frame& frame, uint8_t* output, size_t output_capacity,
              size_t& output_size);

class StreamDecoder {
public:
    Status push(uint8_t byte, Frame& output);
    void discard_partial();

private:
    Status decode_candidate(Frame& output);
    uint8_t encoded_[kMaxCobsBodyBytes]{};
    size_t encoded_size_ = 0;
    bool discarding_ = false;
};

}  // namespace ProtocolV3
