#include "transport/rs485_port.h"

#include "hardware/gpio.h"

Rs485Port::Rs485Port(uart_inst_t* uart,
                     uint tx_pin,
                     uint rx_pin,
                     uint baudrate,
                     int de_pin)
    : uart_(uart),
      tx_pin_(tx_pin),
      rx_pin_(rx_pin),
      baudrate_(baudrate),
      de_pin_(de_pin) {
}

bool Rs485Port::init() {
    if (uart_ == nullptr) {
        return false;
    }

    uart_init(uart_, baudrate_);

    gpio_set_function(tx_pin_, GPIO_FUNC_UART);
    gpio_set_function(rx_pin_, GPIO_FUNC_UART);

    uart_set_format(uart_, 8, 1, UART_PARITY_NONE);
    uart_set_fifo_enabled(uart_, true);

    if (de_pin_ >= 0) {
        gpio_init(static_cast<uint>(de_pin_));
        gpio_set_dir(static_cast<uint>(de_pin_), GPIO_OUT);
        set_driver_enable(false);
    }

    initialized_ = true;
    tx_in_progress_ = false;
    tx_completed_flag_ = false;
    rx_overflow_count_ = 0;

    return true;
}

bool Rs485Port::start_tx(const uint8_t* data, size_t size) {
    if (!initialized_ || data == nullptr || size == 0 || tx_in_progress_) {
        return false;
    }

    tx_completed_flag_ = false;
    tx_in_progress_ = true;
    set_driver_enable(true);

    for (size_t i = 0; i < size; ++i) {
        uart_putc_raw(uart_, data[i]);
    }

    uart_tx_wait_blocking(uart_);
    set_driver_enable(false);
    // Some RS485 paths echo local TX into RX; drop that before command parsing resumes.
    drain_rx();

    tx_in_progress_ = false;
    tx_completed_flag_ = true;
    return true;
}

void Rs485Port::abort_tx() {
    if (!initialized_) return;
    uart_tx_wait_blocking(uart_);
    set_driver_enable(false);
    drain_rx();
    tx_in_progress_ = false;
    tx_completed_flag_ = false;
}

bool Rs485Port::tx_busy() const {
    return tx_in_progress_;
}

bool Rs485Port::tx_completed() const {
    return tx_completed_flag_;
}

void Rs485Port::clear_tx_completed() {
    tx_completed_flag_ = false;
}

void Rs485Port::poll() {
    if (!initialized_) {
        return;
    }

    finish_tx_if_done();
}

bool Rs485Port::write_blocking(const uint8_t* data, size_t size) {
    if (!initialized_ || data == nullptr || size == 0 || tx_in_progress_) {
        return false;
    }

    set_driver_enable(true);

    for (size_t i = 0; i < size; ++i) {
        uart_putc_raw(uart_, data[i]);
    }

    uart_tx_wait_blocking(uart_);
    set_driver_enable(false);
    // Some RS485 paths echo local TX into RX; drop that before command parsing resumes.
    drain_rx();

    tx_completed_flag_ = true;
    return true;
}

bool Rs485Port::read_byte(uint8_t& byte) {
    if (!initialized_) {
        return false;
    }

    if (!uart_is_readable(uart_)) {
        return false;
    }

    byte = static_cast<uint8_t>(uart_getc(uart_));
    return true;
}

void Rs485Port::drain_rx() {
    if (!initialized_) {
        return;
    }

    while (uart_is_readable(uart_)) {
        (void)uart_getc(uart_);
    }
}

uint32_t Rs485Port::rx_overflow_count() const {
    return rx_overflow_count_;
}

void Rs485Port::set_baudrate(uint32_t baudrate) {
    baudrate_ = baudrate;

    if (initialized_) {
        uart_set_baudrate(uart_, baudrate_);
    }
}

uint32_t Rs485Port::baudrate() const {
    return baudrate_;
}

void Rs485Port::set_driver_enable(bool enabled) {
    if (de_pin_ < 0) {
        return;
    }

    gpio_put(static_cast<uint>(de_pin_), enabled ? 1 : 0);
}

void Rs485Port::finish_tx_if_done() {
    if (!tx_in_progress_) {
        return;
    }

    uart_tx_wait_blocking(uart_);
    set_driver_enable(false);
    // Some RS485 paths echo local TX into RX; drop that before command parsing resumes.
    drain_rx();

    tx_in_progress_ = false;
    tx_completed_flag_ = true;
}
