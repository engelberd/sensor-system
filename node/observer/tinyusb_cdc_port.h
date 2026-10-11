#pragma once

#include "interfaces/i_usb_observer_port.h"

class TinyUsbCdcPort final : public IUsbObserverPort {
public:
    void poll() override;
    bool connected() const override;
    bool read_byte(uint8_t& byte) override;
    size_t write_some(const uint8_t* data, size_t size) override;
};
