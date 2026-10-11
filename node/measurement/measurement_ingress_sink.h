#pragma once

#include <cstddef>
#include <cstdint>

#include "interfaces/i_acquisition_quality_sink.h"
#include "interfaces/i_sample_sink.h"
#include "measurement/sample_ingress.h"

namespace measurement {

struct MeasurementIngressSinkState {
    uint64_t accepted_samples = 0;
    uint64_t exact_gaps = 0;
    uint64_t continuity_breaks = 0;
    uint64_t rejected_events = 0;
    uint32_t config_revision = 0;
    bool faulted = false;
};

// Core-0 adapter from the acquisition pipeline to the protocol-5 measurement
// stream. It owns no queue and never blocks.
template <typename Ingress>
class MeasurementIngressSink final : public ISampleSink,
                                     public IAcquisitionQualitySink {
public:
    MeasurementIngressSink(Ingress& ingress, uint32_t config_revision)
        : ingress_(ingress), config_revision_(config_revision) {
        if (config_revision == 0) faulted_ = true;
    }

    void on_samples(const StoredSample* samples, size_t count) override {
        publish(samples, nullptr, count);
    }

    void on_timed_samples(const StoredSample* samples,
                          const SampleDeviceTime* times,
                          size_t count) override {
        publish(samples, times, count);
    }

    void on_continuity_break(ContinuityBreakReason reason) override {
        if (faulted_) return;
        const PublishStatus status = ingress_.publish_continuity_break(reason);
        if (status == PublishStatus::Queued ||
            status == PublishStatus::BufferedContinuityBreak) {
            saturating_increment(continuity_breaks_);
            return;
        }
        reject();
    }

    bool flush_pending() {
        if (faulted_) return false;
        if (!ingress_.flush_pending() && ingress_.state().faulted) {
            faulted_ = true;
            return false;
        }
        return true;
    }

    bool settled() const {
        const SampleIngressState ingress_state = ingress_.state();
        return !faulted_ && !ingress_state.faulted &&
               ingress_state.pending_events == 0 &&
               ingress_state.queue_size_approx == 0;
    }

    bool set_config_revision(uint32_t revision) {
        if (revision == 0 || faulted_) return false;
        config_revision_ = revision;
        return true;
    }

    MeasurementIngressSinkState state() const {
        return {
            accepted_samples_,
            exact_gaps_,
            continuity_breaks_,
            rejected_events_,
            config_revision_,
            faulted_,
        };
    }

private:
    static void saturating_increment(uint64_t& value) {
        if (value != UINT64_MAX) ++value;
    }

    void reject() {
        saturating_increment(rejected_events_);
        faulted_ = true;
    }

    void publish(const StoredSample* samples,
                 const SampleDeviceTime* times,
                 size_t count) {
        if (faulted_ || samples == nullptr) return;
        for (size_t index = 0; index < count; ++index) {
            const SampleDeviceTime time =
                times == nullptr ? SampleDeviceTime{} : times[index];
            const SampleRecord record{
                time.device_time_us,
                samples[index].x,
                samples[index].y,
                samples[index].z,
                config_revision_,
                time.timing_segment_id,
                time.quality_flags,
                0,
            };
            const PublishStatus status = ingress_.publish_sample(record);
            if (status == PublishStatus::Queued) {
                saturating_increment(accepted_samples_);
            } else if (status == PublishStatus::AccountedAsGap) {
                saturating_increment(exact_gaps_);
            } else {
                reject();
                return;
            }
        }
    }

    Ingress& ingress_;
    uint32_t config_revision_ = 0;
    uint64_t accepted_samples_ = 0;
    uint64_t exact_gaps_ = 0;
    uint64_t continuity_breaks_ = 0;
    uint64_t rejected_events_ = 0;
    bool faulted_ = false;
};

}  // namespace measurement
