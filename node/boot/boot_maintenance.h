#pragma once

#include <cstddef>
#include <cstdint>

#include "boot/boot_metadata.h"

namespace boot {

// ============================================================
// Simple UART-based maintenance console over RS-485 line
// ============================================================

void boot_maintenance_uart_init();

MaintenanceCommand boot_wait_for_maintenance_command(uint32_t timeout_ms);

// low-level binary access for update mode
bool boot_uart_read_byte_with_timeout(uint8_t& byte, uint32_t timeout_ms);
void boot_uart_write_bytes(const uint8_t* data, size_t length);

// Helper for debug prints from bootloader
void boot_console_puts(const char* text);

} // namespace boot