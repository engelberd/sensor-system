#include "adxl355/adxl355_driver.h"
#include "adxl355/adxl355_registers.h"

#include "pico/stdlib.h"
#include <cstdlib>

// ============================================================
// Static state
// ============================================================

Adxl355Driver* Adxl355Driver::active_instance_ = nullptr;

// ============================================================
// Construction
// ============================================================

Adxl355Driver::Adxl355Driver(spi_inst_t* spi,
                             uint cs_pin,
                             int drdy_pin,
                             int int1_pin)
    : spi_(spi),
      cs_pin_(cs_pin),
      drdy_pin_(drdy_pin),
      int1_pin_(int1_pin) {
}

// ============================================================
// IAccelerometer
// ============================================================

SensorStatus Adxl355Driver::init() {
    if (spi_ == nullptr) {
        return SensorStatus::InvalidParam;
    }

    gpio_init(cs_pin_);
    gpio_set_dir(cs_pin_, GPIO_OUT);
    cs_deselect();

    if (int1_pin_ >= 0) {
        gpio_init(static_cast<uint>(int1_pin_));
        gpio_set_dir(static_cast<uint>(int1_pin_), GPIO_IN);
        gpio_pull_down(static_cast<uint>(int1_pin_));

        active_instance_ = this;

        gpio_set_irq_enabled_with_callback(
            static_cast<uint>(int1_pin_),
            GPIO_IRQ_EDGE_RISE,
            true,
            &Adxl355Driver::gpio_irq_handler
        );
    }

    // Prefer INT1 when available. DRDY can trigger at ODR rate and is not
    // needed for FIFO watermark mode (which is our default acquisition mode).
    if (drdy_pin_ >= 0 && int1_pin_ < 0) {
        gpio_init(static_cast<uint>(drdy_pin_));
        gpio_set_dir(static_cast<uint>(drdy_pin_), GPIO_IN);
        gpio_pull_up(static_cast<uint>(drdy_pin_));

        active_instance_ = this;

        gpio_set_irq_enabled_with_callback(
            static_cast<uint>(drdy_pin_),
            GPIO_IRQ_EDGE_RISE,
            true,
            &Adxl355Driver::gpio_irq_handler
        );
    }

    initialized_ = true;

    SensorStatus st = reset_device();
    if (st != SensorStatus::Ok) {
        return st;
    }

    return SensorStatus::Ok;
}

SensorStatus Adxl355Driver::check_device() {
    if (!initialized_) {
        return SensorStatus::NotInitialized;
    }

    uint8_t devid_ad = 0;
    uint8_t devid_mst = 0;
    uint8_t partid = 0;

    SensorStatus st = read_register(ADXL355::DEVID_AD, devid_ad);
    if (st != SensorStatus::Ok) return st;

    st = read_register(ADXL355::DEVID_MST, devid_mst);
    if (st != SensorStatus::Ok) return st;

    st = read_register(ADXL355::PARTID, partid);
    if (st != SensorStatus::Ok) return st;

    if (devid_ad != 0xAD || devid_mst != 0x1D || partid != 0xED) {
        return SensorStatus::InvalidDevice;
    }

    return SensorStatus::Ok;
}

SensorStatus Adxl355Driver::configure(const AccelerometerConfig& config) {
    if (!initialized_) {
        return SensorStatus::NotInitialized;
    }

    uint8_t range_bits = 0;
    uint8_t odr_bits = 0;

    switch (config.range_g) {
        case 2: range_bits = ADXL355::RANGE_2G; break;
        case 4: range_bits = ADXL355::RANGE_4G; break;
        case 8: range_bits = ADXL355::RANGE_8G; break;
        default: return SensorStatus::InvalidParam;
    }

    switch (config.odr_hz) {
        case 4000: odr_bits = ADXL355::ODR_4000HZ; break;
        case 2000: odr_bits = ADXL355::ODR_2000HZ; break;
        case 1000: odr_bits = ADXL355::ODR_1000HZ; break;
        case 500:  odr_bits = ADXL355::ODR_500HZ;  break;
        case 250:  odr_bits = ADXL355::ODR_250HZ;  break;
        case 125:  odr_bits = ADXL355::ODR_125HZ;  break;
        default: return SensorStatus::InvalidParam;
    }

    SensorStatus st = enter_standby();
    if (st != SensorStatus::Ok) return st;

    st = set_range_internal(range_bits);
    if (st != SensorStatus::Ok) return st;
    current_range_g_ = config.range_g;

    st = set_odr_internal(odr_bits);
    if (st != SensorStatus::Ok) return st;

    if (config.high_pass_corner == 0) {
        st = set_hpf_disabled();
        if (st != SensorStatus::Ok) return st;
    } else {
        st = set_hpf_corner_internal(config.high_pass_corner);
        if (st != SensorStatus::Ok) return st;
    }

    st = enter_measurement_mode();
    if (st != SensorStatus::Ok) return st;

    return SensorStatus::Ok;
}

