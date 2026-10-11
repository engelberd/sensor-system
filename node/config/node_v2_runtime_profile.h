#pragma once

#include <cstddef>

namespace node_v2 {

// Provisional integration profile. These values are deliberately centralized
// so HIL measurements can tune storage without changing domain or protocol
// types. They are not installation configuration and never depend on node ID,
// baudrate or ODR at runtime.
struct RuntimeCapacityProfile {
    static constexpr size_t kAcquisitionBufferSamples = 256;
    static constexpr size_t kIngressEvents = 128;
    static constexpr size_t kJournalSamples = 2048;
    static constexpr size_t kJournalGaps = 64;
    static constexpr size_t kMaxChunkSamples = 48;
    static constexpr size_t kCommandMailboxEntries = 4;
    static constexpr size_t kResultMailboxEntries = 4;
    static constexpr size_t kLifecycleResultCacheEntries = 8;
    static constexpr size_t kResponseOutboxEntries = 2;
    static constexpr size_t kIngressDrainBudgetPerPoll = 16;
};

}  // namespace node_v2
