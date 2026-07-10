#include <cassert>
#include <cstdint>

#include "acquisition/fifo_service_policy.h"

namespace {

void test_watermark_uses_axis_entries() {
    assert(FifoServicePolicy::fallback_interval_ms(250, 30) == 51);
    assert(FifoServicePolicy::fallback_interval_ms(125, 30) == 101);
    assert(FifoServicePolicy::fallback_interval_ms(4000, 30) < 8);
}

void test_fallback_is_always_before_fifo_capacity() {
    constexpr uint32_t odrs[] = {125, 250, 500, 1000, 2000, 4000};
    for (const uint32_t odr : odrs) {
        const uint32_t full_ms = FifoServicePolicy::ceil_div(96u * 1000u, odr * 3u);
        for (uint32_t watermark = 3; watermark <= 96; watermark += 3) {
            assert(FifoServicePolicy::fallback_interval_ms(odr, watermark) < full_ms);
        }
    }
}

void test_spurious_irq_is_not_fifo_ready() {
    assert(!FifoServicePolicy::fifo_ready(0x00, 0, 30));
    assert(!FifoServicePolicy::fifo_ready(0x00, 29, 30));
    assert(FifoServicePolicy::fifo_ready(0x00, 30, 30));
    assert(!FifoServicePolicy::fifo_ready(ADXL355::STATUS_FIFO_FULL_MASK, 0, 30));
    assert(!FifoServicePolicy::fifo_ready(ADXL355::STATUS_FIFO_OVR_MASK, 0, 30));
    assert(FifoServicePolicy::fifo_ready(ADXL355::STATUS_FIFO_FULL_MASK, 3, 30));
    assert(FifoServicePolicy::fifo_ready(ADXL355::STATUS_FIFO_OVR_MASK, 3, 30));
}

void test_only_fifo_int1_mapping_enables_watermark_mode() {
    assert(!FifoServicePolicy::watermark_irq_configured(0));
    assert(!FifoServicePolicy::watermark_irq_configured(ADXL355::INT_RDY_EN1));
    assert(FifoServicePolicy::watermark_irq_configured(ADXL355::INT_FULL_EN1));
    assert(FifoServicePolicy::watermark_irq_configured(ADXL355::INT_OVR_EN1));
}

}  // namespace

int main() {
    test_watermark_uses_axis_entries();
    test_fallback_is_always_before_fifo_capacity();
    test_spurious_irq_is_not_fifo_ready();
    test_only_fifo_int1_mapping_enables_watermark_mode();
    return 0;
}
