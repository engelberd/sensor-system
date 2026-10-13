#include <cassert>

#include "config/node_v2_capabilities.h"

namespace {

void test_ab_image_reports_only_implemented_behavior() {
    constexpr auto capabilities = node_v2::make_node_capabilities(
        node_v2::FirmwareImageKind::AbSlot, true);

    static_assert(capabilities.schema_version ==
                  ProtocolV3::kCapabilitiesSchemaVersion);
    static_assert(capabilities.output_rate_mask ==
                  ProtocolV3::kImplementedOutputRateMask);
    static_assert((capabilities.feature_flags &
                   ProtocolV3::kFeatureDrdyTimestamps) != 0);
    static_assert((capabilities.feature_flags &
                   ProtocolV3::kFeatureTrialBootConfirmation) != 0);
    static_assert((capabilities.feature_flags &
                   ProtocolV3::kFeatureRemoteFirmwareUpdate) != 0);
    assert(capabilities.supports_service(ProtocolV3::ServiceId::Time));
    assert(capabilities.supports_service(ProtocolV3::ServiceId::Diagnostics));
    assert(capabilities.supports_service(ProtocolV3::ServiceId::Configuration));
    assert((capabilities.feature_flags &
            ProtocolV3::kFeatureAtomicConfig) != 0);
    assert(ProtocolV3::validate(capabilities) ==
           ProtocolV3::IdentityCodecStatus::Ok);
}

void test_standalone_image_does_not_claim_ab_services() {
    constexpr auto capabilities = node_v2::make_node_capabilities(
        node_v2::FirmwareImageKind::Standalone, false);

    static_assert(capabilities.feature_flags ==
                  (ProtocolV3::kFeatureSampleJournal |
                   ProtocolV3::kFeatureAtomicConfig));
    static_assert((capabilities.feature_flags &
                   ProtocolV3::kFeatureTrialBootConfirmation) == 0);
    static_assert((capabilities.feature_flags &
                   ProtocolV3::kFeatureRemoteFirmwareUpdate) == 0);
    assert(capabilities.supports_service(ProtocolV3::ServiceId::Time));
    assert(capabilities.supports_service(ProtocolV3::ServiceId::Diagnostics));
    assert(capabilities.supports_service(ProtocolV3::ServiceId::Configuration));
    assert(ProtocolV3::validate(capabilities) ==
           ProtocolV3::IdentityCodecStatus::Ok);
}

}  // namespace

int main() {
    test_ab_image_reports_only_implemented_behavior();
    test_standalone_image_does_not_claim_ab_services();
    return 0;
}
