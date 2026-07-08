#pragma once

#include <array>
#include <cstddef>

#include "diagnostics/diagnostic_types.h"
#include "pico/critical_section.h"

class PersistentDiagnosticStore;

class DiagnosticStore {
public:
    static constexpr size_t kEventCapacity = 128;

    DiagnosticStore();

    void set_reset_cause(DiagnosticResetCause cause);
    DiagnosticResetCause reset_cause() const;

    void set_live_usb_enabled(bool enabled);
    bool live_usb_enabled() const;
    void set_persistent_store(PersistentDiagnosticStore* store);
    void set_boot_counter(uint32_t boot_counter);
    void set_firmware_version(uint32_t firmware_version);

    void record(DiagnosticSeverity severity,
                DiagnosticEventCode code,
                uint64_t sample_seq = 0,
                int32_t arg0 = 0,
                int32_t arg1 = 0);

    void record_fault(DiagnosticSeverity severity,
                      DiagnosticEventCode code,
                      const DiagnosticFaultContext& context);

    DiagnosticInfoSnapshot info() const;
    bool has_fault_snapshot() const;
    DiagnosticFaultSnapshot fault_snapshot() const;
    bool persistent_record(PersistentDiagnosticRecord& record) const;
    bool clear_persistent_record();

    size_t read_events(uint32_t start_event_id,
                       DiagnosticEvent* out,
                       size_t max_count,
                       uint32_t& next_event_id,
                       uint32_t& first_event_id) const;

    void clear();

private:
    static uint32_t current_time_ms();
    static const char* severity_name(DiagnosticSeverity severity);
    static const char* event_name(DiagnosticEventCode code);
    static bool should_store_event(DiagnosticSeverity severity, DiagnosticEventCode code);
    static bool should_capture_snapshot(DiagnosticSeverity severity, DiagnosticEventCode code);
    static bool can_coalesce_with(const DiagnosticEvent& stored, const DiagnosticEvent& candidate);

    void record_internal(DiagnosticSeverity severity,
                         DiagnosticEventCode code,
                         const DiagnosticFaultContext* fault_context,
                         DiagnosticEvent& emitted_event,
                         bool& should_log_usb);

private:
    mutable critical_section_t cs_{};
    std::array<DiagnosticEvent, kEventCapacity> events_{};
    size_t head_ = 0;
    size_t count_ = 0;
    uint32_t next_event_id_ = 1;
    uint32_t dropped_event_count_ = 0;
    uint32_t last_error_event_id_ = 0;
    uint16_t last_error_code_ = 0;
    DiagnosticResetCause reset_cause_ = DiagnosticResetCause::Unknown;
    DiagnosticFaultSnapshot last_fault_{};
    bool has_fault_snapshot_ = false;
    bool live_usb_enabled_ = true;
    DiagnosticSeverity usb_threshold_ = DiagnosticSeverity::Info;
    PersistentDiagnosticStore* persistent_store_ = nullptr;
    uint32_t boot_counter_ = 0;
    uint32_t firmware_version_ = 0;
    bool persistent_record_written_ = false;
};
