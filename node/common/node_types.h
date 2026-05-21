#pragma once

#include <cstddef>
#include <cstdint>

enum class NodeState : uint8_t {
    Booting = 0,
    Idle = 1,
    Acquiring = 2,
    Fault = 3,
    Bootloader = 4
};

struct BufferState {
    uint64_t oldest_seq = 0;
    uint64_t newest_seq = 0;

    size_t capacity_samples = 0;
    size_t stored_samples = 0;

    uint32_t overwrite_count = 0;
};