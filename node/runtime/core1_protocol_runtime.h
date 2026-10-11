#pragma once

#include <cstddef>
#include <cstdint>

#include "runtime/core1_measurement_runtime.h"

namespace runtime {

struct Core1ProtocolPolicy {
    size_t max_ingress_events_per_poll = 8;

    bool valid() const { return max_ingress_events_per_poll > 0; }
};

enum class Core1ProtocolPollStatus : uint8_t {
    Ok = 0,
    InvalidPolicy,
};

struct Core1ProtocolRuntimeState {
    Core1ProtocolPollStatus last_poll_status = Core1ProtocolPollStatus::Ok;
    IngressDrainStatus last_ingress_status = IngressDrainStatus::Idle;
    uint64_t poll_count = 0;
    uint64_t lifecycle_results_consumed = 0;
};

// Core-1 scheduler. Each collaborator owns one responsibility and must expose
// a bounded step. The fixed order makes progress deterministic without letting
// ingress draining, command reconciliation, RS485 or observation monopolize
// the core.
template <typename MeasurementRuntime,
          typename LifecycleService,
          typename TransportEndpoint,
          typename Observer>
class Core1ProtocolRuntime {
public:
    Core1ProtocolRuntime(MeasurementRuntime& measurement,
                         LifecycleService& lifecycle,
                         TransportEndpoint& transport,
                         Observer& observer,
                         Core1ProtocolPolicy policy = {})
        : measurement_(measurement),
          lifecycle_(lifecycle),
          transport_(transport),
          observer_(observer),
          policy_(policy) {}

    Core1ProtocolRuntime(const Core1ProtocolRuntime&) = delete;
    Core1ProtocolRuntime& operator=(const Core1ProtocolRuntime&) = delete;

    Core1ProtocolPollStatus poll(uint64_t now_us) {
        if (!policy_.valid()) {
            last_poll_status_ = Core1ProtocolPollStatus::InvalidPolicy;
            return last_poll_status_;
        }

        last_ingress_status_ = measurement_.poll_ingress(
            policy_.max_ingress_events_per_poll);
        if (lifecycle_.poll_result()) {
            increment(lifecycle_results_consumed_);
        }
        transport_.update(now_us);
        observer_.update();

        increment(poll_count_);
        last_poll_status_ = Core1ProtocolPollStatus::Ok;
        return last_poll_status_;
    }

    Core1ProtocolRuntimeState state() const {
        return {
            last_poll_status_,
            last_ingress_status_,
            poll_count_,
            lifecycle_results_consumed_,
        };
    }

    const Core1ProtocolPolicy& policy() const { return policy_; }

private:
    static void increment(uint64_t& value) {
        if (value != UINT64_MAX) ++value;
    }

    MeasurementRuntime& measurement_;
    LifecycleService& lifecycle_;
    TransportEndpoint& transport_;
    Observer& observer_;
    Core1ProtocolPolicy policy_{};
    Core1ProtocolPollStatus last_poll_status_ = Core1ProtocolPollStatus::Ok;
    IngressDrainStatus last_ingress_status_ = IngressDrainStatus::Idle;
    uint64_t poll_count_ = 0;
    uint64_t lifecycle_results_consumed_ = 0;
};

}  // namespace runtime