SensorStatus Adxl355Driver::read_sample(AccelSample& sample) {
    if (!initialized_) {
        return SensorStatus::NotInitialized;
    }

    uint8_t buffer[9] = {0};

    const SensorStatus st = read_multiple(ADXL355::XDATA3, buffer, sizeof(buffer));
    if (st != SensorStatus::Ok) {
        return st;
    }

    sample.x = decode_20bit_signed(buffer[0], buffer[1], static_cast<uint8_t>((buffer[2] >> 4) & 0x0F));
    sample.y = decode_20bit_signed(buffer[3], buffer[4], static_cast<uint8_t>((buffer[5] >> 4) & 0x0F));
    sample.z = decode_20bit_signed(buffer[6], buffer[7], static_cast<uint8_t>((buffer[8] >> 4) & 0x0F));

    return SensorStatus::Ok;
}

SensorStatus Adxl355Driver::read_average_sample(AccelSample& avg, size_t count) {
    int64_t sx = 0, sy = 0, sz = 0;

    AccelSample s{};
    for (size_t i = 0; i < count; ++i) {
        SensorStatus st = read_sample(s);
        if (st != SensorStatus::Ok) return st;

        sx += s.x;
        sy += s.y;
        sz += s.z;

        sleep_us(1000);
    }

    avg.x = static_cast<int32_t>(sx / count);
    avg.y = static_cast<int32_t>(sy / count);
    avg.z = static_cast<int32_t>(sz / count);

    return SensorStatus::Ok;
}

SensorStatus Adxl355Driver::read_fifo_samples(AccelSample* samples,
                                              size_t max_samples,
                                              size_t& samples_read) {
    samples_read = 0;

    if (!initialized_) {
        return SensorStatus::NotInitialized;
    }

    if (samples == nullptr || max_samples == 0) {
        return SensorStatus::InvalidParam;
    }

    uint8_t fifo_entries = 0;
    SensorStatus st = read_fifo_entries(fifo_entries);
    if (st != SensorStatus::Ok) {
        return st;
    }

    if (fifo_entries < 3) {
        return SensorStatus::NoData;
    }

    const size_t available_samples = fifo_entries / 3;
    const size_t to_read = (available_samples < max_samples) ? available_samples : max_samples;

    if (to_read == 0) {
        return SensorStatus::NoData;
    }

    const size_t bytes_to_read = to_read * 9;
    uint8_t buffer[96 * 3] = {0};

    st = read_fifo_raw(buffer, bytes_to_read);
    if (st != SensorStatus::Ok) {
        return st;
    }

    for (size_t i = 0; i < to_read; ++i) {
        const size_t base = i * 9;

        FifoEntry x_entry{};
        FifoEntry y_entry{};
        FifoEntry z_entry{};

        st = decode_fifo_entry(&buffer[base + 0], x_entry);
        if (st != SensorStatus::Ok) return st;

        st = decode_fifo_entry(&buffer[base + 3], y_entry);
        if (st != SensorStatus::Ok) return st;

        st = decode_fifo_entry(&buffer[base + 6], z_entry);
        if (st != SensorStatus::Ok) return st;

        if (x_entry.is_empty || y_entry.is_empty || z_entry.is_empty) {
            return SensorStatus::NoData;
        }

        if (!x_entry.is_x_axis) {
            return SensorStatus::InvalidSample;
        }

        if (y_entry.is_x_axis || z_entry.is_x_axis) {
            return SensorStatus::InvalidSample;
        }

        samples[i].x = x_entry.value;
        samples[i].y = y_entry.value;
        samples[i].z = z_entry.value;
    }

    samples_read = to_read;
    return SensorStatus::Ok;
}

