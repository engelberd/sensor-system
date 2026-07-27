#pragma once

#include <cstddef>
#include <cstdint>

#include "common/device_config.h"
#include "common/node_types.h"
#include "common/sensor_types.h"
#include "diagnostics/diagnostic_store.h"
#include "adxl355/adxl355_registers.h"
#include "acquisition/fifo_service_policy.h"
#include "interfaces/i_accelerometer.h"
#include "interfaces/i_sample_sink.h"
#include "pico/critical_section.h"
#include "pico/time.h"
#include "processing/decimating_filter.h"
#include "storage/acquisition_buffer.h"

struct AcquisitionStats {
    uint64_t next_sample_seq = 0;
    uint64_t last_sample_seq = 0;

    uint32_t pushed_samples = 0;
    uint32_t dropped_samples = 0;
    uint32_t sample_buffer_overwrite_count = 0;
    uint32_t last_progress_ms = 0;

    uint32_t update_calls = 0;
    uint32_t fifo_reads = 0;
    uint32_t fifo_no_data = 0;
    uint32_t sensor_errors = 0;
    uint32_t consecutive_no_data_reads = 0;
    uint32_t consecutive_sensor_errors = 0;

    uint32_t fifo_irq_events = 0;
    uint32_t fifo_batches = 0;
    uint32_t fifo_samples_read = 0;
    uint32_t fifo_int1_events = 0;
    uint32_t fifo_drdy_events = 0;
    uint32_t fifo_poll_fallback_reads = 0;
    uint32_t no_data_with_irq = 0;
    uint32_t no_data_without_irq = 0;
    uint32_t soft_recover_count = 0;
    uint32_t last_irq_event_ms = 0;
    uint32_t last_soft_recover_ms = 0;
    uint32_t gpio_int1_edges = 0;
    uint32_t gpio_drdy_edges = 0;
    uint32_t debug_config_snapshot = 0;
    uint32_t irq_status_not_full = 0;
    uint32_t irq_fifo_entries_lt_3 = 0;
    uint32_t irq_fifo_entries_lt_watermark = 0;
    uint32_t debug_irq_snapshot = 0;
    uint32_t spurious_int1_events = 0;
    uint32_t fifo_overrun_events = 0;
    uint32_t fifo_discarded_samples = 0;
    uint32_t fifo_uncertain_loss_events = 0;
    uint32_t drdy_timestamp_ring_overflow = 0;
    uint32_t timing_binding_mismatch = 0;
    uint32_t timing_binding_invalidations = 0;
    uint32_t timing_segment_id = 0;
};

template <size_t BufferCapacity>
class AcquisitionEngine {
public:
    AcquisitionEngine(IAccelerometer& accelerometer,
                      AcquisitionBuffer<BufferCapacity>& buffer,
                      ISampleSink* sample_sink = nullptr,
                      DiagnosticStore* diagnostics = nullptr)
        : accelerometer_(accelerometer),
          buffer_(buffer),
          sample_sink_(sample_sink),
          diagnostics_(diagnostics) {
        critical_section_init(&cs_);
    }

    SensorStatus init(const DeviceConfig& config) {
        critical_section_enter_blocking(&cs_);

        SensorStatus st = accelerometer_.init();
        if (st != SensorStatus::Ok) {
            last_init_status_ = st;
            record_fault_locked(DiagnosticSeverity::Error, DiagnosticEventCode::SensorInitFailed, st, 0);
            critical_section_exit(&cs_);
            return st;
        }

        st = accelerometer_.check_device();
        if (st != SensorStatus::Ok) {
            last_init_status_ = st;
            record_fault_locked(DiagnosticSeverity::Error, DiagnosticEventCode::SensorCheckFailed, st, 0);
            critical_section_exit(&cs_);
            return st;
        }

        st = apply_config_locked(config);
        if (st != SensorStatus::Ok) {
            last_init_status_ = st;
            record_fault_locked(DiagnosticSeverity::Error, DiagnosticEventCode::SensorConfigFailed, st, 0);
            critical_section_exit(&cs_);
            return st;
        }

        st = apply_offsets_locked(config);
        if (st != SensorStatus::Ok) {
            last_init_status_ = st;
            record_fault_locked(DiagnosticSeverity::Error, DiagnosticEventCode::SensorOffsetsFailed, st, 0);
            critical_section_exit(&cs_);
            return st;
        }

        initialized_ = true;
        last_config_ = config;
        has_last_config_ = true;
        last_fifo_service_ms_ = now_ms();
        last_init_status_ = SensorStatus::Ok;
        critical_section_exit(&cs_);
        return SensorStatus::Ok;
    }

