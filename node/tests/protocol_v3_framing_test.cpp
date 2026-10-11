#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <fstream>
#include <vector>

#include "protocol_v3/framing.h"

namespace {
using namespace ProtocolV3;

Frame request(const uint8_t* payload, size_t size) {
    Frame frame{};
    frame.role = Role::Request;
    frame.destination = 7;
    frame.source = 0;
    frame.service = 2;
    frame.message = 3;
    frame.transaction_id = 0x12345678u;
    frame.payload_size = size;
    if (size != 0) std::memcpy(frame.payload, payload, size);
    return frame;
}

void assert_same(const Frame& a, const Frame& b) {
    assert(a.role == b.role && a.destination == b.destination);
    assert(a.source == b.source && a.service == b.service && a.message == b.message);
    assert(a.transaction_id == b.transaction_id && a.payload_size == b.payload_size);
    assert(std::memcmp(a.payload, b.payload, a.payload_size) == 0);
}

Status decode(const uint8_t* wire, size_t size, Frame& frame) {
    StreamDecoder decoder{};
    Status status = Status::NoFrame;
    for (size_t i = 0; i < size; ++i) status = decoder.push(wire[i], frame);
    return status;
}

void test_crc_and_maximum_round_trip() {
    const uint8_t check[] = {'1','2','3','4','5','6','7','8','9'};
    assert(crc32c(check, sizeof(check)) == 0xe3069283u);
    uint8_t payload[kMaxPayloadBytes]{};
    for (size_t i = 0; i < sizeof(payload); ++i) payload[i] = static_cast<uint8_t>(i);
    const Frame expected = request(payload, sizeof(payload));
    uint8_t wire[kMaxWireFrameBytes]{};
    size_t wire_size = 0;
    assert(encode(expected, wire, sizeof(wire), wire_size) == Status::Ok);
    Frame actual{};
    assert(decode(wire, wire_size, actual) == Status::Ok);
    assert_same(actual, expected);
}

void test_corruption_then_recovery() {
    const uint8_t payload[] = {0, 1, 2, 0, 4};
    const Frame expected = request(payload, sizeof(payload));
    uint8_t wire[kMaxWireFrameBytes]{};
    size_t size = 0;
    assert(encode(expected, wire, sizeof(wire), size) == Status::Ok);
    wire[5] ^= 0x40u;
    Frame actual{};
    assert(decode(wire, size, actual) == Status::CrcMismatch);
    wire[5] ^= 0x40u;
    assert(decode(wire, size, actual) == Status::Ok);
    assert_same(actual, expected);
}

void test_oversize_and_routes() {
    StreamDecoder decoder{};
    Frame output{};
    for (size_t i = 0; i <= kMaxCobsBodyBytes; ++i) {
        assert(decoder.push(1, output) == Status::NoFrame);
    }
    assert(decoder.push(0, output) == Status::EncodedFrameTooLarge);

    Frame invalid = request(nullptr, 0);
    invalid.destination = kHostAddress;
    uint8_t wire[kMaxWireFrameBytes]{};
    size_t size = 0;
    assert(encode(invalid, wire, sizeof(wire), size) == Status::InvalidRoute);
    invalid = {};
    invalid.role = Role::Response;
    invalid.destination = kHostAddress;
    invalid.source = kBroadcastAddress;
    assert(encode(invalid, wire, sizeof(wire), size) == Status::InvalidRoute);
}

std::vector<uint8_t> read_vector(const char* name) {
    const std::string path = std::string("../host_v2/contracts/golden/") + name;
    std::ifstream input(path, std::ios::binary);
    assert(input.good());
    return std::vector<uint8_t>(std::istreambuf_iterator<char>(input), {});
}

void assert_golden(const Frame& frame, const char* name) {
    uint8_t wire[kMaxWireFrameBytes]{};
    size_t size = 0;
    assert(encode(frame, wire, sizeof(wire), size) == Status::Ok);
    const std::vector<uint8_t> expected = read_vector(name);
    assert(expected.size() == size);
    assert(std::memcmp(expected.data(), wire, size) == 0);
}

void test_shared_golden_vectors() {
    assert_golden(request(nullptr, 0), "framing_v3_request_empty.bin");

    const uint8_t mixed[] = {0, 1, 0, 255, 16};
    Frame response{};
    response.role = Role::Response;
    response.destination = 0;
    response.source = 7;
    response.service = 2;
    response.message = 4;
    response.transaction_id = 0x89abcdefu;
    response.payload_size = sizeof(mixed);
    std::memcpy(response.payload, mixed, sizeof(mixed));
    assert_golden(response, "framing_v3_response_mixed.bin");

    uint8_t maximum[kMaxPayloadBytes]{};
    for (size_t i = 0; i < sizeof(maximum); ++i) maximum[i] = static_cast<uint8_t>(i);
    assert_golden(request(maximum, sizeof(maximum)), "framing_v3_request_max.bin");
}
}  // namespace

int main() {
    test_crc_and_maximum_round_trip();
    test_corruption_then_recovery();
    test_oversize_and_routes();
    test_shared_golden_vectors();
    return 0;
}
