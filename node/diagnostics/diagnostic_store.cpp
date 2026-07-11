#include "diagnostics/diagnostic_store.h"

#include <cstdio>

#include "diagnostics/persistent_diagnostic_store.h"
#include "pico/time.h"

namespace {

constexpr bool severity_at_least(DiagnosticSeverity value, DiagnosticSeverity threshold) {
    return static_cast<uint8_t>(value) >= static_cast<uint8_t>(threshold);
}

}  // namespace

DiagnosticStore::DiagnosticStore() {
    critical_section_init(&cs_);
}

void DiagnosticStore::set_reset_cause(DiagnosticResetCause cause) {
    critical_section_enter_blocking(&cs_);
    reset_cause_ = cause;
    critical_section_exit(&cs_);
}

DiagnosticResetCause DiagnosticStore::reset_cause() const {
    critical_section_enter_blocking(const_cast<critical_section_t*>(&cs_));
    const auto cause = reset_cause_;
    critical_section_exit(const_cast<critical_section_t*>(&cs_));
    return cause;
}

void DiagnosticStore::set_live_usb_enabled(bool enabled) {
    critical_section_enter_blocking(&cs_);
    live_usb_enabled_ = enabled;
    critical_section_exit(&cs_);
}

bool DiagnosticStore::live_usb_enabled() const {
    critical_section_enter_blocking(const_cast<critical_section_t*>(&cs_));
    const bool enabled = live_usb_enabled_;
    critical_section_exit(const_cast<critical_section_t*>(&cs_));
    return enabled;
}

void DiagnosticStore::set_persistent_store(PersistentDiagnosticStore* store) {
    critical_section_enter_blocking(&cs_);
    persistent_store_ = store;
    persistent_record_written_ = false;
    critical_section_exit(&cs_);
}

void DiagnosticStore::set_boot_counter(uint32_t boot_counter) {
    critical_section_enter_blocking(&cs_);
    boot_counter_ = boot_counter;
    critical_section_exit(&cs_);
}

void DiagnosticStore::set_firmware_version(uint32_t firmware_version) {
    critical_section_enter_blocking(&cs_);
    firmware_version_ = firmware_version;
    critical_section_exit(&cs_);
}

void DiagnosticStore::record(DiagnosticSeverity severity,
                             DiagnosticEventCode code,
                             uint64_t sample_seq,
                             int32_t arg0,
                             int32_t arg1) {
    DiagnosticEvent emitted{};
    bool should_log_usb = false;
    DiagnosticFaultContext context{};
    context.sample_seq = sample_seq;
    context.arg0 = arg0;
    context.arg1 = arg1;
    record_internal(severity, code, &context, emitted, should_log_usb);
    if (should_log_usb) {
        std::printf(
            "[DIAG][%lu ms][%s][%s] event_id=%lu seq=%llu arg0=%ld arg1=%ld\n",
            static_cast<unsigned long>(emitted.time_ms),
            severity_name(severity),
            event_name(code),
            static_cast<unsigned long>(emitted.event_id),
            static_cast<unsigned long long>(sample_seq),
            static_cast<long>(arg0),
            static_cast<long>(arg1)
        );
    }
}

void DiagnosticStore::record_fault(DiagnosticSeverity severity,
                                   DiagnosticEventCode code,
                                   const DiagnosticFaultContext& context) {
    DiagnosticEvent emitted{};
    bool should_log_usb = false;
    record_internal(severity, code, &context, emitted, should_log_usb);
    if (should_log_usb) {
        std::printf(
            "[DIAG][%lu ms][%s][%s] event_id=%lu seq=%llu last_progress_ms=%lu fifo_no_data=%lu sensor_errors=%lu dropped=%lu rx_overflow=%lu packet_overwrite=%lu gpio_int1_edges=%lu gpio_drdy_edges=%lu debug_cfg=0x%08lx debug_irq=0x%08lx arg0=%ld arg1=%ld\n",
            static_cast<unsigned long>(emitted.time_ms),
            severity_name(severity),
            event_name(code),
            static_cast<unsigned long>(emitted.event_id),
            static_cast<unsigned long long>(context.sample_seq),
            static_cast<unsigned long>(context.last_progress_ms),
            static_cast<unsigned long>(context.fifo_no_data),
            static_cast<unsigned long>(context.sensor_errors),
            static_cast<unsigned long>(context.dropped_samples),
            static_cast<unsigned long>(context.rx_overflow_count),
            static_cast<unsigned long>(context.packet_overwrite_count),
            static_cast<unsigned long>(context.debug_gpio_int1_edges),
            static_cast<unsigned long>(context.debug_gpio_drdy_edges),
            static_cast<unsigned long>(context.debug_config_snapshot),
            static_cast<unsigned long>(context.debug_irq_snapshot),
            static_cast<long>(context.arg0),
            static_cast<long>(context.arg1)
        );
    }
}