bool Adxl355Driver::supports_fifo() const {
    return true;
}
bool Adxl355Driver::supports_data_ready_interrupt() const {
    return drdy_pin_ >= 0 || int1_pin_ >= 0;
}

bool Adxl355Driver::consume_data_ready_event() {
    const bool was_set = data_ready_flag_;
    data_ready_flag_ = false;
    return was_set;
}

SensorStatus Adxl355Driver::set_offset(int32_t x, int32_t y, int32_t z) {
    if (!initialized_) {
        return SensorStatus::NotInitialized;
    }

    // ADXL355 offset registers are 16-bit values.
    // We clamp incoming values to signed 16-bit range.
    if (x < -32768) x = -32768;
    if (x >  32767) x =  32767;

    if (y < -32768) y = -32768;
    if (y >  32767) y =  32767;

    if (z < -32768) z = -32768;
    if (z >  32767) z =  32767;

    SensorStatus st = enter_standby();
    if (st != SensorStatus::Ok) return st;

    st = write_offset_axis(ADXL355::OFFSET_X_H, ADXL355::OFFSET_X_L, x);
    if (st != SensorStatus::Ok) return st;

    st = write_offset_axis(ADXL355::OFFSET_Y_H, ADXL355::OFFSET_Y_L, y);
    if (st != SensorStatus::Ok) return st;

    st = write_offset_axis(ADXL355::OFFSET_Z_H, ADXL355::OFFSET_Z_L, z);
    if (st != SensorStatus::Ok) return st;

    st = enter_measurement_mode();
    if (st != SensorStatus::Ok) return st;

    return SensorStatus::Ok;
}

SensorStatus Adxl355Driver::run_self_test(SelfTestResult& r) {

    constexpr size_t N = 32;

    SensorStatus st = enter_standby();
    if (st != SensorStatus::Ok) return st;

    st = set_self_test(false, false);
    if (st != SensorStatus::Ok) return st;

    st = enter_measurement_mode();
    if (st != SensorStatus::Ok) return st;

    sleep_ms(10);

    AccelSample baseline{};
    st = read_average_sample(baseline, N);
    if (st != SensorStatus::Ok) return st;

    st = set_self_test(true, false);
    if (st != SensorStatus::Ok) return st;

    sleep_ms(10);

    AccelSample st1{};
    st = read_average_sample(st1, N);
    if (st != SensorStatus::Ok) return st;

    st = set_self_test(true, true);
    if (st != SensorStatus::Ok) return st;

    sleep_ms(10);

    AccelSample st2{};
    st = read_average_sample(st2, N);
    if (st != SensorStatus::Ok) return st;

    set_self_test(false, false);

    r.baseline = baseline;
    r.st1 = st1;
    r.st2 = st2;

    r.delta.x = st2.x - st1.x;
    r.delta.y = st2.y - st1.y;
    r.delta.z = st2.z - st1.z;

    float scale_g_per_lsb = 3.9e-6f;
    if (current_range_g_ == 4) {
        scale_g_per_lsb = 7.8e-6f;
    } else if (current_range_g_ == 8) {
        scale_g_per_lsb = 15.6e-6f;
    }

    const auto in_range_g = [scale_g_per_lsb](int32_t raw,
                                              float min_g,
                                              float max_g) {
        const float delta_g =
            static_cast<float>(std::abs(raw)) * scale_g_per_lsb;
        return delta_g >= min_g && delta_g <= max_g;
    };

    r.passed =
        in_range_g(r.delta.x, 0.1f, 0.6f) &&
        in_range_g(r.delta.y, 0.1f, 0.6f) &&
        in_range_g(r.delta.z, 0.5f, 3.0f);

    return SensorStatus::Ok;
}

// ============================================================
// ITemperatureSensor
// ============================================================

SensorStatus Adxl355Driver::read_temperature(TemperatureSample& temperature) {
    uint16_t temp_raw = 0;

    SensorStatus st = read_temperature_raw(temp_raw);
    if (st != SensorStatus::Ok) {
        return st;
    }

    temperature.raw = temp_raw;

    // Datasheet approximation
    constexpr float TEMP_AT_25C = 1885.0f;
    constexpr float TEMP_SLOPE  = -9.05f;

    temperature.celsius =
        25.0f + (static_cast<float>(temp_raw) - TEMP_AT_25C) / TEMP_SLOPE;

    return SensorStatus::Ok;
}

