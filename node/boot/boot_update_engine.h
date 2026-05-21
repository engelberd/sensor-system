#pragma once

#include <cstddef>
#include <cstdint>

#include "boot/boot_metadata.h"
#include "boot/boot_update_protocol.h"

namespace boot {

enum class UpdateEngineState : uint8_t {
    Idle = 0,
    WaitingBegin,
    ReceivingImage,
    Completed,
    Failed,
    Aborted
};

class BootUpdateEngine {
public:
    static constexpr size_t STAGING_BUFFER_SIZE = 1024u;

    BootUpdateEngine();

    void reset();

    SlotId preview_target_slot(const BootMetadata& metadata) const;

    UpdateEngineState state() const {
        return state_;
    }

    SlotId target_slot() const {
        return target_slot_;
    }

    UpdateStatus begin(const BootMetadata& current_metadata,
                       uint32_t image_size,
                       uint32_t image_crc32,
                       uint32_t image_version);

    UpdateStatus write_chunk(uint32_t offset,
                             const uint8_t* data,
                             uint16_t flash_length,
                             uint16_t valid_length);

    UpdateStatus finish(BootMetadata& metadata,
                        uint32_t expected_crc32);

    void abort();

private:
    bool erase_target_slot();
    bool program_region(uint32_t flash_offset,
                        const uint8_t* data,
                        size_t length);
    bool flash_region_matches(uint32_t flash_offset,
                              const uint8_t* data,
                              size_t length) const;

    SlotId choose_target_slot(const BootMetadata& metadata) const;

    UpdateEngineState state_ = UpdateEngineState::Idle;

    SlotId target_slot_ = SlotId::None;
    uint32_t target_slot_offset_ = 0;
    uint32_t target_slot_size_ = 0;

    uint32_t expected_image_size_ = 0;
    uint32_t expected_image_crc32_ = 0;
    uint32_t image_version_ = 0;

    uint32_t received_valid_bytes_ = 0;
    uint32_t programmed_flash_bytes_ = 0;

    alignas(256) uint8_t staging_buffer_[STAGING_BUFFER_SIZE]{};
};

} // namespace boot