DiagnosticInfoSnapshot DiagnosticStore::info() const {
    critical_section_enter_blocking(const_cast<critical_section_t*>(&cs_));
    DiagnosticInfoSnapshot snapshot{};
    snapshot.first_event_id =
        (count_ == 0) ? next_event_id_ : events_[(head_ + kEventCapacity - count_) % kEventCapacity].event_id;
    snapshot.next_event_id = next_event_id_;
    snapshot.dropped_event_count = dropped_event_count_;
    snapshot.last_error_event_id = last_error_event_id_;
    snapshot.last_error_code = last_error_code_;
    snapshot.event_capacity = static_cast<uint16_t>(kEventCapacity);
    snapshot.stored_event_count = static_cast<uint16_t>(count_);
    snapshot.reset_cause = static_cast<uint8_t>(reset_cause_);
    snapshot.live_usb_enabled = live_usb_enabled_ ? 1u : 0u;
    critical_section_exit(const_cast<critical_section_t*>(&cs_));
    return snapshot;
}

bool DiagnosticStore::has_fault_snapshot() const {
    critical_section_enter_blocking(const_cast<critical_section_t*>(&cs_));
    const bool value = has_fault_snapshot_;
    critical_section_exit(const_cast<critical_section_t*>(&cs_));
    return value;
}

DiagnosticFaultSnapshot DiagnosticStore::fault_snapshot() const {
    critical_section_enter_blocking(const_cast<critical_section_t*>(&cs_));
    const auto snapshot = last_fault_;
    critical_section_exit(const_cast<critical_section_t*>(&cs_));
    return snapshot;
}

bool DiagnosticStore::persistent_record(PersistentDiagnosticRecord& record) const {
    critical_section_enter_blocking(const_cast<critical_section_t*>(&cs_));
    auto* store = persistent_store_;
    critical_section_exit(const_cast<critical_section_t*>(&cs_));
    return store != nullptr && store->load(record);
}

bool DiagnosticStore::clear_persistent_record() {
    critical_section_enter_blocking(&cs_);
    auto* store = persistent_store_;
    persistent_record_written_ = false;
    critical_section_exit(&cs_);
    return store != nullptr && store->clear();
}

size_t DiagnosticStore::read_events(uint32_t start_event_id,
                                    DiagnosticEvent* out,
                                    size_t max_count,
                                    uint32_t& next_event_id,
                                    uint32_t& first_event_id) const {
    critical_section_enter_blocking(const_cast<critical_section_t*>(&cs_));

    first_event_id =
        (count_ == 0) ? next_event_id_ : events_[(head_ + kEventCapacity - count_) % kEventCapacity].event_id;
    next_event_id = next_event_id_;

    if (out == nullptr || max_count == 0 || count_ == 0) {
        critical_section_exit(const_cast<critical_section_t*>(&cs_));
        return 0;
    }

    size_t copied = 0;
    const size_t first_index = (head_ + kEventCapacity - count_) % kEventCapacity;
    for (size_t i = 0; i < count_ && copied < max_count; ++i) {
        const size_t index = (first_index + i) % kEventCapacity;
        const DiagnosticEvent& event = events_[index];
        if (event.event_id < start_event_id) {
            continue;
        }
        out[copied++] = event;
    }

    critical_section_exit(const_cast<critical_section_t*>(&cs_));
    return copied;
}

void DiagnosticStore::clear() {
    critical_section_enter_blocking(&cs_);
    head_ = 0;
    count_ = 0;
    dropped_event_count_ = 0;
    last_error_event_id_ = 0;
    last_error_code_ = 0;
    last_fault_ = DiagnosticFaultSnapshot{};
    has_fault_snapshot_ = false;
    critical_section_exit(&cs_);

    record(DiagnosticSeverity::Info, DiagnosticEventCode::DiagnosticsCleared, 0, 0, 0);
}

uint32_t DiagnosticStore::current_time_ms() {
    return static_cast<uint32_t>(time_us_64() / 1000u);
}

const char* DiagnosticStore::severity_name(DiagnosticSeverity severity) {
    switch (severity) {
        case DiagnosticSeverity::Debug:
            return "DEBUG";
        case DiagnosticSeverity::Info:
            return "INFO";
        case DiagnosticSeverity::Warn:
            return "WARN";
        case DiagnosticSeverity::Error:
            return "ERROR";
        case DiagnosticSeverity::Critical:
            return "CRITICAL";
        default:
            return "UNKNOWN";
    }
}