// ============================================================
// Public helpers
// ============================================================

SensorStatus Adxl355Driver::read_fifo_entries(uint8_t& entries) {
    SensorStatus st = read_register(ADXL355::FIFO_ENTRIES, entries);
    if (st != SensorStatus::Ok) {
        return st;
    }

    entries &= 0x7F;
    return SensorStatus::Ok;
}

SensorStatus Adxl355Driver::read_status(uint8_t& status) {
    return read_status_register(status);
}

SensorStatus Adxl355Driver::configure_fifo(uint8_t watermark) {
    constexpr uint8_t XYZ_FIFO_ENTRY_COUNT = 3;

    if (watermark < XYZ_FIFO_ENTRY_COUNT) {
        watermark = XYZ_FIFO_ENTRY_COUNT;
    }
    if (watermark > ADXL355::FIFO_SAMPLES_MAX) {
        watermark = ADXL355::FIFO_SAMPLES_MAX;
    }
    watermark =
        static_cast<uint8_t>(watermark - (watermark % XYZ_FIFO_ENTRY_COUNT));
    if (watermark < XYZ_FIFO_ENTRY_COUNT) {
        watermark = XYZ_FIFO_ENTRY_COUNT;
    }

    SensorStatus st = enter_standby();
    if (st != SensorStatus::Ok) return st;

    st = write_register(
        ADXL355::FIFO_SAMPLES,
        static_cast<uint8_t>(watermark & ADXL355::FIFO_SAMPLES_MASK)
    );
    if (st != SensorStatus::Ok) return st;

    uint8_t int_map = 0;
    if (int1_pin_ >= 0) {
        st = set_int1_active_high_internal();
        if (st != SensorStatus::Ok) return st;

        int_map = ADXL355::INT_FULL_EN1 | ADXL355::INT_OVR_EN1;
    }

    st = write_register(ADXL355::INT_MAP, int_map);
    if (st != SensorStatus::Ok) return st;

    return enter_measurement_mode();
}

void Adxl355Driver::gpio_irq_handler(uint gpio, uint32_t events) {
    (void)events;

    if (active_instance_ == nullptr) {
        return;
    }

    if (active_instance_->drdy_pin_ < 0 && active_instance_->int1_pin_ < 0) {
        return;
    }

    if (gpio == static_cast<uint>(active_instance_->drdy_pin_) ||
        gpio == static_cast<uint>(active_instance_->int1_pin_)) {
        active_instance_->data_ready_flag_ = true;
    }
}

// ============================================================
// SPI helpers
// ============================================================

void Adxl355Driver::cs_select() {
    gpio_put(cs_pin_, 0);
}

void Adxl355Driver::cs_deselect() {
    gpio_put(cs_pin_, 1);
}

SensorStatus Adxl355Driver::read_register(uint8_t reg, uint8_t& value) {
    if (!initialized_) {
        return SensorStatus::NotInitialized;
    }

    uint8_t tx[2] = {0, 0};
    uint8_t rx[2] = {0, 0};

    tx[0] = static_cast<uint8_t>((reg << 1) | 0x01);
    tx[1] = 0x00;

    cs_select();
    int transferred = spi_write_read_blocking(spi_, tx, rx, 2);
    cs_deselect();

    if (transferred != 2) {
        return SensorStatus::CommError;
    }

    value = rx[1];
    return SensorStatus::Ok;
}

SensorStatus Adxl355Driver::write_register(uint8_t reg, uint8_t value) {
    if (!initialized_) {
        return SensorStatus::NotInitialized;
    }

    uint8_t tx[2] = {0, 0};

    tx[0] = static_cast<uint8_t>((reg << 1) & 0xFE);
    tx[1] = value;

    cs_select();
    int transferred = spi_write_blocking(spi_, tx, 2);
    cs_deselect();

    if (transferred != 2) {
        return SensorStatus::CommError;
    }

    return SensorStatus::Ok;
}

