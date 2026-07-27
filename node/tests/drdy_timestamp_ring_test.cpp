#include <cassert>
#include <cstddef>
#include <cstdint>

#include "timing/drdy_timestamp_ring.h"

namespace {

DrdyTimestamp timestamp(uint64_t seq) {
    return DrdyTimestamp{seq, 1'000'000u + seq * 4'000u};
}

void test_fifo_order_and_capacity() {
    DrdyTimestampRing<4> ring{};
    assert(ring.capacity() == 4);
    assert(ring.size() == 0);

    for (uint64_t seq = 1; seq <= 4; ++seq) {
        assert(ring.push_from_isr(timestamp(seq)));
    }
    assert(ring.size() == 4);
    assert(!ring.push_from_isr(timestamp(5)));
    assert(ring.overflow_latched());
    assert(ring.overflow_count() == 1);

    for (uint64_t seq = 1; seq <= 4; ++seq) {
        DrdyTimestamp value{};
        assert(ring.pop(value));
        assert(value.event_seq == seq);
        assert(value.device_time_us == timestamp(seq).device_time_us);
    }
    DrdyTimestamp value{};
    assert(!ring.pop(value));
}

void test_wrap_and_overflow_latch_consumption() {
    DrdyTimestampRing<3> ring{};

    for (uint64_t round = 0; round < 20; ++round) {
        const uint64_t base = round * 3;
        for (uint64_t i = 0; i < 3; ++i) {
            assert(ring.push_from_isr(timestamp(base + i)));
        }
        assert(!ring.push_from_isr(timestamp(base + 99)));
        assert(ring.consume_overflow_latched());
        assert(!ring.consume_overflow_latched());

        for (uint64_t i = 0; i < 3; ++i) {
            DrdyTimestamp value{};
            assert(ring.pop(value));
            assert(value.event_seq == base + i);
        }
    }
    assert(ring.overflow_count() == 20);
}

void test_reset_requires_disarmed_producer_and_clears_state() {
    DrdyTimestampRing<2> ring{};
    assert(ring.push_from_isr(timestamp(10)));
    assert(ring.push_from_isr(timestamp(11)));
    assert(!ring.push_from_isr(timestamp(12)));

    ring.reset();

    assert(ring.size() == 0);
    assert(!ring.overflow_latched());
    assert(ring.overflow_count() == 0);
    assert(ring.push_from_isr(timestamp(20)));
    DrdyTimestamp value{};
    assert(ring.pop(value));
    assert(value.event_seq == 20);
}

}  // namespace

int main() {
    test_fifo_order_and_capacity();
    test_wrap_and_overflow_latch_consumption();
    test_reset_requires_disarmed_producer_and_clears_state();
    return 0;
}
