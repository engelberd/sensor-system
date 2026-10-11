#pragma once

#include <cstddef>
#include <cstdint>

#include "measurement/delivery_session.h"
#include "measurement/sample_ingress.h"
#include "protocol_v3/measurement_handler.h"
#include "runtime/runtime_fault_latch.h"

namespace runtime {

enum class IngressDrainStatus : uint8_t {
    Idle = 0,
    Progress,
    Faulted,
    InvalidBudget,
};

struct Core1MeasurementRuntimeState {
    measurement::MeasurementJournalState journal{};
    uint64_t consumed_samples = 0;
    uint64_t consumed_gaps = 0;
    uint64_t consumed_continuity_breaks = 0;
    uint64_t rejected_events = 0;
    size_t ingress_waiting_approx = 0;
    bool faulted = false;
};

// Core-1 owner of the journal and delivery state. The only cross-core object
// it touches is the consumer side of the ingress SPSC queue.
template <size_t IngressCapacity,
          size_t JournalCapacity,
          size_t GapCapacity,
          size_t MaxChunkSamples>
class Core1MeasurementRuntime {
public:
    using IngressQueue =
        runtime::SpscQueue<measurement::IngressEvent, IngressCapacity>;
    using Journal =
        measurement::MeasurementJournal<JournalCapacity, GapCapacity>;
    using Session =
        measurement::DeliverySession<
            JournalCapacity, MaxChunkSamples, GapCapacity>;
    using Handler =
        ProtocolV3::MeasurementServiceHandler<
            JournalCapacity, MaxChunkSamples, GapCapacity>;

    Core1MeasurementRuntime(IngressQueue& ingress,
                            uint64_t boot_epoch,
                            RuntimeFaultLatch& fault_latch)
        : ingress_(ingress), session_(boot_epoch, journal_), handler_(session_),
          fault_latch_(fault_latch) {
        if (boot_epoch == 0) {
            faulted_ = true;
            (void)fault_latch_.trip(RuntimeFaultReason::InvalidBootEpoch);
        }
    }

    Core1MeasurementRuntime(const Core1MeasurementRuntime&) = delete;
    Core1MeasurementRuntime& operator=(const Core1MeasurementRuntime&) = delete;

    IngressDrainStatus poll_ingress(size_t max_events) {
        if (max_events == 0) return IngressDrainStatus::InvalidBudget;
        if (faulted_) return IngressDrainStatus::Faulted;

        size_t processed = 0;
        while (processed < max_events) {
            const measurement::ConsumeIngressStatus status =
                measurement::consume_ingress_once(ingress_, journal_);
            if (status == measurement::ConsumeIngressStatus::Empty) break;
            if (status == measurement::ConsumeIngressStatus::JournalRejected ||
                status == measurement::ConsumeIngressStatus::InvalidEvent) {
                increment(rejected_events_);
                faulted_ = true;
                (void)fault_latch_.trip(
                    RuntimeFaultReason::MeasurementJournalRejected
                );
                return IngressDrainStatus::Faulted;
            }
            if (status ==
                measurement::ConsumeIngressStatus::SampleAppended) {
                increment(consumed_samples_);
            } else if (status ==
                       measurement::ConsumeIngressStatus::GapAppended) {
                increment(consumed_gaps_);
            } else {
                increment(consumed_continuity_breaks_);
            }
            ++processed;
        }
        return processed == 0
            ? IngressDrainStatus::Idle
            : IngressDrainStatus::Progress;
    }

    Handler& handler() { return handler_; }
    const Handler& handler() const { return handler_; }

    Core1MeasurementRuntimeState state() const {
        return {
            journal_.state(),
            consumed_samples_,
            consumed_gaps_,
            consumed_continuity_breaks_,
            rejected_events_,
            ingress_.size_approx(),
            faulted_,
        };
    }

private:
    static void increment(uint64_t& value) {
        if (value != UINT64_MAX) ++value;
    }

    IngressQueue& ingress_;
    Journal journal_{};
    Session session_;
    Handler handler_;
    RuntimeFaultLatch& fault_latch_;
    uint64_t consumed_samples_ = 0;
    uint64_t consumed_gaps_ = 0;
    uint64_t consumed_continuity_breaks_ = 0;
    uint64_t rejected_events_ = 0;
    bool faulted_ = false;
};

}  // namespace runtime
