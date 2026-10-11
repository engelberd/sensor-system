#pragma once

#include <cstddef>
#include <cstdint>

class IUsbObserverPort {
public:
    virtual ~IUsbObserverPort() = default;

    virtual void poll() = 0;
    virtual bool connected() const = 0;
    virtual bool read_byte(uint8_t& byte) = 0;
    virtual size_t write_some(const uint8_t* data, size_t size) = 0;
};
