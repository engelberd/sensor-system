#pragma once

#include "config/persistent_config.h"

class IConfigStore {
public:
    virtual ~IConfigStore() = default;

    virtual bool load(PersistentConfig& config) = 0;
    virtual bool save(const PersistentConfig& config) = 0;
};