#pragma once

#include <cstddef>

#include "storage/stored_sample.h"
#include "timing/sample_timing.h"

class ISampleSink {
public:
    virtual ~ISampleSink() = default;

    virtual void on_samples(const StoredSample* samples, size_t count) = 0;

    virtual void on_timed_samples(const StoredSample* samples,
                                  const SampleDeviceTime* times,
                                  size_t count) {
        (void)times;
        on_samples(samples, count);
    }
};
