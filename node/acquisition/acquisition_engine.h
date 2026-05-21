#pragma once

#include <cstddef>
#include <cstdint>

#include "common/device_config.h"
#include "common/node_types.h"
#include "common/sensor_types.h"
#include "adxl355/adxl355_registers.h"
#include "interfaces/i_accelerometer.h"
#include "interfaces/i_sample_sink.h"
#include "pico/critical_section.h"
#include "processing/decimating_filter.h"
#include "storage/acquisition_buffer.h"

struct AcquisitionStats {
    uint64_t next_sample_seq = 0;

    uint32_t pushed_samples = 0;
    uint32_t dropped_samples = 0;
    uint32_t sample_buffer_overwrite_count = 0;

    uint32_t update_calls = 0;
    uint32_t fifo_reads = 0;
    uint32_t fifo_no_data = 0;
    uint32_t sensor_errors = 0;

    uint32_t fifo_irq_events = 0;
    uint32_t fifo_batches = 0;
    uint32_t fifo_samples_read = 0;
};

template <size_t BufferCapacity>
class AcquisitionEngine {
public:
    AcquisitionEngine(IAccelerometer& accelerometer,
                      AcquisitionBuffer<BufferCapacity>& buffer,
                      ISampleSink* sample_sink = nullptr)
        : accelerometer_(accelerometer),
          buffer_(buffer),
          sample_sink_(sample_sink) {
        critical_section_init(&cs_);
    }

    SensorStatus init(const DeviceConfig& config) {
        critical_section_enter_blocking(&cs_);

        SensorStatus st = accelerometer_.init();
        if (st != SensorStatus::Ok) {
            last_init_status_ = st;
            critical_section_exit(&cs_);
            return st;
        }

        st = accelerometer_.check_device();
        if (st != SensorStatus::Ok) {
            last_init_status_ = st;
            critical_section_exit(&cs_);
            return st;
        }

        st = apply_config_locked(config);
        if (st != SensorStatus::Ok) {
            last_init_status_ = st;
            critical_section_exit(&cs_);
            return st;
        }

        st = apply_offsets_locked(config);
        if (st != SensorStatus::Ok) {
            last_init_status_ = st;
            critical_section_exit(&cs_);
            return st;
        }

        initialized_ = true;
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

    void update_fifo_watermark_mode_locked() {
        if (accelerometer_.supports_data_ready_interrupt()) {
            if (!accelerometer_.consume_data_ready_event()) {
                return;
            }

            ++stats_.fifo_irq_events;
        }

        AccelSample samples[32]{};
        size_t total_samples_read = 0;

        while (true) {
            uint8_t status = 0;
            SensorStatus status_read = accelerometer_.read_status(status);
            if (status_read != SensorStatus::Ok) {
                ++stats_.sensor_errors;
                break;
            }

            if ((status & ADXL355::STATUS_FIFO_OVR_MASK) != 0) {
                ++stats_.dropped_samples;
            }

            size_t samples_read = 0;
            const SensorStatus st =
                accelerometer_.read_fifo_samples(samples, 32, samples_read);

            if (st == SensorStatus::NoData) {
                if (total_samples_read == 0) {
                    ++stats_.fifo_no_data;
                }
                break;
            }

            if (st != SensorStatus::Ok) {
                ++stats_.sensor_errors;
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
            return;
        }

        if (st != SensorStatus::Ok) {
            ++stats_.sensor_errors;
            return;
        }

        StoredSample stored{};
        if (!process_and_store_sample_locked(sample, stored)) {
            return;
        }

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

    mutable critical_section_t cs_{};
    IAccelerometer& accelerometer_;
    AcquisitionBuffer<BufferCapacity>& buffer_;
    ISampleSink* sample_sink_ = nullptr;
    DecimatingFilterX2 resampler_{};

    SensorStatus last_init_status_ = SensorStatus::NotInitialized;
    bool initialized_ = false;
    bool paused_ = false;
    AcquisitionStats stats_{};
};
