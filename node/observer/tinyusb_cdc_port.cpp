#include "observer/tinyusb_cdc_port.h"

#include <algorithm>

#include "tusb.h"

void TinyUsbCdcPort::poll() {
    tud_task();
}

bool TinyUsbCdcPort::connected() const {
    return tud_cdc_connected();
}

bool TinyUsbCdcPort::read_byte(uint8_t& byte) {
    if (!connected() || tud_cdc_available() == 0) {
        return false;
    }

    return tud_cdc_read(&byte, 1) == 1;
}

size_t TinyUsbCdcPort::write_some(const uint8_t* data, size_t size) {
    if (!connected() || data == nullptr || size == 0) {
        return 0;
    }

    const size_t available = tud_cdc_write_available();
    const size_t write_size = std::min(size, available);
    if (write_size == 0) {
        return 0;
    }

    const size_t written = tud_cdc_write(data, write_size);
    if (written > 0) {
        (void)tud_cdc_write_flush();
    }
    return written;
}
