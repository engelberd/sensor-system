#pragma once

#include <cstddef>
#include <cstdint>

#include "common/device_identity.h"

enum class UsbObserverCommand : uint8_t {
    GetCapabilities = 0x01,
    ReadSnapshot = 0x02,
    GetStats = 0x03,
};

static constexpr uint8_t USB_OBSERVER_FORMAT_VERSION = 1;
static constexpr size_t USB_OBSERVER_MAX_REQUEST_PAYLOAD = 8;
static constexpr size_t USB_OBSERVER_RX_BUDGET = 32;
static constexpr size_t USB_OBSERVER_TX_BUDGET = 64;
static constexpr size_t USB_OBSERVER_SAMPLE_WIRE_SIZE = 36;

struct UsbObserverIdentity {
    DeviceHardwareId hardware_id{};
    uint8_t node_id = 0;
    uint8_t firmware_major = 0;
    uint8_t firmware_minor = 0;
    uint8_t firmware_patch = 0;
    uint8_t product_protocol_version = 0;
    uint8_t board_revision = 0;
    bool timestamping_enabled = false;
};

class IUsbObserverIdentitySource {
public:
    virtual ~IUsbObserverIdentitySource() = default;
    virtual UsbObserverIdentity current() const = 0;
};
