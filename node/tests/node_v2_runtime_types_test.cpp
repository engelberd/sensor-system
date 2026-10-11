#include <cassert>
#include <cstddef>

#include "runtime/node_v2_runtime_types.h"

namespace {

void test_provisional_profile_is_internally_consistent() {
    using Profile = node_v2::RuntimeCapacityProfile;
    assert(Profile::kIngressEvents == node_v2::IngressQueue::capacity());
    assert(Profile::kJournalSamples ==
           node_v2::Core1MeasurementRuntime::Journal::sample_capacity());
    assert(Profile::kJournalGaps ==
           node_v2::Core1MeasurementRuntime::Journal::gap_capacity());
    assert(Profile::kCommandMailboxEntries ==
           node_v2::OperationMailbox::command_capacity());
    assert(Profile::kResultMailboxEntries ==
           node_v2::OperationMailbox::result_capacity());
    assert(Profile::kMaxChunkSamples <= ProtocolV3::kMaxWireChunkSamples);
}

void test_profile_ram_cost_is_explicit_and_bounded() {
    const size_t ingress_bytes = sizeof(node_v2::IngressQueue);
    const size_t journal_storage_bytes =
        node_v2::Core1MeasurementRuntime::Journal::storage_bytes();
    assert(ingress_bytes <= 8u * 1024u);
    assert(journal_storage_bytes <= 72u * 1024u);
    assert(ingress_bytes + journal_storage_bytes <= 80u * 1024u);
}

}  // namespace

int main() {
    test_provisional_profile_is_internally_consistent();
    test_profile_ram_cost_is_explicit_and_bounded();
    return 0;
}
