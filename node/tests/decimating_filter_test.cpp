#include <cassert>
#include <cstddef>
#include <cstdint>

#include "processing/decimating_filter.h"

namespace {

struct OutputStats {
    size_t count = 0;
    size_t first_output_raw_count = 0;
    uint64_t next_sample_seq = 0;
};

AccelSample make_sample(int32_t value) {
    AccelSample sample{};
    sample.x = value;
    sample.y = -value;
    sample.z = value * 2;
    return sample;
}

OutputStats feed_ramp(DecimationFilterProfile profile, size_t raw_count) {
    DecimatingFilterX2 filter{};
    filter.set_profile(profile);

    OutputStats stats{};
    for (size_t i = 0; i < raw_count; ++i) {
        AccelSample output{};
        if (!filter.process(make_sample(static_cast<int32_t>(i + 1)), output)) {
            continue;
        }

        if (stats.count == 0) {
            stats.first_output_raw_count = i + 1;
        }

        const uint64_t sample_seq = stats.next_sample_seq++;
        assert(sample_seq == stats.count);
        ++stats.count;
    }

    return stats;
}

void test_output_cadence_after_warmup() {
    const OutputStats light = feed_ramp(DecimationFilterProfile::Light, 100);
    assert(light.first_output_raw_count == 16);
    assert(light.count == 43);

    const OutputStats balanced = feed_ramp(DecimationFilterProfile::Balanced, 100);
    assert(balanced.first_output_raw_count == 32);
    assert(balanced.count == 35);

    const OutputStats aggressive = feed_ramp(DecimationFilterProfile::Aggressive, 100);
    assert(aggressive.first_output_raw_count == 64);
    assert(aggressive.count == 19);
}

void test_constant_signal_is_preserved() {
    DecimatingFilterX2 filter{};
    filter.set_profile(DecimationFilterProfile::Balanced);

    size_t output_count = 0;
    for (size_t i = 0; i < 96; ++i) {
        AccelSample input{};
        input.x = 1000;
        input.y = -2000;
        input.z = 12345;

        AccelSample output{};
        if (!filter.process(input, output)) {
            continue;
        }

        assert(output.x == input.x);
        assert(output.y == input.y);
        assert(output.z == input.z);
        ++output_count;
    }

    assert(output_count == 33);
}

void test_reset_restarts_warmup_and_sequence() {
    DecimatingFilterX2 filter{};
    filter.set_profile(DecimationFilterProfile::Light);

    size_t output_count = 0;
    for (size_t i = 0; i < 20; ++i) {
        AccelSample output{};
        if (filter.process(make_sample(static_cast<int32_t>(i)), output)) {
            ++output_count;
        }
    }
    assert(output_count == 3);

    filter.reset();

    output_count = 0;
    uint64_t next_sample_seq = 0;
    for (size_t i = 0; i < 20; ++i) {
        AccelSample output{};
        if (!filter.process(make_sample(static_cast<int32_t>(i)), output)) {
            continue;
        }

        const uint64_t sample_seq = next_sample_seq++;
        assert(sample_seq == output_count);
        ++output_count;
    }

    assert(output_count == 3);
    assert(next_sample_seq == output_count);
}

}  // namespace

int main() {
    test_output_cadence_after_warmup();
    test_constant_signal_is_preserved();
    test_reset_restarts_warmup_and_sequence();
    return 0;
}

