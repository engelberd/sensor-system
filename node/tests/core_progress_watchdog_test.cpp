#include <cassert>
#include <cstdint>

#include "runtime/core_progress_watchdog.h"

namespace {

void progress_extends_the_feed_window() {
    runtime::CoreProgressWatchdogGate gate(100, 1000);
    assert(gate.valid());
    assert(gate.should_feed(1, 100));
    assert(gate.should_feed(1, 1100));
    assert(!gate.should_feed(1, 1101));
    assert(gate.should_feed(2, 1200));
    assert(gate.deadline_us() == 2200);
}

void heartbeat_wrap_is_progress() {
    runtime::CoreProgressWatchdogGate gate(0, 10);
    assert(gate.should_feed(UINT32_MAX, 1));
    assert(gate.should_feed(0, 2));
    assert(gate.should_feed(0, 12));
    assert(!gate.should_feed(0, 13));
}

void invalid_or_overflowing_policy_fails_safely() {
    runtime::CoreProgressWatchdogGate invalid(0, 0);
    assert(!invalid.valid());
    assert(!invalid.should_feed(1, 0));

    runtime::CoreProgressWatchdogGate saturated(UINT64_MAX - 2, 10);
    assert(saturated.deadline_us() == UINT64_MAX);
    assert(saturated.should_feed(1, UINT64_MAX - 2));
    assert(saturated.deadline_us() == UINT64_MAX);
}

}  // namespace

int main() {
    progress_extends_the_feed_window();
    heartbeat_wrap_is_progress();
    invalid_or_overflowing_policy_fails_safely();
    return 0;
}