    SensorStatus last_init_status() const {
        critical_section_enter_blocking(const_cast<critical_section_t*>(&cs_));
        const SensorStatus st = last_init_status_;
        critical_section_exit(&cs_);
        return st;
    }

    SensorStatus apply_config(const DeviceConfig& config) {
        critical_section_enter_blocking(&cs_);
        const SensorStatus st = apply_config_locked(config);
        if (st == SensorStatus::Ok) {
            last_config_ = config;
            has_last_config_ = true;
            last_fifo_service_ms_ = now_ms();
        }
        critical_section_exit(&cs_);
        return st;
    }

    SensorStatus apply_fifo_config(const DeviceConfig& config) {
        critical_section_enter_blocking(&cs_);
        const SensorStatus st = apply_fifo_config_locked(config);
        critical_section_exit(&cs_);
        return st;
    }

    SensorStatus apply_offsets(const DeviceConfig& config) {
        critical_section_enter_blocking(&cs_);
        const SensorStatus st = apply_offsets_locked(config);
        critical_section_exit(&cs_);
        return st;
    }

    SensorStatus reload_runtime_config(const DeviceConfig& config) {
        critical_section_enter_blocking(&cs_);

        if (initialized_ && accelerometer_.supports_fifo()) {
            update_fifo_watermark_mode_locked(true);
        }

        SensorStatus st = apply_config_locked(config);
        if (st != SensorStatus::Ok) {
            critical_section_exit(&cs_);
            return st;
        }

        st = apply_offsets_locked(config);
        critical_section_exit(&cs_);

        if (st != SensorStatus::Ok) {
            return st;
        }

        critical_section_enter_blocking(&cs_);
        last_config_ = config;
        has_last_config_ = true;
        last_fifo_service_ms_ = now_ms();
        critical_section_exit(&cs_);

        return SensorStatus::Ok;
    }

    void pause() {
        critical_section_enter_blocking(&cs_);
        paused_ = true;
        critical_section_exit(&cs_);
    }

    void resume() {
        critical_section_enter_blocking(&cs_);
        paused_ = false;
        critical_section_exit(&cs_);
    }

    bool is_paused() const {
        critical_section_enter_blocking(const_cast<critical_section_t*>(&cs_));
        const bool paused = paused_;
        critical_section_exit(&cs_);
        return paused;
    }

    SensorStatus run_self_test(SelfTestResult& result) {
        critical_section_enter_blocking(&cs_);

        const bool was_paused = paused_;
        paused_ = true;
        if (accelerometer_.supports_fifo()) {
            update_fifo_watermark_mode_locked(true);
        }

        const SensorStatus test_status = accelerometer_.run_self_test(result);
        SensorStatus restore_status = SensorStatus::Ok;
        if (has_last_config_) {
            restore_status = apply_config_locked(last_config_);
            if (restore_status == SensorStatus::Ok) {
                restore_status = apply_offsets_locked(last_config_);
            }
            last_fifo_service_ms_ = now_ms();
        }
        paused_ = was_paused;

        critical_section_exit(&cs_);
        return restore_status != SensorStatus::Ok ? restore_status : test_status;
    }

    void update() {
        critical_section_enter_blocking(&cs_);
        saturating_increment(stats_.update_calls);

        if (!initialized_) {
            saturating_increment(stats_.sensor_errors);
            critical_section_exit(&cs_);
            return;
        }

        if (paused_) {
            critical_section_exit(&cs_);
            return;
        }

        if (accelerometer_.supports_fifo()) {
            update_fifo_watermark_mode_locked(false);
            critical_section_exit(&cs_);
            return;
        }

        update_single_sample_mode_locked();
        critical_section_exit(&cs_);
    }

