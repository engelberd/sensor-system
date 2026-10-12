#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <fstream>
#include <string>
#include <vector>

#include "protocol_v3/time_service.h"

namespace {
using namespace ProtocolV3;

std::vector<uint8_t> read_vector(const char* name) {
    const std::string path = std::string("../host_v2/contracts/golden/") + name;
    std::ifstream input(path, std::ios::binary);
    assert(input.good());
    return std::vector<uint8_t>(std::istreambuf_iterator<char>(input), {});
}

void test_success_matches_shared_golden_vector() {
    const TimeExchangeResponse response{
        0x0102030405060708ull,
        0x1112131415161718ull,
        0x2122232425262728ull,
    };
    uint8_t encoded[kObserveClockResponseBytes]{};
    assert(encode_time_success(response, encoded, sizeof(encoded)) ==
           TimeCodecStatus::Ok);
    const auto golden = read_vector("time_v5_observe_response.bin");
    assert(golden.size() == sizeof(encoded));
    assert(std::memcmp(golden.data(), encoded, sizeof(encoded)) == 0);
}

void test_request_and_timestamp_invariants() {
    assert(validate_time_request(0) == TimeCodecStatus::Ok);
    assert(validate_time_request(1) == TimeCodecStatus::InvalidRequestLength);

    uint8_t encoded[kObserveClockResponseBytes]{};
    assert(encode_time_success({0, 10, 11}, encoded, sizeof(encoded)) ==
           TimeCodecStatus::ZeroBootEpoch);
    assert(encode_time_success({1, 11, 10}, encoded, sizeof(encoded)) ==
           TimeCodecStatus::NodeClockRegressed);
    assert(encode_time_success({1, 10, 11}, encoded, sizeof(encoded) - 1) ==
           TimeCodecStatus::OutputTooSmall);
    assert(encode_time_success({1, 10, 10}, encoded, sizeof(encoded)) ==
           TimeCodecStatus::Ok);
    assert(stamp_time_transmit(encoded, sizeof(encoded), 123) ==
           TimeCodecStatus::Ok);
    const uint8_t expected_t3[] = {123, 0, 0, 0, 0, 0, 0, 0};
    assert(std::memcmp(encoded + 17, expected_t3, sizeof(expected_t3)) == 0);
    assert(stamp_time_transmit(encoded, sizeof(encoded), 9) ==
           TimeCodecStatus::NodeClockRegressed);
}

}  // namespace

int main() {
    test_success_matches_shared_golden_vector();
    test_request_and_timestamp_invariants();
    return 0;
}
