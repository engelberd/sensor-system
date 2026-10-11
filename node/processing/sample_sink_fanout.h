#pragma once

#include <cstddef>

#include "interfaces/i_sample_sink.h"

// Keeps the production sink authoritative while allowing a passive secondary
// observer. Neither sink owns the other and no buffering policy is introduced.
class SampleSinkFanout final : public ISampleSink {
public:
    SampleSinkFanout(ISampleSink& primary, ISampleSink& observer)
        : primary_(primary), observer_(observer) {}

    void on_samples(const StoredSample* samples, size_t count) override {
        primary_.on_samples(samples, count);
        observer_.on_samples(samples, count);
    }

    void on_timed_samples(const StoredSample* samples,
                          const SampleDeviceTime* times,
                          size_t count) override {
        primary_.on_timed_samples(samples, times, count);
        observer_.on_timed_samples(samples, times, count);
    }

private:
    ISampleSink& primary_;
    ISampleSink& observer_;
};