    AcquisitionStats stats() const {
        critical_section_enter_blocking(const_cast<critical_section_t*>(&cs_));
        const AcquisitionStats stats = stats_;
        critical_section_exit(&cs_);
        return stats;
    }

    BufferState buffer_state() const {
        return buffer_.state();
    }

    size_t read_latest(StoredSample* out, size_t max_count) const {
        return buffer_.copy_latest(out, max_count);
    }

    size_t read_from_seq(uint64_t start_seq,
                         StoredSample* out,
                         size_t max_count) const {
        return buffer_.copy_from_seq(start_seq, out, max_count);
    }

private:
    static uint32_t now_ms() {
        return static_cast<uint32_t>(time_us_64() / 1000u);
    }

    static bool should_log_streak(uint32_t streak, uint32_t threshold) {
        return streak >= threshold && (streak & (streak - 1u)) == 0u;
    }

    static bool interval_elapsed(uint32_t now, uint32_t since, uint32_t interval_ms) {
        return static_cast<uint32_t>(now - since) >= interval_ms;
    }

    static void saturating_increment(uint32_t& value) {
        if (value != UINT32_MAX) {
            ++value;
        }
    }

    static void saturating_add(uint32_t& value, uint32_t increment) {
        if (increment > UINT32_MAX - value) {
            value = UINT32_MAX;
        } else {
            value += increment;
        }
    }

    static uint32_t pack_debug_config_snapshot(const SensorDiagnosticSnapshot& snapshot) {
        return
            static_cast<uint32_t>(snapshot.last_int_map) |
            (static_cast<uint32_t>(snapshot.last_fifo_samples) << 8) |
            (static_cast<uint32_t>(snapshot.last_int1_level) << 16) |
            (static_cast<uint32_t>(snapshot.last_drdy_level) << 24);
    }

    static uint32_t pack_irq_debug_snapshot(uint8_t status,
                                            uint8_t fifo_entries,
                                            uint8_t watermark,
                                            uint8_t flags) {
        return
            static_cast<uint32_t>(status) |
            (static_cast<uint32_t>(fifo_entries) << 8) |
            (static_cast<uint32_t>(watermark) << 16) |
            (static_cast<uint32_t>(flags) << 24);
    }

    uint32_t fifo_fallback_interval_ms_locked(const SensorDiagnosticSnapshot& snapshot) const {
        if (!has_last_config_) {
            return kLegacyPollingIntervalMs;
        }

        uint32_t odr_hz = last_config_.odr_hz;
        if (odr_hz == 0) {
            odr_hz = 250;
        }

        uint32_t watermark = snapshot.last_fifo_samples;
        if (watermark == 0) {
            watermark = last_config_.fifo_watermark;
        }
        if (watermark < 3u) {
            watermark = 3u;
        }

        return FifoServicePolicy::fallback_interval_ms(odr_hz, watermark);
    }

    void sync_diagnostic_stats_locked(const SensorDiagnosticSnapshot& snapshot) {
        stats_.gpio_int1_edges = snapshot.int1_gpio_edges;
        stats_.gpio_drdy_edges = snapshot.drdy_gpio_edges;
        stats_.debug_config_snapshot = pack_debug_config_snapshot(snapshot);
        stats_.drdy_timestamp_ring_overflow =
            snapshot.drdy_timestamp_ring_overflow;
        stats_.timing_binding_mismatch =
            snapshot.timing_binding_mismatch;
        stats_.timing_binding_invalidations =
            snapshot.timing_binding_invalidations;
        stats_.timing_segment_id = snapshot.timing_segment_id;
    }

    int32_t pack_sensor_snapshot_locked(bool irq_seen) {
        (void)accelerometer_.refresh_diagnostic_snapshot();
        const SensorDiagnosticSnapshot snapshot = accelerometer_.diagnostic_snapshot();
        sync_diagnostic_stats_locked(snapshot);
        uint32_t flags = snapshot.flags;
        if (irq_seen) {
            flags |= 0x80u;
        }

        const uint32_t packed =
            static_cast<uint32_t>(snapshot.last_status_reg) |
            (static_cast<uint32_t>(snapshot.last_fifo_entries) << 8) |
            (static_cast<uint32_t>(snapshot.last_fifo_read_status) << 16) |
            ((flags & 0xFFu) << 24);
        return static_cast<int32_t>(packed);
    }

