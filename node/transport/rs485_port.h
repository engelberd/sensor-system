#pragma once

#include <cstddef>
#include <cstdint>

#include "hardware/uart.h"
#include "interfaces/i_transport_diagnostics.h"
#include "pico/stdlib.h"

class Rs485Port : public ITransportDiagnostics {
public:
    Rs485Port(uart_inst_t* uart,
              uint tx_pin,
              uint rx_pin,
              uint baudrate,
              int de_pin = -1);

    bool init();

    bool start_tx(const uint8_t* data, size_t size);
    void abort_tx();
    bool tx_busy() const;
    bool tx_completed() const;
    void clear_tx_completed();
    void poll();
    bool write_blocking(const uint8_t* data, size_t size);

    bool read_byte(uint8_t& byte);
    void drain_rx();
    uint32_t rx_overflow_count() const override;

    void set_baudrate(uint32_t baudrate);
    uint32_t baudrate() const;

private:
    void set_driver_enable(bool enabled);
    void finish_tx_if_done();

private:
    uart_inst_t* uart_;
    uint tx_pin_;
    uint rx_pin_;
    uint32_t baudrate_;
    int de_pin_;

    bool initialized_ = false;

    bool tx_in_progress_ = false;
    bool tx_completed_flag_ = false;

    uint32_t rx_overflow_count_ = 0;
};