const char* DiagnosticStore::event_name(DiagnosticEventCode code) {
    switch (code) {
        case DiagnosticEventCode::Boot:
            return "BOOT";
        case DiagnosticEventCode::ControllerInitOk:
            return "CONTROLLER_INIT_OK";
        case DiagnosticEventCode::ControllerInitFailed:
            return "CONTROLLER_INIT_FAILED";
        case DiagnosticEventCode::Rs485InitFailed:
            return "RS485_INIT_FAILED";
        case DiagnosticEventCode::TransportInitFailed:
            return "TRANSPORT_INIT_FAILED";
        case DiagnosticEventCode::SensorInitFailed:
            return "SENSOR_INIT_FAILED";
        case DiagnosticEventCode::SensorCheckFailed:
            return "SENSOR_CHECK_FAILED";
        case DiagnosticEventCode::SensorConfigFailed:
            return "SENSOR_CONFIG_FAILED";
        case DiagnosticEventCode::SensorOffsetsFailed:
            return "SENSOR_OFFSETS_FAILED";
        case DiagnosticEventCode::FifoOverrun:
            return "FIFO_OVERRUN";
        case DiagnosticEventCode::FifoStatusReadError:
            return "FIFO_STATUS_READ_ERROR";
        case DiagnosticEventCode::FifoRepeatedNoData:
            return "FIFO_REPEATED_NO_DATA";
        case DiagnosticEventCode::SensorReadError:
            return "SENSOR_READ_ERROR";
        case DiagnosticEventCode::InvalidSample:
            return "INVALID_SAMPLE";
        case DiagnosticEventCode::AcquisitionRecovered:
            return "ACQUISITION_RECOVERED";
        case DiagnosticEventCode::TemperatureReadFailed:
            return "TEMPERATURE_READ_FAILED";
        case DiagnosticEventCode::TemperatureReadRecovered:
            return "TEMPERATURE_READ_RECOVERED";
        case DiagnosticEventCode::RuntimeConfigApplyFailed:
            return "RUNTIME_CONFIG_APPLY_FAILED";
        case DiagnosticEventCode::RuntimeConfigApplied:
            return "RUNTIME_CONFIG_APPLIED";
        case DiagnosticEventCode::RestartCommand:
            return "RESTART_COMMAND";
        case DiagnosticEventCode::EnterBootloaderCommand:
            return "ENTER_BOOTLOADER_COMMAND";
        case DiagnosticEventCode::DiagnosticsCleared:
            return "DIAGNOSTICS_CLEARED";
        default:
            return "UNKNOWN_EVENT";
    }
}

bool DiagnosticStore::should_store_event(DiagnosticSeverity severity, DiagnosticEventCode code) {
    if (severity_at_least(severity, DiagnosticSeverity::Warn)) {
        return true;
    }

    switch (code) {
        case DiagnosticEventCode::Boot:
        case DiagnosticEventCode::AcquisitionRecovered:
        case DiagnosticEventCode::RuntimeConfigApplied:
        case DiagnosticEventCode::DiagnosticsCleared:
            return true;
        default:
            return false;
    }
}

bool DiagnosticStore::should_capture_snapshot(DiagnosticSeverity severity, DiagnosticEventCode code) {
    if (severity_at_least(severity, DiagnosticSeverity::Error)) {
        return true;
    }

    if (severity != DiagnosticSeverity::Warn) {
        return false;
    }

    switch (code) {
        case DiagnosticEventCode::FifoOverrun:
        case DiagnosticEventCode::FifoRepeatedNoData:
        case DiagnosticEventCode::SensorReadError:
        case DiagnosticEventCode::TemperatureReadFailed:
            return true;
        default:
            return false;
    }
}

bool DiagnosticStore::can_coalesce_with(const DiagnosticEvent& stored, const DiagnosticEvent& candidate) {
    return stored.code == candidate.code &&
           stored.severity == candidate.severity &&
           stored.arg0 == candidate.arg0 &&
           stored.arg1 == candidate.arg1 &&
           stored.repeat_count < 0xFF;
}

