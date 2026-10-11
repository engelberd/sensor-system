#pragma once

#include <cstdint>

class ITransportDiagnostics {
public:
    virtual ~ITransportDiagnostics() = default;

    virtual uint32_t rx_overflow_count() const = 0;
};
