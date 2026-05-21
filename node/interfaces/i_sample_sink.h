#pragma once

#include <cstddef>

#include "storage/stored_sample.h"

class ISampleSink {
public:
    virtual ~ISampleSink() = default;

    virtual void on_samples(const StoredSample* samples, size_t count) = 0;
};