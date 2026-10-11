#pragma once

#include "measurement/stream_continuity.h"

class IAcquisitionQualitySink {
public:
    virtual ~IAcquisitionQualitySink() = default;

    virtual void on_continuity_break(
        measurement::ContinuityBreakReason reason
    ) = 0;
};