void DiagnosticStore::record_internal(DiagnosticSeverity severity,
                                      DiagnosticEventCode code,
                                      const DiagnosticFaultContext* fault_context,
                                      DiagnosticEvent& emitted_event,
                                      bool& should_log_usb) {
    const uint32_t now_ms = current_time_ms();
    critical_section_enter_blocking(&cs_);

    DiagnosticEvent event{};
    event.event_id = next_event_id_++;
    event.time_ms = now_ms;
    event.code = static_cast<uint16_t>(code);
    event.severity = static_cast<uint8_t>(severity);
    if (fault_context != nullptr) {
        event.sample_seq = fault_context->sample_seq;
        event.arg0 = fault_context->arg0;
        event.arg1 = fault_context->arg1;
    }

    const bool store_event = should_store_event(severity, code);
    if (store_event) {
        const size_t last_index = (head_ + kEventCapacity - 1) % kEventCapacity;
        const bool can_coalesce = count_ > 0 && can_coalesce_with(events_[last_index], event);
        if (can_coalesce) {
            ++events_[last_index].repeat_count;
            event = events_[last_index];
        } else if (count_ >= kEventCapacity) {
            // Unread history is never overwritten. The host acknowledges and
            // clears archived batches; saturation remains explicitly visible.
            ++dropped_event_count_;
        } else {
            events_[head_] = event;
            head_ = (head_ + 1) % kEventCapacity;
            ++count_;
        }
    }

    if (severity_at_least(severity, DiagnosticSeverity::Error)) {
        last_error_event_id_ = event.event_id;
        last_error_code_ = static_cast<uint16_t>(code);
    }

    const bool should_snapshot =
        fault_context != nullptr && should_capture_snapshot(severity, code);
    const bool replace_snapshot =
        should_snapshot &&
        (!has_fault_snapshot_ ||
         (severity_at_least(severity, DiagnosticSeverity::Error) &&
          last_fault_.severity < static_cast<uint8_t>(DiagnosticSeverity::Error)));

    if (replace_snapshot) {
        last_fault_.event_id = event.event_id;
        last_fault_.time_ms = now_ms;
        last_fault_.code = static_cast<uint16_t>(code);
        last_fault_.severity = static_cast<uint8_t>(severity);
        last_fault_.reset_cause = static_cast<uint8_t>(reset_cause_);
        last_fault_.sample_seq = fault_context->sample_seq;
        last_fault_.last_progress_ms = fault_context->last_progress_ms;
        last_fault_.fifo_no_data = fault_context->fifo_no_data;
        last_fault_.sensor_errors = fault_context->sensor_errors;
        last_fault_.dropped_samples = fault_context->dropped_samples;
        last_fault_.rx_overflow_count = fault_context->rx_overflow_count;
        last_fault_.packet_overwrite_count = fault_context->packet_overwrite_count;
        last_fault_.debug_gpio_int1_edges = fault_context->debug_gpio_int1_edges;
        last_fault_.debug_gpio_drdy_edges = fault_context->debug_gpio_drdy_edges;
        last_fault_.debug_config_snapshot = fault_context->debug_config_snapshot;
        last_fault_.debug_irq_snapshot = fault_context->debug_irq_snapshot;
        last_fault_.arg0 = fault_context->arg0;
        last_fault_.arg1 = fault_context->arg1;
        has_fault_snapshot_ = true;
    }

    if (should_snapshot && !persistent_record_written_ && persistent_store_ != nullptr) {
        PersistentDiagnosticRecord persistent{};
        persistent.reset_cause = static_cast<uint8_t>(reset_cause_);
        persistent.repeat_count = event.repeat_count;
        persistent.boot_counter = boot_counter_;
        persistent.firmware_version = firmware_version_;
        persistent.event_id = event.event_id;
        persistent.time_ms = now_ms;
        persistent.event_code = static_cast<uint16_t>(code);
        persistent.severity = static_cast<uint8_t>(severity);
        persistent.sample_seq = fault_context->sample_seq;
        persistent.last_progress_ms = fault_context->last_progress_ms;
        persistent.fifo_no_data = fault_context->fifo_no_data;
        persistent.sensor_errors = fault_context->sensor_errors;
        persistent.dropped_samples = fault_context->dropped_samples;
        persistent.rx_overflow_count = fault_context->rx_overflow_count;
        persistent.packet_overwrite_count = fault_context->packet_overwrite_count;
        persistent.debug_gpio_int1_edges = fault_context->debug_gpio_int1_edges;
        persistent.debug_gpio_drdy_edges = fault_context->debug_gpio_drdy_edges;
        persistent.debug_config_snapshot = fault_context->debug_config_snapshot;
        persistent.debug_irq_snapshot = fault_context->debug_irq_snapshot;
        persistent.arg0 = fault_context->arg0;
        persistent.arg1 = fault_context->arg1;
        persistent_record_written_ = persistent_store_->save(persistent);
    }

    should_log_usb = live_usb_enabled_ && severity_at_least(severity, usb_threshold_);
    emitted_event = event;
    critical_section_exit(&cs_);
}
