#include "boot/boot_decision.h"

namespace boot {
namespace {

SlotId other_slot(SlotId slot) {
    switch (slot) {
        case SlotId::A:
            return SlotId::B;
        case SlotId::B:
            return SlotId::A;
        case SlotId::None:
        default:
            return SlotId::None;
    }
}

SlotId select_default_bootable_slot(const BootMetadata& metadata) {
    if (boot_slot_is_bootable(metadata, metadata.boot_slot)) {
        return metadata.boot_slot;
    }

    if (boot_slot_is_bootable(metadata, metadata.active_slot)) {
        return metadata.active_slot;
    }

    const SlotId fallback = other_slot(metadata.active_slot);
    if (boot_slot_is_bootable(metadata, fallback)) {
        return fallback;
    }

    if (boot_slot_is_bootable(metadata, SlotId::A)) {
        return SlotId::A;
    }

    if (boot_slot_is_bootable(metadata, SlotId::B)) {
        return SlotId::B;
    }

    return SlotId::None;
}

BootDecision maintenance_or_halt(BootError error) {
    BootDecision d{};
    d.mode = BootMode::Maintenance;
    d.slot = SlotId::None;
    d.error = error;
    d.metadata_changed = false;
    return d;
}

} // namespace

BootDecision boot_decide_next(BootMetadata& metadata,
                              MaintenanceCommand command) {
    BootDecision decision{};

    if (!boot_metadata_is_valid(metadata)) {
        decision.mode = BootMode::Maintenance;
        decision.slot = SlotId::None;
        decision.error = BootError::InvalidMetadata;
        decision.metadata_changed = false;
        return decision;
    }

    switch (command) {
        case MaintenanceCommand::EnterUpdate:
        case MaintenanceCommand::StayInBoot:
            decision.mode = BootMode::Maintenance;
            decision.slot = SlotId::None;
            decision.error = BootError::None;
            return decision;

        case MaintenanceCommand::BootSlotA:
            if (boot_slot_is_bootable(metadata, SlotId::A)) {
                decision.mode = BootMode::Normal;
                decision.slot = SlotId::A;
                decision.error = BootError::None;
                return decision;
            }
            return maintenance_or_halt(BootError::RequestedSlotNotBootable);

        case MaintenanceCommand::BootSlotB:
            if (boot_slot_is_bootable(metadata, SlotId::B)) {
                decision.mode = BootMode::Normal;
                decision.slot = SlotId::B;
                decision.error = BootError::None;
                return decision;
            }
            return maintenance_or_halt(BootError::RequestedSlotNotBootable);

        case MaintenanceCommand::BootSafeA:
            if (boot_slot_is_bootable(metadata, SlotId::A)) {
                decision.mode = BootMode::Safe;
                decision.slot = SlotId::A;
                decision.error = BootError::None;
                return decision;
            }
            return maintenance_or_halt(BootError::RequestedSlotNotBootable);

        case MaintenanceCommand::BootSafeB:
            if (boot_slot_is_bootable(metadata, SlotId::B)) {
                decision.mode = BootMode::Safe;
                decision.slot = SlotId::B;
                decision.error = BootError::None;
                return decision;
            }
            return maintenance_or_halt(BootError::RequestedSlotNotBootable);

        case MaintenanceCommand::BootDefault:
        case MaintenanceCommand::None:
        default:
            break;
    }

    if (metadata.trial_armed != 0u && metadata.trial_slot != SlotId::None) {
        if (!boot_slot_is_bootable(metadata, metadata.trial_slot)) {
            metadata.trial_armed = 0u;
            metadata.trial_attempted = 0u;
            metadata.trial_slot = SlotId::None;
            metadata.last_error = static_cast<uint32_t>(BootError::RequestedSlotNotBootable);

            decision.mode = BootMode::Maintenance;
            decision.slot = SlotId::None;
            decision.error = BootError::RequestedSlotNotBootable;
            decision.metadata_changed = true;
            return decision;
        }

        if (metadata.trial_attempted == 0u) {
            metadata.trial_attempted = 1u;
            metadata.boot_slot = metadata.trial_slot;

            decision.mode = BootMode::Normal;
            decision.slot = metadata.trial_slot;
            decision.error = BootError::None;
            decision.metadata_changed = true;
            return decision;
        }

        SlotMetadata& trial_md = boot_slot_metadata(metadata, metadata.trial_slot);
        ++trial_md.failed_trial_boots;

        const SlotId rollback_slot = metadata.active_slot;
        metadata.trial_armed = 0u;
        metadata.trial_attempted = 0u;
        metadata.trial_slot = SlotId::None;
        metadata.boot_slot = rollback_slot;
        metadata.last_error = static_cast<uint32_t>(BootError::TrialRollback);

        if (boot_slot_is_bootable(metadata, rollback_slot)) {
            decision.mode = BootMode::Normal;
            decision.slot = rollback_slot;
            decision.error = BootError::TrialRollback;
            decision.metadata_changed = true;
            return decision;
        }

        const SlotId fallback = select_default_bootable_slot(metadata);
        if (fallback != SlotId::None) {
            metadata.boot_slot = fallback;
            decision.mode = BootMode::Normal;
            decision.slot = fallback;
            decision.error = BootError::TrialRollback;
            decision.metadata_changed = true;
            return decision;
        }

        decision.mode = BootMode::Maintenance;
        decision.slot = SlotId::None;
        decision.error = BootError::NoBootableSlot;
        decision.metadata_changed = true;
        return decision;
    }

    const SlotId slot = select_default_bootable_slot(metadata);
    if (slot == SlotId::None) {
        decision.mode = BootMode::Maintenance;
        decision.slot = SlotId::None;
        decision.error = BootError::NoBootableSlot;
        decision.metadata_changed = false;
        return decision;
    }

    metadata.boot_slot = slot;

    decision.mode = BootMode::Normal;
    decision.slot = slot;
    decision.error = BootError::None;
    decision.metadata_changed = false;
    return decision;
}

bool boot_mark_trial_pending(BootMetadata& metadata, SlotId slot) {
    if (!boot_slot_is_bootable(metadata, slot)) {
        return false;
    }

    metadata.trial_slot = slot;
    metadata.trial_armed = 1u;
    metadata.trial_attempted = 0u;
    metadata.boot_slot = slot;
    return true;
}

bool boot_confirm_trial_success(BootMetadata& metadata) {
    if (metadata.trial_armed == 0u || metadata.trial_slot == SlotId::None) {
        return false;
    }

    if (!boot_slot_is_bootable(metadata, metadata.trial_slot)) {
        return false;
    }

    metadata.active_slot = metadata.trial_slot;
    metadata.boot_slot = metadata.trial_slot;

    SlotMetadata& sm = boot_slot_metadata(metadata, metadata.trial_slot);
    ++sm.confirmed_boots;

    metadata.trial_slot = SlotId::None;
    metadata.trial_armed = 0u;
    metadata.trial_attempted = 0u;
    metadata.last_error = static_cast<uint32_t>(BootError::None);

    return true;
}

} // namespace boot