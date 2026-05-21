#pragma once

#include "boot/boot_metadata.h"

namespace boot {

// Does not return if successful.
[[noreturn]] void boot_jump_to_slot(SlotId slot);

} // namespace boot