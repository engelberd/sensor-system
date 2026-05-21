#pragma once

#include "config/config_store.h"

class StubConfigStore : public IConfigStore {
public:
    bool load(PersistentConfig& config) override;
    bool save(const PersistentConfig& config) override;

private:
    bool has_saved_config_ = false;
    PersistentConfig saved_{};
};