SensorStatus Adxl355Driver::read_multiple(uint8_t start_reg,
                                          uint8_t* buffer,
                                          size_t length) {
    if (!initialized_) {
        return SensorStatus::NotInitialized;
    }

    if (buffer == nullptr || length == 0) {
        return SensorStatus::InvalidParam;
    }

    const uint8_t command = static_cast<uint8_t>((start_reg << 1) | 0x01);

    cs_select();

    int written = spi_write_blocking(spi_, &command, 1);
    if (written != 1) {
        cs_deselect();
        return SensorStatus::CommError;
    }

    int read = spi_read_blocking(spi_, 0x00, buffer, length);
    cs_deselect();

    if (read != static_cast<int>(length)) {
        return SensorStatus::CommError;
    }

    return SensorStatus::Ok;
}

// ============================================================
// FIFO helpers
// ============================================================

SensorStatus Adxl355Driver::read_fifo_raw(uint8_t* buffer, size_t length) {
    if (!initialized_) {
        return SensorStatus::NotInitialized;
    }

    if (buffer == nullptr || length == 0) {
        return SensorStatus::InvalidParam;
    }

    return read_multiple(ADXL355::FIFO_DATA, buffer, length);
}

SensorStatus Adxl355Driver::decode_fifo_entry(const uint8_t* data,
                                              FifoEntry& entry) {
    if (data == nullptr) {
        return SensorStatus::InvalidParam;
    }

    const uint8_t byte0 = data[0];
    const uint8_t byte1 = data[1];
    const uint8_t byte2 = data[2];

    entry.is_empty = (byte2 & 0x02) != 0;
    entry.is_x_axis = (byte2 & 0x01) != 0;

    const uint8_t lsb_nibble = static_cast<uint8_t>((byte2 >> 4) & 0x0F);
    entry.value = decode_20bit_signed(byte0, byte1, lsb_nibble);

    return SensorStatus::Ok;
}

int32_t Adxl355Driver::decode_20bit_signed(uint8_t msb,
                                           uint8_t mid,
                                           uint8_t lsb_nibble) {
    uint32_t raw =
        (static_cast<uint32_t>(msb) << 12) |
        (static_cast<uint32_t>(mid) << 4) |
        (static_cast<uint32_t>(lsb_nibble) & 0x0F);

    if (raw & 0x80000) {
        raw |= 0xFFF00000;
    }

    return static_cast<int32_t>(raw);
}

// ============================================================
// Temperature helpers
// ============================================================

SensorStatus Adxl355Driver::read_temperature_raw(uint16_t& temp_raw) {
    uint8_t buffer[2] = {0, 0};

    SensorStatus st = read_multiple(ADXL355::TEMP2, buffer, 2);
    if (st != SensorStatus::Ok) {
        return st;
    }

    temp_raw =
        (static_cast<uint16_t>(buffer[0] & 0x0F) << 8) |
        static_cast<uint16_t>(buffer[1]);

    return SensorStatus::Ok;
}

SensorStatus Adxl355Driver::read_shadow_registers(uint8_t* values,
                                                  size_t length) {
    if (values == nullptr || length < ADXL355::SHADOW_REG_COUNT) {
        return SensorStatus::InvalidParam;
    }

    return read_multiple(ADXL355::SHADOW_REG1, values, ADXL355::SHADOW_REG_COUNT);
}

// ============================================================
// Internal config helpers
// ============================================================

SensorStatus Adxl355Driver::reset_device() {
    uint8_t expected_shadow[ADXL355::SHADOW_REG_COUNT]{};
    SensorStatus st = read_shadow_registers(expected_shadow, sizeof(expected_shadow));
    if (st != SensorStatus::Ok) {
        return st;
    }

    for (size_t attempt = 0; attempt < 3; ++attempt) {
        st = write_register(ADXL355::RESET, ADXL355::RESET_CODE);
        if (st != SensorStatus::Ok) {
            return st;
        }

        sleep_ms(10);

        uint8_t actual_shadow[ADXL355::SHADOW_REG_COUNT]{};
        st = read_shadow_registers(actual_shadow, sizeof(actual_shadow));
        if (st != SensorStatus::Ok) {
            return st;
        }

        bool shadow_matches = true;
        for (size_t i = 0; i < ADXL355::SHADOW_REG_COUNT; ++i) {
            if (actual_shadow[i] != expected_shadow[i]) {
                shadow_matches = false;
                break;
            }
        }

        if (shadow_matches) {
            return SensorStatus::Ok;
        }
    }

    return SensorStatus::InternalError;
}

