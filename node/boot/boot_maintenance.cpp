#include "boot/boot_maintenance.h"

#include <cstring>

#include "hardware/gpio.h"
#include "hardware/uart.h"
#include "pico/stdlib.h"

namespace boot {
namespace {

uart_inst_t* const BOOT_UART = uart0;
constexpr uint BOOT_UART_TX_PIN = 0;
constexpr uint BOOT_UART_RX_PIN = 1;
constexpr int  BOOT_RS485_DE_PIN = 2;
constexpr uint32_t BOOT_UART_BAUD = 115200;

void rs485_set_rx_mode() {
    if (BOOT_RS485_DE_PIN >= 0) {
        gpio_put(static_cast<uint>(BOOT_RS485_DE_PIN), 0);
    }
}

void rs485_set_tx_mode() {
    if (BOOT_RS485_DE_PIN >= 0) {
        gpio_put(static_cast<uint>(BOOT_RS485_DE_PIN), 1);
    }
}

bool line_to_command(const char* line, MaintenanceCommand& cmd) {
    if (line == nullptr) {
        return false;
    }

    if (std::strcmp(line, "AUTO") == 0) {
        cmd = MaintenanceCommand::BootDefault;
        return true;
    }

    if (std::strcmp(line, "BOOT A") == 0) {
        cmd = MaintenanceCommand::BootSlotA;
        return true;
    }

    if (std::strcmp(line, "BOOT B") == 0) {
        cmd = MaintenanceCommand::BootSlotB;
        return true;
    }

    if (std::strcmp(line, "SAFE A") == 0) {
        cmd = MaintenanceCommand::BootSafeA;
        return true;
    }

    if (std::strcmp(line, "SAFE B") == 0) {
        cmd = MaintenanceCommand::BootSafeB;
        return true;
    }

    if (std::strcmp(line, "UPDATE") == 0) {
        cmd = MaintenanceCommand::EnterUpdate;
        return true;
    }

    if (std::strcmp(line, "STAY") == 0) {
        cmd = MaintenanceCommand::StayInBoot;
        return true;
    }

    return false;
}

} // namespace

void boot_maintenance_uart_init() {
    uart_init(BOOT_UART, BOOT_UART_BAUD);

    gpio_set_function(BOOT_UART_TX_PIN, GPIO_FUNC_UART);
    gpio_set_function(BOOT_UART_RX_PIN, GPIO_FUNC_UART);

    uart_set_format(BOOT_UART, 8, 1, UART_PARITY_NONE);
    uart_set_fifo_enabled(BOOT_UART, true);

    if (BOOT_RS485_DE_PIN >= 0) {
        gpio_init(static_cast<uint>(BOOT_RS485_DE_PIN));
        gpio_set_dir(static_cast<uint>(BOOT_RS485_DE_PIN), GPIO_OUT);
        rs485_set_rx_mode();
    }
}

bool boot_uart_read_byte_with_timeout(uint8_t& byte, uint32_t timeout_ms) {
    const absolute_time_t deadline = make_timeout_time_ms(timeout_ms);

    while (!time_reached(deadline)) {
        if (uart_is_readable(BOOT_UART)) {
            byte = static_cast<uint8_t>(uart_getc(BOOT_UART));
            return true;
        }

        tight_loop_contents();
    }

    return false;
}

void boot_uart_write_bytes(const uint8_t* data, size_t length) {
    if (data == nullptr || length == 0u) {
        return;
    }

    rs485_set_tx_mode();

    for (size_t i = 0; i < length; ++i) {
        uart_putc_raw(BOOT_UART, static_cast<char>(data[i]));
    }

    uart_tx_wait_blocking(BOOT_UART);
    rs485_set_rx_mode();
}

MaintenanceCommand boot_wait_for_maintenance_command(uint32_t timeout_ms) {
    absolute_time_t deadline = make_timeout_time_ms(timeout_ms);

    char line[32]{};
    size_t pos = 0;

    boot_console_puts("\r\nBOOT> maintenance window open\r\n");
    boot_console_puts("Commands: AUTO | BOOT A | BOOT B | SAFE A | SAFE B | UPDATE | STAY\r\n");

    while (!time_reached(deadline)) {
        if (!uart_is_readable(BOOT_UART)) {
            tight_loop_contents();
            continue;
        }

        const char c = static_cast<char>(uart_getc(BOOT_UART));

        if (c == '\r' || c == '\n') {
            if (pos == 0) {
                continue;
            }

            line[pos] = '\0';

            MaintenanceCommand cmd = MaintenanceCommand::None;
            if (line_to_command(line, cmd)) {
                boot_console_puts("BOOT> command accepted\r\n");
                return cmd;
            }

            boot_console_puts("BOOT> unknown command\r\n");
            pos = 0;
            std::memset(line, 0, sizeof(line));
            continue;
        }

        if (pos + 1 < sizeof(line)) {
            if (c >= 32 && c <= 126) {
                line[pos++] = c;
            }
        } else {
            pos = 0;
            std::memset(line, 0, sizeof(line));
            boot_console_puts("BOOT> line too long\r\n");
        }
    }

    return MaintenanceCommand::None;
}

void boot_console_puts(const char* text) {
    if (text == nullptr) {
        return;
    }

    boot_uart_write_bytes(reinterpret_cast<const uint8_t*>(text), std::strlen(text));
}

} // namespace boot
