#include "config/config_store_stub.h"

bool StubConfigStore::load(PersistentConfig& config) {
    if (!has_saved_config_) {
        return false;
    }

    config = saved_;
    return true;
}

bool StubConfigStore::save(const PersistentConfig& config) {
    saved_ = config;
    has_saved_config_ = true;
    return true;
}