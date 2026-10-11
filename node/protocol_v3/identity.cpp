#include "protocol_v3/identity.h"

#include <cstring>

namespace ProtocolV3 {
namespace {

void write_u16_le(uint16_t value, uint8_t* output) {
    output[0] = static_cast<uint8_t>(value);
    output[1] = static_cast<uint8_t>(value >> 8u);
}

void write_u32_le(uint32_t value, uint8_t* output) {
    for (size_t i = 0; i < 4; ++i) {
        output[i] = static_cast<uint8_t>(value >> (8u * i));
    }
}

void write_u64_le(uint64_t value, uint8_t* output) {
    for (size_t i = 0; i < 8; ++i) {
        output[i] = static_cast<uint8_t>(value >> (8u * i));
    }
}

uint16_t read_u16_le(const uint8_t* input) {
    return static_cast<uint16_t>(input[0]) |
           static_cast<uint16_t>(static_cast<uint16_t>(input[1]) << 8u);
}

uint32_t read_u32_le(const uint8_t* input) {
    uint32_t value = 0;
    for (size_t i = 0; i < 4; ++i) {
        value |= static_cast<uint32_t>(input[i]) << (8u * i);
    }
    return value;
}

uint64_t read_u64_le(const uint8_t* input) {
    uint64_t value = 0;
    for (size_t i = 0; i < 8; ++i) {
        value |= static_cast<uint64_t>(input[i]) << (8u * i);
    }
    return value;
}

bool decode_status(uint8_t value, ResponseStatus& output) {
    if (value > static_cast<uint8_t>(ResponseStatus::InternalError)) return false;
    output = static_cast<ResponseStatus>(value);
    return true;
}

}  // namespace

IdentityCodecStatus validate(const NodeIdentity& identity) {
    if (identity.boot_epoch == 0) return IdentityCodecStatus::ZeroBootEpoch;
    if (identity.node_address == kHostAddress ||
        identity.node_address == kBroadcastAddress) {
        return IdentityCodecStatus::InvalidNodeAddress;
    }
    if (identity.board_revision == 0) {
        return IdentityCodecStatus::InvalidBoardRevision;
    }
    return IdentityCodecStatus::Ok;
}

IdentityCodecStatus validate(const NodeCapabilities& capabilities) {
    if (capabilities.schema_version != kCapabilitiesSchemaVersion) {
        return IdentityCodecStatus::UnsupportedCapabilitiesSchema;
    }
    if (capabilities.max_payload_bytes == 0 ||
        capabilities.max_payload_bytes > kMaxPayloadBytes) {
        return IdentityCodecStatus::InvalidMaxPayload;
    }
    if ((capabilities.service_flags & ~kKnownServiceFlags) != 0) {
        return IdentityCodecStatus::UnknownServiceBits;
    }
    if (!capabilities.supports_service(ServiceId::Identity)) {
        return IdentityCodecStatus::MissingIdentityService;
    }
    if ((capabilities.feature_flags & ~kKnownFeatureFlags) != 0) {
        return IdentityCodecStatus::UnknownFeatureBits;
    }
    const bool trial_confirmation =
        (capabilities.feature_flags & kFeatureTrialBootConfirmation) != 0;
    const bool remote_update =
        (capabilities.feature_flags & kFeatureRemoteFirmwareUpdate) != 0;
    if (trial_confirmation != remote_update) {
        return IdentityCodecStatus::InvalidFeatureCombination;
    }
    if ((capabilities.output_rate_mask &
         static_cast<uint16_t>(~kKnownOutputRateMask)) != 0) {
        return IdentityCodecStatus::UnknownOutputRateBits;
    }
    if (capabilities.supports_service(ServiceId::Measurement) &&
        (((capabilities.feature_flags & kFeatureSampleJournal) == 0) ||
         capabilities.journal_capacity_samples == 0 ||
         capabilities.output_rate_mask == 0 ||
         capabilities.max_sample_chunk_samples == 0)) {
        return IdentityCodecStatus::IncompleteMeasurementCapabilities;
    }
    const size_t chunk_bytes = kChunkFixedBytes +
        static_cast<size_t>(capabilities.max_sample_chunk_samples) *
            kXyz24SampleBytes;
    if (capabilities.supports_service(ServiceId::Measurement) &&
        (capabilities.max_sample_chunk_samples > kMaxWireChunkSamples ||
         capabilities.max_sample_chunk_samples >
             capabilities.journal_capacity_samples ||
         chunk_bytes > capabilities.max_payload_bytes)) {
        return IdentityCodecStatus::InvalidMaxSampleChunk;
    }
    return IdentityCodecStatus::Ok;
}

IdentityCodecStatus encode_identity_success(
    const NodeIdentity& identity, uint8_t* output, size_t output_capacity) {
    const IdentityCodecStatus status = validate(identity);
    if (status != IdentityCodecStatus::Ok) return status;
    if (output == nullptr || output_capacity < kIdentityResponseBytes) {
        return IdentityCodecStatus::OutputTooSmall;
    }
    output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
    std::memcpy(output + 1, identity.hardware_id, kHardwareIdBytes);
    write_u64_le(identity.boot_epoch, output + 9);
    output[17] = identity.node_address;
    output[18] = identity.board_revision;
    output[19] = identity.firmware.major;
    output[20] = identity.firmware.minor;
    output[21] = identity.firmware.patch;
    output[22] = identity.framing_version;
    output[23] = identity.application_version;
    return IdentityCodecStatus::Ok;
}

IdentityCodecStatus decode_identity(
    const uint8_t* payload, size_t payload_size, NodeIdentity& output,
    ResponseStatus& remote_status) {
    if (payload == nullptr || payload_size == 0) {
        return IdentityCodecStatus::InvalidPayloadLength;
    }
    if (!decode_status(payload[0], remote_status)) {
        return IdentityCodecStatus::UnknownResponseStatus;
    }
    if (remote_status != ResponseStatus::Ok) return IdentityCodecStatus::RemoteError;
    if (payload_size != kIdentityResponseBytes) {
        return IdentityCodecStatus::InvalidPayloadLength;
    }
    std::memcpy(output.hardware_id, payload + 1, kHardwareIdBytes);
    output.boot_epoch = read_u64_le(payload + 9);
    output.node_address = payload[17];
    output.board_revision = payload[18];
    output.firmware = {payload[19], payload[20], payload[21]};
    output.framing_version = payload[22];
    output.application_version = payload[23];
    return validate(output);
}

IdentityCodecStatus encode_capabilities_success(
    const NodeCapabilities& capabilities, uint8_t* output, size_t output_capacity) {
    const IdentityCodecStatus status = validate(capabilities);
    if (status != IdentityCodecStatus::Ok) return status;
    if (output == nullptr || output_capacity < kCapabilitiesResponseBytes) {
        return IdentityCodecStatus::OutputTooSmall;
    }
    output[0] = static_cast<uint8_t>(ResponseStatus::Ok);
    output[1] = capabilities.schema_version;
    write_u32_le(capabilities.service_flags, output + 2);
    write_u64_le(capabilities.feature_flags, output + 6);
    write_u16_le(capabilities.max_payload_bytes, output + 14);
    write_u32_le(capabilities.journal_capacity_samples, output + 16);
    write_u16_le(capabilities.output_rate_mask, output + 20);
    write_u16_le(capabilities.max_sample_chunk_samples, output + 22);
    return IdentityCodecStatus::Ok;
}

IdentityCodecStatus decode_capabilities(
    const uint8_t* payload, size_t payload_size, NodeCapabilities& output,
    ResponseStatus& remote_status) {
    if (payload == nullptr || payload_size == 0) {
        return IdentityCodecStatus::InvalidPayloadLength;
    }
    if (!decode_status(payload[0], remote_status)) {
        return IdentityCodecStatus::UnknownResponseStatus;
    }
    if (remote_status != ResponseStatus::Ok) return IdentityCodecStatus::RemoteError;
    if (payload_size != kCapabilitiesResponseBytes) {
        return IdentityCodecStatus::InvalidPayloadLength;
    }
    output.schema_version = payload[1];
    output.service_flags = read_u32_le(payload + 2);
    output.feature_flags = read_u64_le(payload + 6);
    output.max_payload_bytes = read_u16_le(payload + 14);
    output.journal_capacity_samples = read_u32_le(payload + 16);
    output.output_rate_mask = read_u16_le(payload + 20);
    output.max_sample_chunk_samples = read_u16_le(payload + 22);
    return validate(output);
}

}  // namespace ProtocolV3
