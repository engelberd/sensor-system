#include "diagnostics/persistent_diagnostic_store.h"

#include <cstring>

#include "boot/boot_metadata.h"
#include "hardware/address_mapped.h"
#include "hardware/flash.h"
#include "pico/flash.h"

namespace {

struct FlashWriteContext {
    uint32_t flash_offset;
    const uint8_t* data;
    size_t length;
};

void flash_erase_callback(void* user_data) {
    auto* ctx = static_cast<FlashWriteContext*>(user_data);
    flash_range_erase(ctx->flash_offset, ctx->length);
}

void flash_program_callback(void* user_data) {
    auto* ctx = static_cast<FlashWriteContext*>(user_data);
    flash_range_program(ctx->flash_offset, ctx->data, ctx->length);
}

}  // namespace

const PersistentDiagnosticRecord* PersistentDiagnosticStore::flash_ptr(uint32_t flash_offset) {
    return reinterpret_cast<const PersistentDiagnosticRecord*>(XIP_BASE + flash_offset);
}

bool PersistentDiagnosticStore::is_valid_copy(const PersistentDiagnosticRecord& record) {
    if (record.magic != PERSISTENT_DIAGNOSTIC_MAGIC) {
        return false;
    }

    if (record.version != PERSISTENT_DIAGNOSTIC_VERSION) {
        return false;
    }

    PersistentDiagnosticRecord copy = record;
    const uint32_t stored_crc = copy.crc32;
    copy.crc32 = 0;

    return stored_crc == boot::boot_crc32(&copy, sizeof(PersistentDiagnosticRecord));
}

bool PersistentDiagnosticStore::load_copy(uint32_t flash_offset, PersistentDiagnosticRecord& record) {
    const PersistentDiagnosticRecord* current = flash_ptr(flash_offset);
    if (!is_valid_copy(*current)) {
        return false;
    }

    record = *current;
    return true;
}

bool PersistentDiagnosticStore::load(PersistentDiagnosticRecord& record) const {
    PersistentDiagnosticRecord a{};
    PersistentDiagnosticRecord b{};
    const bool a_valid = load_copy(COPY0_OFFSET, a);
    const bool b_valid = load_copy(COPY1_OFFSET, b);

    if (!a_valid && !b_valid) {
        return false;
    }

    if (a_valid && !b_valid) {
        record = a;
        return true;
    }

    if (!a_valid && b_valid) {
        record = b;
        return true;
    }

    record = (a.generation >= b.generation) ? a : b;
    return true;
}

bool PersistentDiagnosticStore::write_copy(uint32_t flash_offset, const PersistentDiagnosticRecord& record) {
    alignas(FLASH_PAGE_BYTES) uint8_t page_buffer[FLASH_PAGE_BYTES] = {};

    static_assert(sizeof(PersistentDiagnosticRecord) <= FLASH_PAGE_BYTES,
                  "PersistentDiagnosticRecord must fit in one flash page");

    std::memcpy(page_buffer, &record, sizeof(PersistentDiagnosticRecord));

    FlashWriteContext erase_ctx{
        .flash_offset = flash_offset,
        .data = nullptr,
        .length = FLASH_SECTOR_BYTES
    };

    FlashWriteContext prog_ctx{
        .flash_offset = flash_offset,
        .data = page_buffer,
        .length = FLASH_PAGE_BYTES
    };

    const int erase_rc = flash_safe_execute(flash_erase_callback, &erase_ctx, UINT32_MAX);
    if (erase_rc != PICO_OK) {
        return false;
    }

    const int prog_rc = flash_safe_execute(flash_program_callback, &prog_ctx, UINT32_MAX);

    if (prog_rc != PICO_OK) {
        return false;
    }

    const PersistentDiagnosticRecord* verify = flash_ptr(flash_offset);
    return is_valid_copy(*verify) && verify->generation == record.generation;
}

bool PersistentDiagnosticStore::erase_copy(uint32_t flash_offset) {
    FlashWriteContext erase_ctx{
        .flash_offset = flash_offset,
        .data = nullptr,
        .length = FLASH_SECTOR_BYTES
    };

    const int erase_rc = flash_safe_execute(flash_erase_callback, &erase_ctx, UINT32_MAX);

    if (erase_rc != PICO_OK) {
        return false;
    }

    PersistentDiagnosticRecord blank{};
    std::memset(&blank, 0xFF, sizeof(blank));
    const PersistentDiagnosticRecord* verify = flash_ptr(flash_offset);
    return std::memcmp(verify, &blank, sizeof(blank)) == 0;
}

bool PersistentDiagnosticStore::save(const PersistentDiagnosticRecord& record) {
    PersistentDiagnosticRecord prepared = record;
    PersistentDiagnosticRecord current{};
    if (load(current)) {
        prepared.generation = current.generation + 1;
    } else {
        prepared.generation = 1;
    }

    prepared.magic = PERSISTENT_DIAGNOSTIC_MAGIC;
    prepared.version = PERSISTENT_DIAGNOSTIC_VERSION;
    prepared.crc32 = 0;
    prepared.crc32 = boot::boot_crc32(&prepared, sizeof(PersistentDiagnosticRecord));

    const PersistentDiagnosticRecord* a = flash_ptr(COPY0_OFFSET);
    const PersistentDiagnosticRecord* b = flash_ptr(COPY1_OFFSET);
    const bool a_valid = is_valid_copy(*a);
    const bool b_valid = is_valid_copy(*b);

    uint32_t target_offset = COPY0_OFFSET;
    if (!a_valid) {
        target_offset = COPY0_OFFSET;
    } else if (!b_valid) {
        target_offset = COPY1_OFFSET;
    } else {
        target_offset = (a->generation <= b->generation) ? COPY0_OFFSET : COPY1_OFFSET;
    }

    return write_copy(target_offset, prepared);
}

bool PersistentDiagnosticStore::clear() {
    return erase_copy(COPY0_OFFSET) && erase_copy(COPY1_OFFSET);
}
