#pragma once

#include <cstddef>
#include <cstdint>

#include "common/device_config.h"
#include "common/node_types.h"
#include "common/sensor_types.h"
#include "diagnostics/diagnostic_store.h"
#include "adxl355/adxl355_registers.h"
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
        const SensorStatus st = accelerometer_.run_self_test(result);
        paused_ = was_paused;

        critical_section_exit(&cs_);
        return st;
    }

    void update() {
        critical_section_enter_blocking(&cs_);
        ++stats_.update_calls;

        if (!initialized_) {
            ++stats_.sensor_errors;
            critical_section_exit(&cs_);
            return;
        }

        if (paused_) {
            critical_section_exit(&cs_);
            return;
        }

        if (accelerometer_.supports_fifo()) {
            update_fifo_watermark_mode_locked();
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

        const uint32_t fill_time_ms = (watermark * 1000u + odr_hz - 1u) / odr_hz;
        uint32_t interval_ms = fill_time_ms + (fill_time_ms / 4u) + kIrqFallbackGuardMs;
        if (interval_ms < kMinIrqFallbackProbeIntervalMs) {
            interval_ms = kMinIrqFallbackProbeIntervalMs;
        }
        return interval_ms;
    }

    void sync_diagnostic_stats_locked(const SensorDiagnosticSnapshot& snapshot) {
        stats_.gpio_int1_edges = snapshot.int1_gpio_edges;
        stats_.gpio_drdy_edges = snapshot.drdy_gpio_edges;
        stats_.debug_config_snapshot = pack_debug_config_snapshot(snapshot);
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
            ++stats_.sensor_errors;
            ++stats_.consecutive_sensor_errors;
            record_fault_locked(
                DiagnosticSeverity::Warn,
                DiagnosticEventCode::SensorConfigFailed,
                st,
                static_cast<int32_t>(stats_.consecutive_no_data_reads)
            );
            return false;
        }

        stats_.consecutive_no_data_reads = 0;
        ++stats_.soft_recover_count;
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

    void update_fifo_watermark_mode_locked() {
        const uint32_t now_ms_value = now_ms();
        SensorDiagnosticSnapshot sensor_diag{};
        bool irq_seen = false;
        uint8_t irq_sources = 0;
        bool should_service_fifo = true;
        if (accelerometer_.supports_data_ready_interrupt()) {
            irq_sources = accelerometer_.consume_data_ready_event_sources();
            sensor_diag = accelerometer_.diagnostic_snapshot();
            sync_diagnostic_stats_locked(sensor_diag);

            const bool int1_event = (irq_sources & SENSOR_DIAG_FLAG_INT1_EVENT) != 0;
            const bool drdy_event = (irq_sources & SENSOR_DIAG_FLAG_DRDY_EVENT) != 0;
            const bool watermark_irq_configured = sensor_diag.last_int_map != 0;

            if (int1_event) {
                ++stats_.fifo_int1_events;
            }
            if (drdy_event) {
                ++stats_.fifo_drdy_events;
            }

            irq_seen = watermark_irq_configured ? int1_event : (int1_event || drdy_event);
            if (irq_seen) {
                ++stats_.fifo_irq_events;
                stats_.last_irq_event_ms = now_ms_value;
                last_fifo_service_ms_ = now_ms_value;
            } else if (!interval_elapsed(
                           now_ms_value,
                           last_fifo_service_ms_,
                           watermark_irq_configured
                               ? fifo_fallback_interval_ms_locked(sensor_diag)
                               : kLegacyPollingIntervalMs
                       )) {
                should_service_fifo = false;
            } else {
                ++stats_.fifo_poll_fallback_reads;
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

        while (true) {
            uint8_t status = 0;
            SensorStatus status_read = accelerometer_.read_status(status);
            if (status_read != SensorStatus::Ok) {
                ++stats_.sensor_errors;
                ++stats_.consecutive_sensor_errors;
                if (diagnostics_ != nullptr &&
                    should_log_streak(stats_.consecutive_sensor_errors, 1u)) {
                    diagnostics_->record_fault(
                        DiagnosticSeverity::Error,
                        DiagnosticEventCode::FifoStatusReadError,
                        build_fault_context_locked(
                            pack_sensor_snapshot_locked(irq_seen),
                            pack_sensor_status_streak(
                                status_read,
                                stats_.consecutive_sensor_errors
                            )
                        )
                    );
                }
                break;
            }

            if ((status & ADXL355::STATUS_FIFO_OVR_MASK) != 0) {
                ++stats_.dropped_samples;
                if (diagnostics_ != nullptr &&
                    should_log_streak(stats_.dropped_samples, 1u)) {
                    diagnostics_->record_fault(
                        DiagnosticSeverity::Warn,
                        DiagnosticEventCode::FifoOverrun,
                        build_fault_context_locked(
                            pack_sensor_snapshot_locked(irq_seen),
                            static_cast<int32_t>(stats_.dropped_samples)
                        )
                    );
                }
            }

            if (irq_seen && total_samples_read == 0) {
                uint8_t fifo_entries_before_read = 0;
                if (accelerometer_.read_fifo_entries(fifo_entries_before_read) == SensorStatus::Ok) {
                    const uint8_t configured_watermark =
                        (sensor_diag.last_fifo_samples != 0)
                            ? sensor_diag.last_fifo_samples
                            : static_cast<uint8_t>(last_config_.fifo_watermark);
                    uint8_t irq_flags = 0;
                    if ((status & ADXL355::STATUS_FIFO_FULL_MASK) != 0) {
                        irq_flags |= 0x01u;
                    } else {
                        ++stats_.irq_status_not_full;
                    }
                    if ((status & ADXL355::STATUS_FIFO_OVR_MASK) != 0) {
                        irq_flags |= 0x02u;
                    }
                    if (fifo_entries_before_read < 3u) {
                        irq_flags |= 0x04u;
                        ++stats_.irq_fifo_entries_lt_3;
                    }
                    if (fifo_entries_before_read < configured_watermark) {
                        irq_flags |= 0x08u;
                        ++stats_.irq_fifo_entries_lt_watermark;
                    }
                    stats_.debug_irq_snapshot = pack_irq_debug_snapshot(
                        status,
                        fifo_entries_before_read,
                        configured_watermark,
                        irq_flags
                    );
                }
            }

            size_t samples_read = 0;
            const SensorStatus st =
                accelerometer_.read_fifo_samples(samples, 32, samples_read);

            if (st == SensorStatus::NoData) {
                if (total_samples_read == 0) {
                    ++stats_.fifo_no_data;
                    ++stats_.consecutive_no_data_reads;
                    if (irq_seen) {
                        ++stats_.no_data_with_irq;
                    } else {
                        ++stats_.no_data_without_irq;
                    }
                    if (should_log_streak(stats_.consecutive_no_data_reads, 8u)) {
                        if (diagnostics_ != nullptr) {
                            diagnostics_->record_fault(
                                DiagnosticSeverity::Warn,
                                DiagnosticEventCode::FifoRepeatedNoData,
                                build_fault_context_locked(
                                    pack_sensor_snapshot_locked(irq_seen),
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
                ++stats_.sensor_errors;
                ++stats_.consecutive_sensor_errors;
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
                            pack_sensor_snapshot_locked(irq_seen),
                            pack_sensor_status_streak(
                                st,
                                stats_.consecutive_sensor_errors
                            )
                        )
                    );
                }
                break;
            }

            ++stats_.fifo_reads;
            ++stats_.fifo_batches;
            stats_.fifo_samples_read += static_cast<uint32_t>(samples_read);
            total_samples_read += samples_read;

            StoredSample stored_batch[32]{};
            size_t stored_count = 0;

            for (size_t i = 0; i < samples_read; ++i) {
                StoredSample stored{};
                if (process_and_store_sample_locked(samples[i], stored)) {
                    stored_batch[stored_count++] = stored;
                }
            }

            if (stored_count > 0) {
                const uint32_t previous_no_data = stats_.consecutive_no_data_reads;
                const uint32_t previous_sensor_errors = stats_.consecutive_sensor_errors;
                stats_.consecutive_no_data_reads = 0;
                stats_.consecutive_sensor_errors = 0;
                last_fifo_service_ms_ = now_ms();
                record_recovery_locked(previous_no_data, previous_sensor_errors);
            }

            if (sample_sink_ != nullptr && stored_count > 0) {
                sample_sink_->on_samples(stored_batch, stored_count);
            }

            if (samples_read < 32) {
                break;
            }
        }
    }

    void update_single_sample_mode_locked() {
        AccelSample sample{};
        const SensorStatus st = accelerometer_.read_sample(sample);

        if (st == SensorStatus::NoData) {
            ++stats_.consecutive_no_data_reads;
            return;
        }

        if (st != SensorStatus::Ok) {
            ++stats_.sensor_errors;
            ++stats_.consecutive_sensor_errors;
            return;
        }

        const uint32_t previous_no_data = stats_.consecutive_no_data_reads;
        const uint32_t previous_sensor_errors = stats_.consecutive_sensor_errors;
        stats_.consecutive_no_data_reads = 0;
        stats_.consecutive_sensor_errors = 0;

        StoredSample stored{};
        if (!process_and_store_sample_locked(sample, stored)) {
            return;
        }

        record_recovery_locked(previous_no_data, previous_sensor_errors);

        if (sample_sink_ != nullptr) {
            sample_sink_->on_samples(&stored, 1);
        }
    }

    bool process_and_store_sample_locked(const AccelSample& input,
                                         StoredSample& stored) {
        AccelSample output{};
        if (!resampler_.process(input, output)) {
            return false;
        }

        stored.sample_seq = stats_.next_sample_seq++;
        stats_.last_sample_seq = stored.sample_seq;
        stats_.last_progress_ms = now_ms();
        stored.x = output.x;
        stored.y = output.y;
        stored.z = output.z;

        if (buffer_.push_sample(stored)) {
            ++stats_.sample_buffer_overwrite_count;
        }
        ++stats_.pushed_samples;
        return true;
    }

private:
    static constexpr DecimationFilterProfile kDefaultFilterProfile =
        DecimationFilterProfile::Balanced;
    static constexpr uint32_t kLegacyPollingIntervalMs = 8;
    static constexpr uint32_t kMinIrqFallbackProbeIntervalMs = 25;
    static constexpr uint32_t kIrqFallbackGuardMs = 10;
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
