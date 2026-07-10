#pragma once

#include "../../../diagnostics/diagnostic_types.h"

class DiagnosticStore {
public:
    void record(DiagnosticSeverity,
                DiagnosticEventCode,
                uint64_t,
                int32_t,
                int32_t) {}

    void record_fault(DiagnosticSeverity,
                      DiagnosticEventCode,
                      const DiagnosticFaultContext&) {}
};
