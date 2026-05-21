#pragma once

#include <cstddef>
#include <cstdint>

#include "hardware/spi.h"

#include "common/sensor_types.h"
#include "interfaces/i_accelerometer.h"
#include "interfaces/i_temperature_sensor.h"

class Adxl355Driver : public IAccelerometer, public ITemperatureSensor {
public:
    explicit Adxl355Driver(spi_inst_t* spi,
                           uint cs_pin,
                           int drdy_pin = -1,
                           int int1_pin = -1);

    SensorStatus init() override;
    SensorStatus check_device() override;
    SensorStatus configure(const AccelerometerConfig& config) override;
    SensorStatus read_sample(AccelSample& sample) override;
    SensorStatus read_fifo_samples(AccelSample* samples,
                                   size_t max_samples,
                                   size_t& samples_read) override;

    bool supports_fifo() const override;
    SensorStatus configure_fifo(uint8_t watermark) override;

    bool supports_data_ready_interrupt() const override;
    bool consume_data_ready_event() override;
    SensorStatus read_status(uint8_t& status) override;

    SensorStatus set_offset(int32_t x, int32_t y, int32_t z) override;
    SensorStatus run_self_test(SelfTestResult& result) override;

    SensorStatus read_temperature(TemperatureSample& temperature) override;

    SensorStatus read_fifo_entries(uint8_t& entries);

private:
    struct FifoEntry {
        int32_t value = 0;
        bool is_x_axis = false;
        bool is_empty = false;
    };

private:
    spi_inst_t* spi_;
    uint cs_pin_;
    int drdy_pin_;
    int int1_pin_;
    bool initialized_ = false;
    uint8_t current_range_g_ = 2;

    static Adxl355Driver* active_instance_;
    volatile bool data_ready_flag_ = false;

    static void gpio_irq_handler(uint gpio, uint32_t events);

    

private:
    void cs_select();
    void cs_deselect();

    SensorStatus read_register(uint8_t reg, uint8_t& value);
    SensorStatus write_register(uint8_t reg, uint8_t value);
    SensorStatus read_multiple(uint8_t start_reg, uint8_t* buffer, size_t length);

    SensorStatus read_fifo_raw(uint8_t* buffer, size_t length);
    SensorStatus decode_fifo_entry(const uint8_t* data, FifoEntry& entry);
    int32_t decode_20bit_signed(uint8_t msb, uint8_t mid, uint8_t lsb_nibble);

    SensorStatus read_temperature_raw(uint16_t& temp_raw);
    SensorStatus read_shadow_registers(uint8_t* values, size_t length);

    SensorStatus reset_device();
    SensorStatus read_status_register(uint8_t& status);

    SensorStatus set_self_test(bool st1, bool st2);
    SensorStatus read_average_sample(AccelSample& avg, size_t count);

    SensorStatus enter_standby();
    SensorStatus enter_measurement_mode();

    SensorStatus set_range_internal(uint8_t range_bits);
    SensorStatus set_odr_internal(uint8_t odr_bits);
    SensorStatus set_int1_active_high_internal();
    SensorStatus set_hpf_disabled();
    SensorStatus set_hpf_corner_internal(uint8_t high_pass_corner);
    SensorStatus write_offset_axis(uint8_t reg_h, uint8_t reg_l, int32_t value_16bit);
};
