#pragma once

#include <cstddef>
#include <cstdint>

#include "protocol_v3/application.h"
#include "protocol_v3/framing.h"
#include "protocol_v3/measurement.h"

namespace ProtocolV3 {

constexpr uint8_t kApplicationProtocolVersion = 5;
constexpr uint8_t kIdentityServiceId = static_cast<uint8_t>(ServiceId::Identity);
constexpr uint8_t kGetIdentityMessageId = 1;
constexpr uint8_t kGetCapabilitiesMessageId = 2;
constexpr size_t kHardwareIdBytes = 8;
constexpr size_t kIdentityResponseBytes = 24;
constexpr size_t kCapabilitiesResponseBytes = 24;
constexpr uint8_t kCapabilitiesSchemaVersion = 2;
constexpr uint16_t kKnownOutputRateMask = 0x07ffu;
// Current DeviceConfig accepts sensor ODR 4000..125 Hz and the acquisition
// pipeline always decimates by two, so Measurement Service can emit exactly
// 2000, 1000, 500, 250, 125 or 62.5 samples/s.
constexpr uint16_t kImplementedOutputRateMask = 0x007eu;
constexpr uint32_t kKnownServiceFlags = 0x000000ffu;

constexpr uint64_t kFeatureSampleJournal = 1ull << 0u;
constexpr uint64_t kFeatureDrdyTimestamps = 1ull << 1u;
constexpr uint64_t kFeatureAtomicConfig = 1ull << 2u;
constexpr uint64_t kFeatureTrialBootConfirmation = 1ull << 3u;
constexpr uint64_t kFeatureRemoteFirmwareUpdate = 1ull << 4u;
constexpr uint64_t kKnownFeatureFlags =
    kFeatureSampleJournal | kFeatureDrdyTimestamps | kFeatureAtomicConfig |
    kFeatureTrialBootConfirmation | kFeatureRemoteFirmwareUpdate;

struct FirmwareVersion {
    uint8_t major = 0;
    uint8_t minor = 0;
    uint8_t patch = 0;
};

struct NodeIdentity {
    uint8_t hardware_id[kHardwareIdBytes]{};
    uint64_t boot_epoch = 0;
    uint8_t node_address = 0;
    uint8_t board_revision = 0;
    FirmwareVersion firmware{};
    uint8_t framing_version = kFramingVersion;
    uint8_t application_version = kApplicationProtocolVersion;
};

struct NodeCapabilities {
    uint8_t schema_version = kCapabilitiesSchemaVersion;
    uint32_t service_flags = 0;
    uint64_t feature_flags = 0;
    uint16_t max_payload_bytes = 0;
    uint32_t journal_capacity_samples = 0;
    uint16_t output_rate_mask = 0;
    uint16_t max_sample_chunk_samples = 0;

    bool supports_service(ServiceId service) const {
        return (service_flags & service_capability_bit(service)) != 0;
    }
};

enum class IdentityCodecStatus : uint8_t {
    Ok = 0,
    OutputTooSmall,
    InvalidPayloadLength,
    UnknownResponseStatus,
    RemoteError,
    ZeroBootEpoch,
    InvalidNodeAddress,
    InvalidBoardRevision,
    ProtocolMismatch,
    UnsupportedCapabilitiesSchema,
    InvalidMaxPayload,
    UnknownServiceBits,
    MissingIdentityService,
    UnknownFeatureBits,
    InvalidFeatureCombination,
    UnknownOutputRateBits,
    IncompleteMeasurementCapabilities,
    InvalidMaxSampleChunk,
};

IdentityCodecStatus validate(const NodeIdentity& identity);
IdentityCodecStatus validate(const NodeCapabilities& capabilities);

IdentityCodecStatus encode_identity_success(
    const NodeIdentity& identity, uint8_t* output, size_t output_capacity);
IdentityCodecStatus decode_identity(
    const uint8_t* payload, size_t payload_size, NodeIdentity& output,
    ResponseStatus& remote_status);

IdentityCodecStatus encode_capabilities_success(
    const NodeCapabilities& capabilities, uint8_t* output, size_t output_capacity);
IdentityCodecStatus decode_capabilities(
    const uint8_t* payload, size_t payload_size, NodeCapabilities& output,
    ResponseStatus& remote_status);

}  // namespace ProtocolV3
