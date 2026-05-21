#pragma once

#include <cstdint>

#include "boot/boot_config.h"
#include "config/config_store.h"

class FlashConfigStore : public IConfigStore {
public:
    bool load(PersistentConfig& config) override;
    bool save(const PersistentConfig& config) override;

private:
    static constexpr uint32_t FLASH_SECTOR_BYTES = 4096;
    static constexpr uint32_t FLASH_PAGE_BYTES = 256;

    // Dwie redundantne kopie configu.
    // Uwaga: te offsety MUSZĄ wskazywać obszar zarezerwowany dla configu.
    static constexpr uint32_t CONFIG_COPY0_OFFSET = boot::CONFIG_PRIMARY_OFFSET;
    static constexpr uint32_t CONFIG_COPY1_OFFSET = boot::CONFIG_SECONDARY_OFFSET;

private:
    static const PersistentConfig* flash_ptr(uint32_t flash_offset);
    static bool write_copy(uint32_t flash_offset, const PersistentConfig& config);
    static bool is_valid_copy(const PersistentConfig& config);
    static bool load_copy(uint32_t flash_offset, PersistentConfig& config);
};
