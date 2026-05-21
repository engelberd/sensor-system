#pragma once

#include <cstdint>

#include "boot/boot_metadata.h"

namespace boot {

// ============================================================
// Error codes for boot decision path
// ============================================================

enum class BootError : uint32_t {
    None = 0,
    InvalidMetadata = 1,
    TrialRollback = 2,
    RequestedSlotNotBootable = 3,
    NoBootableSlot = 4,
    RequestedUpdateTimeout = 5
};

// ============================================================
// Decision returned by boot policy
// ============================================================

struct BootDecision {
    BootMode mode = BootMode::Halt;
    SlotId slot = SlotId::None;
    BootError error = BootError::None;
    bool metadata_changed = false;
};

// ============================================================
// Boot policy
// ============================================================

BootDecision boot_decide_next(BootMetadata& metadata,
                              MaintenanceCommand command);

bool boot_mark_trial_pending(BootMetadata& metadata, SlotId slot);
bool boot_confirm_trial_success(BootMetadata& metadata);

} // namespace boot