    static int32_t pack_sensor_status_streak(SensorStatus status, uint32_t streak) {
        const uint32_t packed =
            static_cast<uint32_t>(static_cast<uint8_t>(status)) |
            ((streak & 0x00FFFFFFu) << 8);
        return static_cast<int32_t>(packed);
    }

    DiagnosticFaultContext build_fault_context_locked(int32_t arg0, int32_t arg1) const {
        DiagnosticFaultContext context{};
        context.sample_seq = stats_.last_sample_seq;
        context.last_progress_ms = stats_.last_progress_ms;
        context.fifo_no_data = stats_.fifo_no_data;
        context.sensor_errors = stats_.sensor_errors;
        context.dropped_samples = stats_.dropped_samples;
        context.debug_gpio_int1_edges = stats_.gpio_int1_edges;
        context.debug_gpio_drdy_edges = stats_.gpio_drdy_edges;
        context.debug_config_snapshot = stats_.debug_config_snapshot;
        context.debug_irq_snapshot = stats_.debug_irq_snapshot;
        context.arg0 = arg0;
        context.arg1 = arg1;
        return context;
    }

    void record_fault_locked(DiagnosticSeverity severity,
                             DiagnosticEventCode code,
                             SensorStatus sensor_status,
                             int32_t arg1) {
        if (diagnostics_ == nullptr) {
            return;
        }
        diagnostics_->record_fault(
            severity,
            code,
            build_fault_context_locked(static_cast<int32_t>(sensor_status), arg1)
        );
    }

    void record_recovery_locked(uint32_t previous_no_data, uint32_t previous_sensor_errors) {
        if (diagnostics_ == nullptr) {
            return;
        }
        if (previous_no_data == 0 && previous_sensor_errors == 0) {
            return;
        }
        diagnostics_->record(
            DiagnosticSeverity::Info,
            DiagnosticEventCode::AcquisitionRecovered,
            stats_.last_sample_seq,
            static_cast<int32_t>(previous_no_data),
            static_cast<int32_t>(previous_sensor_errors)
        );
    }

    SensorStatus apply_config_locked(const DeviceConfig& config) {
        AccelerometerConfig accel_cfg{};
        accel_cfg.odr_hz = config.odr_hz;
        accel_cfg.range_g = config.range_g;
        accel_cfg.high_pass_corner = config.high_pass_corner;

        SensorStatus st = accelerometer_.configure(accel_cfg);
        if (st != SensorStatus::Ok) {
            return st;
        }

        st = apply_fifo_config_locked(config);
        if (st != SensorStatus::Ok) {
            return st;
        }

        resampler_.set_profile(kDefaultFilterProfile);
        return SensorStatus::Ok;
    }

    SensorStatus apply_fifo_config_locked(const DeviceConfig& config) {
        uint8_t watermark = config.fifo_watermark;

        if (watermark < 3) {
            watermark = 3;
        }
        if (watermark > 96) {
            watermark = 96;
        }
        watermark = static_cast<uint8_t>(watermark - (watermark % 3));

        if (!accelerometer_.supports_fifo()) {
            return SensorStatus::NotSupported;
        }

        return accelerometer_.configure_fifo(watermark);
    }

    SensorStatus apply_offsets_locked(const DeviceConfig& config) {
        return accelerometer_.set_offset(
            config.offset_x,
            config.offset_y,
            config.offset_z
        );
    }

