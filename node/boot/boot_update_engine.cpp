#include "boot/boot_update_engine.h"

#include <cstring>

#include "boot/boot_config.h"
#include "boot/boot_decision.h"
#include "boot/boot_metadata.h"
#include "hardware/address_mapped.h"
#include "hardware/flash.h"
#include "hardware/sync.h"

namespace boot {
namespace {

static_assert((BootUpdateEngine::STAGING_BUFFER_SIZE % FLASH_PAGE_SIZE) == 0,
              "Staging buffer size must be multiple of flash page size");

uint32_t slot_end_offset(SlotId slot) {
    return boot_slot_offset(slot) + boot_slot_size(slot);
}

const uint8_t* slot_xip_ptr(SlotId slot) {
    return reinterpret_cast<const uint8_t*>(XIP_BASE + boot_slot_offset(slot));
}

} // namespace

BootUpdateEngine::BootUpdateEngine() {
    reset();
}

void BootUpdateEngine::reset() {
    state_ = UpdateEngineState::WaitingBegin;

    target_slot_ = SlotId::None;
    target_slot_offset_ = 0;
    target_slot_size_ = 0;

    expected_image_size_ = 0;
    expected_image_crc32_ = 0;
    image_version_ = 0;

    received_valid_bytes_ = 0;
    programmed_flash_bytes_ = 0;

    std::memset(staging_buffer_, 0xFF, sizeof(staging_buffer_));
}

SlotId BootUpdateEngine::preview_target_slot(const BootMetadata& metadata) const {
    return choose_target_slot(metadata);
}

SlotId BootUpdateEngine::choose_target_slot(const BootMetadata& metadata) const {
    if (metadata.active_slot == SlotId::A) {
        return SlotId::B;
    }

    if (metadata.active_slot == SlotId::B) {
        return SlotId::A;
    }

    return SlotId::B;
}

UpdateStatus BootUpdateEngine::begin(const BootMetadata& current_metadata,
                                     uint32_t image_size,
                                     uint32_t image_crc32,
                                     uint32_t image_version) {
    if (state_ != UpdateEngineState::WaitingBegin &&
        state_ != UpdateEngineState::Idle) {
        return UpdateStatus::BadState;
    }

    const SlotId slot = choose_target_slot(current_metadata);
    if (slot == SlotId::None) {
        state_ = UpdateEngineState::Failed;
        return UpdateStatus::InvalidSlot;
    }

    const uint32_t slot_size = boot_slot_size(slot);
    if (image_size == 0u || image_size > slot_size) {
        state_ = UpdateEngineState::Failed;
        return UpdateStatus::ImageTooLarge;
    }

    target_slot_ = slot;
    target_slot_offset_ = boot_slot_offset(slot);
    target_slot_size_ = slot_size;

    expected_image_size_ = image_size;
    expected_image_crc32_ = image_crc32;
    image_version_ = image_version;

    received_valid_bytes_ = 0;
    programmed_flash_bytes_ = 0;

    if (!erase_target_slot()) {
        state_ = UpdateEngineState::Failed;
        return UpdateStatus::FlashError;
    }

    state_ = UpdateEngineState::ReceivingImage;
    return UpdateStatus::Ok;
}

UpdateStatus BootUpdateEngine::write_chunk(uint32_t offset,
                                           const uint8_t* data,
                                           uint16_t flash_length,
                                           uint16_t valid_length) {
    if (state_ != UpdateEngineState::ReceivingImage) {
        return UpdateStatus::BadState;
    }

    if (data == nullptr) {
        state_ = UpdateEngineState::Failed;
        return UpdateStatus::BadLength;
    }

    if (flash_length == 0u || valid_length == 0u) {
        state_ = UpdateEngineState::Failed;
        return UpdateStatus::BadLength;
    }

    if (valid_length > flash_length) {
        state_ = UpdateEngineState::Failed;
        return UpdateStatus::BadLength;
    }

    if ((flash_length % FLASH_PAGE_SIZE) != 0u) {
        state_ = UpdateEngineState::Failed;
        return UpdateStatus::BadLength;
    }

    if ((offset % FLASH_PAGE_SIZE) != 0u) {
        state_ = UpdateEngineState::Failed;
        return UpdateStatus::BadOffset;
    }

    if (offset != programmed_flash_bytes_) {
        const uint64_t duplicate_end =
            static_cast<uint64_t>(offset) + static_cast<uint64_t>(flash_length);
        if (offset < programmed_flash_bytes_ &&
            duplicate_end <= programmed_flash_bytes_ &&
            flash_region_matches(target_slot_offset_ + offset, data, flash_length)) {
            return UpdateStatus::Ok;
        }

        state_ = UpdateEngineState::Failed;
        return UpdateStatus::BadOffset;
    }

    const uint64_t end_flash =
        static_cast<uint64_t>(offset) + static_cast<uint64_t>(flash_length);

    const uint64_t end_valid =
        static_cast<uint64_t>(received_valid_bytes_) + static_cast<uint64_t>(valid_length);

    if (end_flash > target_slot_size_) {
        state_ = UpdateEngineState::Failed;
        return UpdateStatus::BadLength;
    }

    if (end_valid > expected_image_size_) {
        state_ = UpdateEngineState::Failed;
        return UpdateStatus::BadLength;
    }

    if (!program_region(target_slot_offset_ + offset, data, flash_length)) {
        state_ = UpdateEngineState::Failed;
        return UpdateStatus::FlashError;
    }

    programmed_flash_bytes_ += flash_length;
    received_valid_bytes_ += valid_length;

    return UpdateStatus::Ok;
}

UpdateStatus BootUpdateEngine::finish(BootMetadata& metadata,
                                      uint32_t expected_crc32) {
    if (state_ != UpdateEngineState::ReceivingImage) {
        return UpdateStatus::BadState;
    }

    if (received_valid_bytes_ != expected_image_size_) {
        state_ = UpdateEngineState::Failed;
        return UpdateStatus::BadLength;
    }

    if (expected_crc32 != expected_image_crc32_) {
        state_ = UpdateEngineState::Failed;
        return UpdateStatus::BadCrc;
    }

    const uint8_t* image = slot_xip_ptr(target_slot_);
    const uint32_t computed_crc = boot_crc32(image, expected_image_size_);
    if (computed_crc != expected_image_crc32_) {
        state_ = UpdateEngineState::Failed;
        return UpdateStatus::BadCrc;
    }

    SlotMetadata& sm = boot_slot_metadata(metadata, target_slot_);
    sm.image_size = expected_image_size_;
    sm.image_crc32 = expected_image_crc32_;
    sm.image_version = image_version_;
    sm.image_valid = 1u;
    sm.failed_trial_boots = 0u;
    metadata.boot_flags &= static_cast<uint8_t>(~BOOT_FLAG_ENTER_UPDATE);

    if (!boot_mark_trial_pending(metadata, target_slot_)) {
        state_ = UpdateEngineState::Failed;
        return UpdateStatus::InternalError;
    }

    state_ = UpdateEngineState::Completed;
    return UpdateStatus::Ok;
}

void BootUpdateEngine::abort() {
    state_ = UpdateEngineState::Aborted;
}

bool BootUpdateEngine::erase_target_slot() {
    if (target_slot_ == SlotId::None) {
        return false;
    }

    const uint32_t begin = target_slot_offset_;
    const uint32_t end = slot_end_offset(target_slot_);

    uint32_t irq_state = save_and_disable_interrupts();

    for (uint32_t off = begin; off < end; off += FLASH_SECTOR_SIZE) {
        flash_range_erase(off, FLASH_SECTOR_SIZE);
    }

    restore_interrupts(irq_state);
    return true;
}

bool BootUpdateEngine::program_region(uint32_t flash_offset,
                                      const uint8_t* data,
                                      size_t length) {
    if (data == nullptr || length == 0u) {
        return false;
    }

    if ((flash_offset % FLASH_PAGE_SIZE) != 0u) {
        return false;
    }

    if ((length % FLASH_PAGE_SIZE) != 0u) {
        return false;
    }

    uint32_t irq_state = save_and_disable_interrupts();
    flash_range_program(flash_offset, data, length);
    restore_interrupts(irq_state);

    return true;
}

bool BootUpdateEngine::flash_region_matches(uint32_t flash_offset,
                                            const uint8_t* data,
                                            size_t length) const {
    if (data == nullptr || length == 0u) {
        return false;
    }

    const uint8_t* flash_ptr =
        reinterpret_cast<const uint8_t*>(XIP_BASE + flash_offset);
    return std::memcmp(flash_ptr, data, length) == 0;
}

} // namespace boot
