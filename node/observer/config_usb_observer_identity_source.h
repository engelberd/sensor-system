#pragma once

#include "config/config_manager.h"
#include "observer/usb_observer_protocol.h"

class ConfigUsbObserverIdentitySource final : public IUsbObserverIdentitySource {
public:
    ConfigUsbObserverIdentitySource(const ConfigManager& config_manager,
                                    const UsbObserverIdentity& stable_identity)
        : config_manager_(config_manager), stable_identity_(stable_identity) {}

    UsbObserverIdentity current() const override {
        UsbObserverIdentity identity = stable_identity_;
        identity.node_id = config_manager_.current().node_id;
        return identity;
    }

private:
    const ConfigManager& config_manager_;
    UsbObserverIdentity stable_identity_{};
};
