#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <fstream>
#include <string>
#include <vector>

#include "protocol_v3/identity.h"

namespace {
using namespace ProtocolV3;

std::vector<uint8_t> read_vector(const char* name) {
    const std::string path = std::string("../host_v2/contracts/golden/") + name;
    std::ifstream input(path, std::ios::binary);
    assert(input.good());
    return std::vector<uint8_t>(std::istreambuf_iterator<char>(input), {});
}

NodeIdentity identity() {
    NodeIdentity value{};
    const uint8_t hardware_id[] = {0x2f, 0x13, 0x34, 0x94, 0x50, 0xfa, 0x40, 0x84};
    std::memcpy(value.hardware_id, hardware_id, sizeof(hardware_id));
    value.boot_epoch = 0x1122334455667788ull;
    value.node_address = 7;
    value.board_revision = 2;
    value.firmware = {0, 5, 0};
    return value;
}

NodeCapabilities capabilities() {
    NodeCapabilities value{};
    value.service_flags = service_capability_bit(ServiceId::Identity) |
                          service_capability_bit(ServiceId::Measurement) |
                          service_capability_bit(ServiceId::Time);
    value.feature_flags = kFeatureSampleJournal | kFeatureDrdyTimestamps;
    value.max_payload_bytes = 512;
    value.journal_capacity_samples = 4096;
    value.output_rate_mask = kImplementedOutputRateMask;
    value.max_sample_chunk_samples = 48;
    return value;
}

void test_identity_golden_round_trip() {
    uint8_t encoded[kIdentityResponseBytes]{};
    assert(encode_identity_success(identity(), encoded, sizeof(encoded)) ==
           IdentityCodecStatus::Ok);
    const auto golden = read_vector("identity_v5_response.bin");
    assert(golden.size() == sizeof(encoded));
    assert(std::memcmp(golden.data(), encoded, sizeof(encoded)) == 0);

    NodeIdentity decoded{};
    ResponseStatus remote = ResponseStatus::InternalError;
    assert(decode_identity(encoded, sizeof(encoded), decoded, remote) ==
           IdentityCodecStatus::Ok);
    assert(remote == ResponseStatus::Ok);
    assert(decoded.boot_epoch == identity().boot_epoch);
    assert(std::memcmp(decoded.hardware_id, identity().hardware_id, kHardwareIdBytes) == 0);
}

void test_capabilities_golden_round_trip() {
    uint8_t encoded[kCapabilitiesResponseBytes]{};
    assert(encode_capabilities_success(capabilities(), encoded, sizeof(encoded)) ==
           IdentityCodecStatus::Ok);
    const auto golden = read_vector("capabilities_v5_response.bin");
    assert(golden.size() == sizeof(encoded));
    assert(std::memcmp(golden.data(), encoded, sizeof(encoded)) == 0);

    NodeCapabilities decoded{};
    ResponseStatus remote = ResponseStatus::InternalError;
    assert(decode_capabilities(encoded, sizeof(encoded), decoded, remote) ==
           IdentityCodecStatus::Ok);
    assert(decoded.journal_capacity_samples == 4096);
    assert(decoded.supports_service(ServiceId::Measurement));
    assert(decoded.output_rate_mask == kImplementedOutputRateMask);
    assert((decoded.output_rate_mask & (1u << 0u)) == 0u);  // no 4000 Hz output
    assert((decoded.output_rate_mask & (1u << 1u)) != 0u);  // 2000 Hz output
    assert((decoded.output_rate_mask & (1u << 6u)) != 0u);  // 62.5 Hz output
    assert((decoded.output_rate_mask & (1u << 7u)) == 0u);  // no 31.25 Hz output
}

void test_fail_closed_validation() {
    NodeIdentity invalid = identity();
    invalid.boot_epoch = 0;
    uint8_t output[kIdentityResponseBytes]{};
    assert(encode_identity_success(invalid, output, sizeof(output)) ==
           IdentityCodecStatus::ZeroBootEpoch);

    NodeCapabilities incomplete = capabilities();
    incomplete.journal_capacity_samples = 0;
    assert(validate(incomplete) ==
           IdentityCodecStatus::IncompleteMeasurementCapabilities);

    NodeCapabilities unknown_rate = capabilities();
    unknown_rate.output_rate_mask = static_cast<uint16_t>(1u << 15u);
    assert(validate(unknown_rate) == IdentityCodecStatus::UnknownOutputRateBits);

    NodeCapabilities old_schema = capabilities();
    old_schema.schema_version = 1;
    assert(validate(old_schema) ==
           IdentityCodecStatus::UnsupportedCapabilitiesSchema);

    NodeCapabilities oversized_chunk = capabilities();
    oversized_chunk.max_sample_chunk_samples =
        static_cast<uint16_t>(kMaxWireChunkSamples + 1u);
    assert(validate(oversized_chunk) ==
           IdentityCodecStatus::InvalidMaxSampleChunk);

    NodeCapabilities inconsistent_features = capabilities();
    inconsistent_features.feature_flags |= kFeatureTrialBootConfirmation;
    assert(validate(inconsistent_features) ==
           IdentityCodecStatus::InvalidFeatureCombination);

    NodeCapabilities missing_identity = capabilities();
    missing_identity.service_flags &=
        ~service_capability_bit(ServiceId::Identity);
    assert(validate(missing_identity) ==
           IdentityCodecStatus::MissingIdentityService);

    NodeIdentity decoded{};
    ResponseStatus remote = ResponseStatus::Ok;
    const uint8_t busy[] = {static_cast<uint8_t>(ResponseStatus::Busy)};
    assert(decode_identity(busy, sizeof(busy), decoded, remote) ==
           IdentityCodecStatus::RemoteError);
    assert(remote == ResponseStatus::Busy);
}
}  // namespace

int main() {
    test_identity_golden_round_trip();
    test_capabilities_golden_round_trip();
    test_fail_closed_validation();
    return 0;
}
