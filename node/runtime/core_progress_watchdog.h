#pragma once

#include <cstdint>

namespace runtime {

// Core 0 uses this gate before feeding the hardware watchdog. Core 1 owns the
// wrapping heartbeat. A stalled communication core therefore causes an
// automatic reboot instead of leaving an unreachable node powered forever.
class CoreProgressWatchdogGate {
public:
    CoreProgressWatchdogGate(uint64_t started_us, uint64_t grace_us)
        : deadline_us_(saturating_add(started_us, grace_us)),
          grace_us_(grace_us), valid_(grace_us > 0) {}

    bool should_feed(uint32_t heartbeat, uint64_t now_us) {
        if (!valid_) return false;
        if (!initialized_ || heartbeat != last_heartbeat_) {
            initialized_ = true;
            last_heartbeat_ = heartbeat;
            deadline_us_ = saturating_add(now_us, grace_us_);
            return true;
        }
        return now_us <= deadline_us_;
    }

    bool valid() const { return valid_; }
    uint64_t deadline_us() const { return deadline_us_; }

private:
    static uint64_t saturating_add(uint64_t left, uint64_t right) {
        return UINT64_MAX - left < right ? UINT64_MAX : left + right;
    }

    uint64_t deadline_us_ = 0;
    uint64_t grace_us_ = 0;
    uint32_t last_heartbeat_ = 0;
    bool valid_ = false;
    bool initialized_ = false;
};

}  // namespace runtime