SensorStatus Adxl355Driver::read_status_register(uint8_t& status) {
    return read_register(ADXL355::STATUS, status);
}

SensorStatus Adxl355Driver::enter_standby() {
    uint8_t value = 0;

    SensorStatus st = read_register(ADXL355::POWER_CTL, value);
    if (st != SensorStatus::Ok) {
        return st;
    }

    value |= ADXL355::POWER_STANDBY;
    return write_register(ADXL355::POWER_CTL, value);
}

SensorStatus Adxl355Driver::enter_measurement_mode() {
    uint8_t value = 0;

    SensorStatus st = read_register(ADXL355::POWER_CTL, value);
    if (st != SensorStatus::Ok) {
        return st;
    }

    value &= static_cast<uint8_t>(~ADXL355::POWER_STANDBY);
    return write_register(ADXL355::POWER_CTL, value);
}

SensorStatus Adxl355Driver::set_range_internal(uint8_t range_bits) {
    uint8_t value = 0;

    SensorStatus st = read_register(ADXL355::RANGE, value);
    if (st != SensorStatus::Ok) {
        return st;
    }

    value &= 0xFC;
    value |= (range_bits & 0x03);

    return write_register(ADXL355::RANGE, value);
}

SensorStatus Adxl355Driver::set_odr_internal(uint8_t odr_bits) {
    uint8_t value = 0;

    SensorStatus st = read_register(ADXL355::FILTER, value);
    if (st != SensorStatus::Ok) {
        return st;
    }

    value &= static_cast<uint8_t>(~ADXL355::ODR_LPF_MASK);
    value |= (odr_bits & ADXL355::ODR_LPF_MASK);

    return write_register(ADXL355::FILTER, value);
}

SensorStatus Adxl355Driver::set_int1_active_high_internal() {
    uint8_t value = 0;

    SensorStatus st = read_register(ADXL355::RANGE, value);
    if (st != SensorStatus::Ok) {
        return st;
    }

    value |= ADXL355::RANGE_INT_POL_MASK;
    return write_register(ADXL355::RANGE, value);
}

SensorStatus Adxl355Driver::set_self_test(bool st1, bool st2) {
    uint8_t val = 0;
    if (st1) val |= 0x01;
    if (st2) val |= 0x02;
    return write_register(ADXL355::SELF_TEST, val);
}

SensorStatus Adxl355Driver::set_hpf_disabled() {
    uint8_t value = 0;

    SensorStatus st = read_register(ADXL355::FILTER, value);
    if (st != SensorStatus::Ok) {
        return st;
    }

    value &= static_cast<uint8_t>(~ADXL355::HPF_CORNER_MASK);
    return write_register(ADXL355::FILTER, value);
}

SensorStatus Adxl355Driver::set_hpf_corner_internal(uint8_t high_pass_corner) {
    if (high_pass_corner > 7) {
        return SensorStatus::InvalidParam;
    }

    uint8_t value = 0;

    SensorStatus st = read_register(ADXL355::FILTER, value);
    if (st != SensorStatus::Ok) {
        return st;
    }

    value &= static_cast<uint8_t>(~ADXL355::HPF_CORNER_MASK);
    value |= static_cast<uint8_t>(
        (high_pass_corner << ADXL355::HPF_CORNER_POS) & ADXL355::HPF_CORNER_MASK
    );
    return write_register(ADXL355::FILTER, value);
}

SensorStatus Adxl355Driver::write_offset_axis(uint8_t reg_h, uint8_t reg_l, int32_t value_20bit) {
    const int16_t v = static_cast<int16_t>(value_20bit);

    SensorStatus st = write_register(reg_h, static_cast<uint8_t>((static_cast<uint16_t>(v) >> 8) & 0xFF));
    if (st != SensorStatus::Ok) return st;

    st = write_register(reg_l, static_cast<uint8_t>(static_cast<uint16_t>(v) & 0xFF));
    if (st != SensorStatus::Ok) return st;

    return SensorStatus::Ok;
}
