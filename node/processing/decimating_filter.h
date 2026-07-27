#pragma once

#include <cstddef>
#include <cstdint>

#include "common/sensor_types.h"
#include "timing/sample_timing.h"

enum class DecimationFilterProfile : uint8_t {
    Light = 0,
    Balanced = 1,
    Aggressive = 2
};

class DecimatingFilterX2 {
public:
    static constexpr uint8_t kDecimationFactor = 2;

    void reset() {
        write_index_ = 0;
        samples_seen_ = 0;
        phase_ = 0;

        for (auto& sample : ring_) {
            sample = {};
        }
        for (auto& timestamp : time_ring_) {
            timestamp = {};
        }
    }

    void set_profile(DecimationFilterProfile profile) {
        profile_ = profile;
        reset();
    }

    DecimationFilterProfile profile() const {
        return profile_;
    }

    bool process(const AccelSample& input, AccelSample& output) {
        return process_internal(input, SampleDeviceTime{}, output, nullptr);
    }

    bool process(const AccelSample& input,
                 uint64_t input_time_us,
                 AccelSample& output,
                 uint64_t& output_time_us) {
        SampleDeviceTime input_time{};
        input_time.device_time_us = input_time_us;
        input_time.quality_flags = TIMING_QUALITY_LOCKED;
        input_time.source = TimestampSource::DrdyTimeUs64;
        SampleDeviceTime output_time{};
        const bool produced = process(
            input,
            input_time,
            output,
            output_time
        );
        output_time_us = output_time.device_time_us;
        return produced;
    }

    bool process(const AccelSample& input,
                 const SampleDeviceTime& input_time,
                 AccelSample& output,
                 SampleDeviceTime& output_time) {
        return process_internal(
            input,
            input_time,
            output,
            &output_time
        );
    }

private:
    bool process_internal(const AccelSample& input,
                          const SampleDeviceTime& input_time,
                          AccelSample& output,
                          SampleDeviceTime* output_time) {
        ring_[write_index_] = input;
        time_ring_[write_index_] = input_time;
        write_index_ = (write_index_ + 1) % kMaxTaps;

        if (samples_seen_ < kMaxTaps) {
            ++samples_seen_;
        }

        phase_ = static_cast<uint8_t>((phase_ + 1) % kDecimationFactor);
        if (phase_ != 0 || samples_seen_ < tap_count()) {
            return false;
        }

        output.x = filter_axis(Axis::X);
        output.y = filter_axis(Axis::Y);
        output.z = filter_axis(Axis::Z);
        if (output_time != nullptr) {
            const size_t taps = tap_count();
            const size_t oldest_index =
                (write_index_ + kMaxTaps - taps) % kMaxTaps;
            const size_t center_index =
                (oldest_index + (taps - 1) / 2) % kMaxTaps;
            *output_time = time_ring_[center_index];
        }
        return true;
    }

    enum class Axis : uint8_t {
        X,
        Y,
        Z
    };

    static constexpr size_t kLightTaps = 15;
    static constexpr size_t kBalancedTaps = 31;
    static constexpr size_t kAggressiveTaps = 63;
    static constexpr size_t kMaxTaps = kAggressiveTaps;
    static constexpr int32_t kQ15Scale = 32768;

    static constexpr int16_t kLightCoefficients[kLightTaps] = {
        -120, 0, 530, 0, -2242, 0, 9993, 16446,
        9993, 0, -2242, 0, 530, 0, -120
    };

    static constexpr int16_t kBalancedCoefficients[kBalancedTaps] = {
        -56, 0, 96, 0, -221, 0, 462, 0,
        -878, 0, 1609, 0, -3176, 0, 10342, 16412,
        10342, 0, -3176, 0, 1609, 0, -878, 0,
        462, 0, -221, 0, 96, 0, -56
    };

    static constexpr int16_t kAggressiveCoefficients[kAggressiveTaps] = {
        0, 0, 1, 0, -6, 0, 16, 0,
        -32, 0, 60, 0, -102, 0, 164, 0,
        -254, 0, 381, 0, -561, 0, 818, 0,
        -1209, 0, 1876, 0, -3347, 0, 10387, 16384,
        10387, 0, -3347, 0, 1876, 0, -1209, 0,
        818, 0, -561, 0, 381, 0, -254, 0,
        164, 0, -102, 0, 60, 0, -32, 0,
        16, 0, -6, 0, 1, 0, 0
    };

    size_t tap_count() const {
        switch (profile_) {
            case DecimationFilterProfile::Light:
                return kLightTaps;
            case DecimationFilterProfile::Aggressive:
                return kAggressiveTaps;
            case DecimationFilterProfile::Balanced:
            default:
                return kBalancedTaps;
        }
    }

    const int16_t* coefficients() const {
        switch (profile_) {
            case DecimationFilterProfile::Light:
                return kLightCoefficients;
            case DecimationFilterProfile::Aggressive:
                return kAggressiveCoefficients;
            case DecimationFilterProfile::Balanced:
            default:
                return kBalancedCoefficients;
        }
    }

    int32_t sample_axis(const AccelSample& sample, Axis axis) const {
        switch (axis) {
            case Axis::X:
                return sample.x;
            case Axis::Y:
                return sample.y;
            case Axis::Z:
            default:
                return sample.z;
        }
    }

    int32_t filter_axis(Axis axis) const {
        const size_t taps = tap_count();
        const int16_t* coeffs = coefficients();
        const size_t oldest_index = (write_index_ + kMaxTaps - taps) % kMaxTaps;

        int64_t acc = 0;
        for (size_t i = 0; i < taps; ++i) {
            const size_t ring_index = (oldest_index + i) % kMaxTaps;
            acc += static_cast<int64_t>(sample_axis(ring_[ring_index], axis)) *
                   static_cast<int64_t>(coeffs[i]);
        }

        if (acc >= 0) {
            acc += kQ15Scale / 2;
        } else {
            acc -= kQ15Scale / 2;
        }

        return static_cast<int32_t>(acc / kQ15Scale);
    }

private:
    AccelSample ring_[kMaxTaps]{};
    SampleDeviceTime time_ring_[kMaxTaps]{};
    size_t write_index_ = 0;
    size_t samples_seen_ = 0;
    uint8_t phase_ = 0;
    DecimationFilterProfile profile_ = DecimationFilterProfile::Balanced;
};
