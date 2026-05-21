#include "config/config_manager.h"

#include "config/config_crc.h"

namespace {
constexpr uint32_t kSupportedBaudrates[] = {
    9600u,
    19200u,
    38400u,
    57600u,
    115200u,
};

bool is_supported_baudrate(uint32_t baudrate) {
    for (const uint32_t supported : kSupportedBaudrates) {
        if (supported == baudrate) {
            return true;
        }
    }
    return false;
}
}

ConfigManager::ConfigManager(IConfigStore& store)
    : store_(store) {
}

bool ConfigManager::init() {
    PersistentConfig loaded{};
    if (store_.load(loaded) && is_valid(loaded)) {
        current_config_ = loaded;
        current_config_.device = sanitize_device_config(current_config_.device);
        finalize(current_config_);
        return true;
    }

    reset_to_defaults();
    return false;
}

bool ConfigManager::save() {
    PersistentConfig tmp = current_config_;
    ++tmp.generation;
    finalize(tmp);

    if (!store_.save(tmp)) {
        return false;
    }

    current_config_ = tmp;
    return true;
}

bool ConfigManager::reload() {
    PersistentConfig loaded{};
    if (!store_.load(loaded)) {
        return false;
    }

    if (!is_valid(loaded)) {
        return false;
    }

    current_config_ = loaded;
    current_config_.device = sanitize_device_config(current_config_.device);
    finalize(current_config_);
    return true;
}

bool ConfigManager::load_device_config(DeviceConfig& config) {
    PersistentConfig loaded{};
    if (!store_.load(loaded)) {
        return false;
    }

    if (!is_valid(loaded)) {
        return false;
    }

    config = sanitize_device_config(loaded.device);
    return true;
}

void ConfigManager::reset_to_defaults() {
    current_config_ = {};
    current_config_.magic = PERSISTENT_CONFIG_MAGIC;
    current_config_.version = PERSISTENT_CONFIG_VERSION;
    current_config_.generation = 1;
    current_config_.device = make_default_device_config();
    finalize(current_config_);
}

const DeviceConfig& ConfigManager::current() const {
    return current_config_.device;
}

void ConfigManager::replace_device_config(const DeviceConfig& config) {
    current_config_.device = sanitize_device_config(config);
    finalize(current_config_);
}

DeviceConfig ConfigManager::default_device_config() {
    return make_default_device_config();
}

void ConfigManager::set_node_id(uint8_t node_id) {
    current_config_.device.node_id = node_id;
    finalize(current_config_);
}

void ConfigManager::set_baudrate(uint32_t baudrate) {
    current_config_.device.baudrate = baudrate;
    current_config_.device = sanitize_device_config(current_config_.device);
    finalize(current_config_);
}

void ConfigManager::set_odr(uint16_t odr_hz) {
    current_config_.device.odr_hz = odr_hz;
    finalize(current_config_);
}

void ConfigManager::set_range(uint8_t range_g) {
    current_config_.device.range_g = range_g;
    finalize(current_config_);
}

void ConfigManager::set_high_pass_corner(uint8_t high_pass_corner) {
    current_config_.device.high_pass_corner = high_pass_corner;
    current_config_.device = sanitize_device_config(current_config_.device);
    finalize(current_config_);
}

void ConfigManager::set_offset(int32_t x, int32_t y, int32_t z) {
    current_config_.device.offset_x = x;
    current_config_.device.offset_y = y;
    current_config_.device.offset_z = z;
    finalize(current_config_);
}

void ConfigManager::set_fifo_watermark(uint8_t fifo_watermark) {
    current_config_.device.fifo_watermark = fifo_watermark;
    current_config_.device = sanitize_device_config(current_config_.device);
    finalize(current_config_);
}

DeviceConfig ConfigManager::make_default_device_config() {
    DeviceConfig cfg{};
    cfg.node_id = UNASSIGNED_NODE_ID;
    cfg.baudrate = 115200;
    cfg.odr_hz = 250;
    cfg.range_g = 2;
    cfg.high_pass_corner = 0;
    cfg.offset_x = 0;
    cfg.offset_y = 0;
    cfg.offset_z = 0;
    cfg.act_threshold = 0;
    cfg.act_count = 1;
    cfg.fifo_watermark = 30;
    return cfg;
}

DeviceConfig ConfigManager::sanitize_device_config(DeviceConfig config) {
    if (!is_supported_baudrate(config.baudrate)) {
        config.baudrate = 115200;
    }

    if (config.high_pass_corner > 7) {
        config.high_pass_corner = 0;
    }

    if (config.fifo_watermark < 3) {
        config.fifo_watermark = 3;
    }
    if (config.fifo_watermark > 96) {
        config.fifo_watermark = 96;
    }
    config.fifo_watermark =
        static_cast<uint8_t>(config.fifo_watermark - (config.fifo_watermark % 3));
    if (config.fifo_watermark < 3) {
        config.fifo_watermark = 3;
    }

    return config;
}

void ConfigManager::finalize(PersistentConfig& cfg) {
    cfg.magic = PERSISTENT_CONFIG_MAGIC;
    cfg.version = PERSISTENT_CONFIG_VERSION;
    cfg.crc32 = 0;
    cfg.crc32 = config_crc32(&cfg, sizeof(PersistentConfig));
}

bool ConfigManager::is_valid(const PersistentConfig& cfg) {
    if (cfg.magic != PERSISTENT_CONFIG_MAGIC) {
        return false;
    }

    if (cfg.version != PERSISTENT_CONFIG_VERSION) {
        return false;
    }

    PersistentConfig copy = cfg;
    const uint32_t stored_crc = copy.crc32;
    copy.crc32 = 0;

    const uint32_t computed_crc = config_crc32(&copy, sizeof(PersistentConfig));
    return stored_crc == computed_crc;
}