    bool maybe_soft_recover_from_no_data_locked(uint32_t now_ms_value) {
        if (!has_last_config_) {
            return false;
        }
        if (stats_.consecutive_no_data_reads < kSoftRecoverNoDataThreshold) {
            return false;
        }
        if (!interval_elapsed(now_ms_value, last_soft_recover_ms_, kSoftRecoverCooldownMs)) {
            return false;
        }

        last_soft_recover_ms_ = now_ms_value;
        const SensorStatus st = apply_fifo_config_locked(last_config_);
        if (st != SensorStatus::Ok) {
            saturating_increment(stats_.sensor_errors);
            saturating_increment(stats_.consecutive_sensor_errors);
            record_fault_locked(
                DiagnosticSeverity::Warn,
                DiagnosticEventCode::SensorConfigFailed,
                st,
                static_cast<int32_t>(stats_.consecutive_no_data_reads)
            );
            return false;
        }

        stats_.consecutive_no_data_reads = 0;
        saturating_increment(stats_.soft_recover_count);
        stats_.last_soft_recover_ms = now_ms_value;
        last_fifo_service_ms_ = now_ms_value;
        if (diagnostics_ != nullptr) {
            diagnostics_->record(
                DiagnosticSeverity::Info,
                DiagnosticEventCode::RuntimeConfigApplied,
                stats_.last_sample_seq,
                static_cast<int32_t>(last_config_.fifo_watermark),
                static_cast<int32_t>(kSoftRecoverNoDataThreshold)
            );
        }
        return true;
    }

