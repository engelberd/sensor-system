#pragma once

#include <cstdint>

#include "config/node_v2_runtime_profile.h"
#include "processing/decimating_filter.h"
#include "protocol_v3/identity.h"

namespace node_v2 {

enum class FirmwareImageKind : uint8_t {
    Standalone,
    AbSlot,
};

constexpr ProtocolV3::NodeCapabilities make_node_capabilities(
    FirmwareImageKind image_kind,
    bool drdy_timestamps_enabled) {
    static_assert(DecimatingFilterX2::kDecimationFactor == 2,
                  "output-rate capabilities must follow the acquisition pipeline");
    static_assert(RuntimeCapacityProfile::kMaxChunkSamples <=
                      ProtocolV3::kMaxWireChunkSamples,
                  "advertised chunk must fit the Measurement wire payload");
    static_assert(RuntimeCapacityProfile::kMaxChunkSamples <=
                      RuntimeCapacityProfile::kJournalSamples,
                  "advertised chunk must fit the journal");

    ProtocolV3::NodeCapabilities capabilities{};
    capabilities.service_flags =
        ProtocolV3::service_capability_bit(ProtocolV3::ServiceId::Identity) |
        ProtocolV3::service_capability_bit(ProtocolV3::ServiceId::Measurement) |
        ProtocolV3::service_capability_bit(ProtocolV3::ServiceId::Time) |
        ProtocolV3::service_capability_bit(ProtocolV3::ServiceId::Lifecycle);
    capabilities.feature_flags = ProtocolV3::kFeatureSampleJournal;
    if (drdy_timestamps_enabled) {
        capabilities.feature_flags |= ProtocolV3::kFeatureDrdyTimestamps;
    }
    if (image_kind == FirmwareImageKind::AbSlot) {
        capabilities.feature_flags |=
            ProtocolV3::kFeatureTrialBootConfirmation |
            ProtocolV3::kFeatureRemoteFirmwareUpdate;
    }
    capabilities.max_payload_bytes = ProtocolV3::kMaxPayloadBytes;
    capabilities.journal_capacity_samples =
        RuntimeCapacityProfile::kJournalSamples;
    capabilities.output_rate_mask = ProtocolV3::kImplementedOutputRateMask;
    capabilities.max_sample_chunk_samples =
        RuntimeCapacityProfile::kMaxChunkSamples;
    return capabilities;
}

}  // namespace node_v2
