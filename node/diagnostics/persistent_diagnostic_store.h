#pragma once

#include "boot/boot_config.h"
#include "diagnostics/diagnostic_types.h"

class PersistentDiagnosticStore {
public:
    bool load(PersistentDiagnosticRecord& record) const;
    bool save(const PersistentDiagnosticRecord& record);
    bool clear();

private:
    static constexpr uint32_t FLASH_SECTOR_BYTES = 4096;
    static constexpr uint32_t FLASH_PAGE_BYTES = 256;
    static constexpr uint32_t COPY0_OFFSET = boot::DIAG_PRIMARY_OFFSET;
    static constexpr uint32_t COPY1_OFFSET = boot::DIAG_SECONDARY_OFFSET;

private:
    static const PersistentDiagnosticRecord* flash_ptr(uint32_t flash_offset);
    static bool is_valid_copy(const PersistentDiagnosticRecord& record);
    static bool load_copy(uint32_t flash_offset, PersistentDiagnosticRecord& record);
    static bool write_copy(uint32_t flash_offset, const PersistentDiagnosticRecord& record);
    static bool erase_copy(uint32_t flash_offset);
};