    void update_fifo_watermark_mode_locked(bool force_service) {
        const uint32_t now_ms_value = now_ms();
        SensorDiagnosticSnapshot sensor_diag{};
        bool raw_irq_seen = false;
        uint8_t irq_sources = 0;
        bool should_service_fifo = true;
        if (accelerometer_.supports_data_ready_interrupt()) {
            irq_sources = accelerometer_.consume_data_ready_event_sources();
            sensor_diag = accelerometer_.diagnostic_snapshot();
            sync_diagnostic_stats_locked(sensor_diag);

            const bool int1_event = (irq_sources & SENSOR_DIAG_FLAG_INT1_EVENT) != 0;
            const bool drdy_event = (irq_sources & SENSOR_DIAG_FLAG_DRDY_EVENT) != 0;
            const bool watermark_irq_configured =
                FifoServicePolicy::watermark_irq_configured(sensor_diag.last_int_map);

            if (int1_event) {
                saturating_increment(stats_.fifo_int1_events);
            }
            if (drdy_event) {
                saturating_increment(stats_.fifo_drdy_events);
            }

            raw_irq_seen = watermark_irq_configured ? int1_event : (int1_event || drdy_event);
            if (raw_irq_seen) {
                saturating_increment(stats_.fifo_irq_events);
            } else if (!force_service && !interval_elapsed(
                           now_ms_value,
                           last_fifo_service_ms_,
                           watermark_irq_configured
                               ? fifo_fallback_interval_ms_locked(sensor_diag)
                               : kLegacyPollingIntervalMs
                       )) {
                should_service_fifo = false;
            } else if (!force_service) {
                saturating_increment(stats_.fifo_poll_fallback_reads);
            }
        } else {
            sensor_diag = accelerometer_.diagnostic_snapshot();
            sync_diagnostic_stats_locked(sensor_diag);
        }
        if (!should_service_fifo) {
            return;
        }

        AccelSample samples[32]{};
        size_t total_samples_read = 0;
        bool first_read = true;

        while (true) {
            uint8_t status = 0;
            SensorStatus status_read = accelerometer_.read_status(status);
            if (status_read != SensorStatus::Ok) {
                saturating_increment(stats_.sensor_errors);
                saturating_increment(stats_.consecutive_sensor_errors);
                if (diagnostics_ != nullptr &&
                    should_log_streak(stats_.consecutive_sensor_errors, 1u)) {
                    diagnostics_->record_fault(
                        DiagnosticSeverity::Error,
                        DiagnosticEventCode::FifoStatusReadError,
                        build_fault_context_locked(
                            pack_sensor_snapshot_locked(raw_irq_seen),
                            pack_sensor_status_streak(
                                status_read,
                                stats_.consecutive_sensor_errors
                            )
                        )
                    );
                }
                break;
            }

            uint8_t fifo_entries_before_read = 0;
            const SensorStatus entries_status =
                accelerometer_.read_fifo_entries(fifo_entries_before_read);
            if (entries_status != SensorStatus::Ok) {
                saturating_increment(stats_.sensor_errors);
                saturating_increment(stats_.consecutive_sensor_errors);
                if (diagnostics_ != nullptr &&
                    should_log_streak(stats_.consecutive_sensor_errors, 1u)) {
                    diagnostics_->record_fault(
                        DiagnosticSeverity::Error,
                        DiagnosticEventCode::FifoStatusReadError,
                        build_fault_context_locked(
                            pack_sensor_snapshot_locked(raw_irq_seen),
                            pack_sensor_status_streak(
                                entries_status,
                                stats_.consecutive_sensor_errors
                            )
                        )
                    );
                }
                break;
            }

            const uint8_t configured_watermark =
                (sensor_diag.last_fifo_samples != 0)
                    ? sensor_diag.last_fifo_samples
                    : static_cast<uint8_t>(last_config_.fifo_watermark);

            if (raw_irq_seen && first_read) {
                uint8_t irq_flags = 0;
                if ((status & ADXL355::STATUS_FIFO_FULL_MASK) != 0) {
                    irq_flags |= 0x01u;
                } else {
                    saturating_increment(stats_.irq_status_not_full);
                }
                if ((status & ADXL355::STATUS_FIFO_OVR_MASK) != 0) {
                    irq_flags |= 0x02u;
                }
                if (fifo_entries_before_read < 3u) {
                    irq_flags |= 0x04u;
                    saturating_increment(stats_.irq_fifo_entries_lt_3);
                }
                if (fifo_entries_before_read < configured_watermark) {
                    irq_flags |= 0x08u;
                    saturating_increment(stats_.irq_fifo_entries_lt_watermark);
                }
                stats_.debug_irq_snapshot = pack_irq_debug_snapshot(
                    status,
                    fifo_entries_before_read,
                    configured_watermark,
                    irq_flags
                );

                if (!FifoServicePolicy::fifo_ready(
                        status,
                        fifo_entries_before_read,
                        configured_watermark)) {
                    saturating_increment(stats_.spurious_int1_events);
                    return;
                }

                // Only a sensor-confirmed IRQ is allowed to reset IRQ health timing.
                stats_.last_irq_event_ms = now_ms_value;
            }

            if (force_service && !raw_irq_seen && fifo_entries_before_read < 3u) {
                return;
            }

            if ((status & ADXL355::STATUS_FIFO_OVR_MASK) != 0) {
                saturating_increment(stats_.fifo_overrun_events);
                accelerometer_.notify_fifo_timing_discontinuity(
                    TIMING_QUALITY_INVALID | TIMING_QUALITY_FIFO_OVERRUN
                );
                // ADXL355 exposes only a sticky overrun bit, so one sample is a lower bound.
                saturating_increment(stats_.dropped_samples);
                if (diagnostics_ != nullptr &&
                    should_log_streak(stats_.fifo_overrun_events, 1u)) {
                    diagnostics_->record_fault(
                        DiagnosticSeverity::Warn,
                        DiagnosticEventCode::FifoOverrun,
                        build_fault_context_locked(
                            pack_sensor_snapshot_locked(raw_irq_seen),
                            static_cast<int32_t>(stats_.fifo_overrun_events)
                        )
                    );
                }
            }

            size_t samples_read = 0;
            SampleDeviceTime sample_times[32]{};
            const SensorStatus st =
                accelerometer_.read_fifo_samples_timed(
                    samples,
                    sample_times,
                    32,
                    samples_read
                );
            const SensorDiagnosticSnapshot read_diag = accelerometer_.diagnostic_snapshot();
            const uint32_t discarded_samples = read_diag.last_fifo_discarded_samples;

            if (discarded_samples > 0u) {
                saturating_add(stats_.fifo_discarded_samples, discarded_samples);
                saturating_add(stats_.dropped_samples, discarded_samples);
            }
            if ((read_diag.flags & SENSOR_DIAG_FLAG_FIFO_LOSS_UNCERTAIN) != 0u) {
                saturating_increment(stats_.fifo_uncertain_loss_events);
            }

            if (samples_read > 0) {
                saturating_increment(stats_.fifo_reads);
                saturating_increment(stats_.fifo_batches);
                saturating_add(stats_.fifo_samples_read, static_cast<uint32_t>(samples_read));
                total_samples_read += samples_read;

                if (accelerometer_.supports_sample_timestamps()) {
                    bool timing_valid = true;
                    for (size_t i = 0; i < samples_read; ++i) {
                        if (!sample_times[i].valid()) {
                            timing_valid = false;
                            break;
                        }
                    }
                    if (!timing_valid) {
                        saturating_add(
                            stats_.dropped_samples,
                            static_cast<uint32_t>(
                                (samples_read + 1u) /
                                DecimatingFilterX2::kDecimationFactor
                            )
                        );
                        saturating_increment(stats_.sensor_errors);
                        resampler_.reset();
                        if (has_last_config_) {
                            const SensorStatus recovery_status =
                                apply_fifo_config_locked(last_config_);
                            if (recovery_status == SensorStatus::Ok) {
                                saturating_increment(
                                    stats_.soft_recover_count
                                );
                                stats_.last_soft_recover_ms = now_ms_value;
                            }
                        }
                        record_fault_locked(
                            DiagnosticSeverity::Error,
                            DiagnosticEventCode::InvalidSample,
                            SensorStatus::InvalidSample,
                            static_cast<int32_t>(samples_read)
                        );
                        return;
                    }
                }

                StoredSample stored_batch[32]{};
                SampleDeviceTime stored_times[32]{};
                size_t stored_count = 0;

                for (size_t i = 0; i < samples_read; ++i) {
                    StoredSample stored{};
                    SampleDeviceTime output_time{};
                    if (process_and_store_sample_locked(
                            samples[i],
                            accelerometer_.supports_sample_timestamps()
                                ? &sample_times[i]
                                : nullptr,
                            stored,
                            output_time)) {
                        stored_batch[stored_count] = stored;
                        stored_times[stored_count] = output_time;
                        ++stored_count;
                    }
                }

                const uint32_t previous_no_data = stats_.consecutive_no_data_reads;
                const uint32_t previous_sensor_errors = stats_.consecutive_sensor_errors;
                stats_.consecutive_no_data_reads = 0;
                if (st == SensorStatus::Ok) {
                    stats_.consecutive_sensor_errors = 0;
                }
                last_fifo_service_ms_ = now_ms();
                record_recovery_locked(previous_no_data, previous_sensor_errors);

                if (sample_sink_ != nullptr && stored_count > 0) {
                    sample_sink_->on_timed_samples(
                        stored_batch,
                        stored_times,
                        stored_count
                    );
                }
            }

            if (st == SensorStatus::NoData) {
                if (discarded_samples > 0u) {
                    saturating_increment(stats_.sensor_errors);
                    saturating_increment(stats_.consecutive_sensor_errors);
                    record_fault_locked(
                        DiagnosticSeverity::Error,
                        DiagnosticEventCode::InvalidSample,
                        st,
                        static_cast<int32_t>(discarded_samples)
                    );
                } else if (total_samples_read == 0) {
                    saturating_increment(stats_.fifo_no_data);
                    saturating_increment(stats_.consecutive_no_data_reads);
                    if (raw_irq_seen) {
                        saturating_increment(stats_.no_data_with_irq);
                    } else {
                        saturating_increment(stats_.no_data_without_irq);
                    }
                    if (should_log_streak(stats_.consecutive_no_data_reads, 8u)) {
                        if (diagnostics_ != nullptr) {
                            diagnostics_->record_fault(
                                DiagnosticSeverity::Warn,
                                DiagnosticEventCode::FifoRepeatedNoData,
                                build_fault_context_locked(
                                    pack_sensor_snapshot_locked(raw_irq_seen),
                                    static_cast<int32_t>(stats_.consecutive_no_data_reads)
                                )
                            );
                        }
                    }
                    maybe_soft_recover_from_no_data_locked(now_ms_value);
                }
                break;
            }

            if (st != SensorStatus::Ok) {
                saturating_increment(stats_.sensor_errors);
                saturating_increment(stats_.consecutive_sensor_errors);
                if (diagnostics_ != nullptr &&
                    should_log_streak(stats_.consecutive_sensor_errors, 1u)) {
                    diagnostics_->record_fault(
                        (st == SensorStatus::InvalidSample)
                            ? DiagnosticSeverity::Error
                            : DiagnosticSeverity::Warn,
                        (st == SensorStatus::InvalidSample)
                            ? DiagnosticEventCode::InvalidSample
                            : DiagnosticEventCode::SensorReadError,
                        build_fault_context_locked(
                            pack_sensor_snapshot_locked(raw_irq_seen),
                            pack_sensor_status_streak(
                                st,
                                stats_.consecutive_sensor_errors
                            )
                        )
                    );
                }
                break;
            }

            if (samples_read == 0) {
                saturating_increment(stats_.sensor_errors);
                saturating_increment(stats_.consecutive_sensor_errors);
                record_fault_locked(
                    DiagnosticSeverity::Error,
                    DiagnosticEventCode::SensorReadError,
                    SensorStatus::InternalError,
                    0
                );
                break;
            }

            if (samples_read < 32) {
                break;
            }
            first_read = false;
            raw_irq_seen = false;
        }
    }

