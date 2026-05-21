#pragma once

#include <cstdint>

#include "common/device_config.h"
#include "config/config_store.h"
#include "config/persistent_config.h"

class ConfigManager {
public:
    explicit ConfigManager(IConfigStore& store);

    bool init();
    bool save();
    bool reload();
    bool load_device_config(DeviceConfig& config);

    void reset_to_defaults();

    const DeviceConfig& current() const;
    void replace_device_config(const DeviceConfig& config);

    static DeviceConfig default_device_config();

    void set_node_id(uint8_t node_id);
    void set_baudrate(uint32_t baudrate);
    void set_odr(uint16_t odr_hz);
    void set_range(uint8_t range_g);
    void set_high_pass_corner(uint8_t high_pass_corner);
    void set_offset(int32_t x, int32_t y, int32_t z);
    void set_fifo_watermark(uint8_t fifo_watermark);

private:
    static DeviceConfig make_default_device_config();
    static DeviceConfig sanitize_device_config(DeviceConfig config);

    static void finalize(PersistentConfig& cfg);
    static bool is_valid(const PersistentConfig& cfg);

private:
    IConfigStore& store_;
    PersistentConfig current_config_{};
};