    void update_single_sample_mode_locked() {
        AccelSample sample{};
        const SensorStatus st = accelerometer_.read_sample(sample);

        if (st == SensorStatus::NoData) {
            saturating_increment(stats_.consecutive_no_data_reads);
            return;
        }

        if (st != SensorStatus::Ok) {
            saturating_increment(stats_.sensor_errors);
            saturating_increment(stats_.consecutive_sensor_errors);
            return;
        }

        const uint32_t previous_no_data = stats_.consecutive_no_data_reads;
        const uint32_t previous_sensor_errors = stats_.consecutive_sensor_errors;
        stats_.consecutive_no_data_reads = 0;
        stats_.consecutive_sensor_errors = 0;

        StoredSample stored{};
        SampleDeviceTime output_time{};
        if (!process_and_store_sample_locked(
                sample,
                nullptr,
                stored,
                output_time)) {
            return;
        }

        record_recovery_locked(previous_no_data, previous_sensor_errors);

        if (sample_sink_ != nullptr) {
            sample_sink_->on_samples(&stored, 1);
        }
    }

    bool process_and_store_sample_locked(const AccelSample& input,
                                         const SampleDeviceTime* input_time,
                                         StoredSample& stored,
                                         SampleDeviceTime& output_time) {
        AccelSample output{};
        const bool produced =
            input_time != nullptr
                ? resampler_.process(input, *input_time, output, output_time)
                : resampler_.process(input, output);
        if (!produced) {
            return false;
        }

        stored.sample_seq = stats_.next_sample_seq++;
        stats_.last_sample_seq = stored.sample_seq;
        stats_.last_progress_ms = now_ms();
        stored.x = output.x;
        stored.y = output.y;
        stored.z = output.z;

        if (buffer_.push_sample(stored)) {
            saturating_increment(stats_.sample_buffer_overwrite_count);
        }
        saturating_increment(stats_.pushed_samples);
        return true;
    }

private:
    static constexpr DecimationFilterProfile kDefaultFilterProfile =
        DecimationFilterProfile::Balanced;
    static constexpr uint32_t kLegacyPollingIntervalMs = 8;
    static constexpr uint32_t kSoftRecoverNoDataThreshold = 32;
    static constexpr uint32_t kSoftRecoverCooldownMs = 1000;

    mutable critical_section_t cs_{};
    IAccelerometer& accelerometer_;
    AcquisitionBuffer<BufferCapacity>& buffer_;
    ISampleSink* sample_sink_ = nullptr;
    DiagnosticStore* diagnostics_ = nullptr;
    DecimatingFilterX2 resampler_{};

    SensorStatus last_init_status_ = SensorStatus::NotInitialized;
    bool initialized_ = false;
    bool paused_ = false;
    AcquisitionStats stats_{};
    DeviceConfig last_config_{};
    bool has_last_config_ = false;
    uint32_t last_fifo_service_ms_ = 0;
    uint32_t last_soft_recover_ms_ = 0;
};
