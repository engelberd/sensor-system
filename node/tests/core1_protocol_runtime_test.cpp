#include <cassert>
#include <cstddef>
#include <cstdint>

#include "runtime/core1_protocol_runtime.h"

namespace {

struct CallTrace {
    char calls[16]{};
    size_t size = 0;

    void push(char value) {
        assert(size < sizeof(calls));
        calls[size++] = value;
    }
};

class FakeMeasurementRuntime {
public:
    explicit FakeMeasurementRuntime(CallTrace& trace) : trace_(trace) {}

    runtime::IngressDrainStatus poll_ingress(size_t budget) {
        trace_.push('M');
        last_budget = budget;
        ++poll_count;
        return result;
    }

    size_t last_budget = 0;
    uint32_t poll_count = 0;
    runtime::IngressDrainStatus result = runtime::IngressDrainStatus::Progress;

private:
    CallTrace& trace_;
};

class FakeLifecycleService {
public:
    explicit FakeLifecycleService(CallTrace& trace) : trace_(trace) {}

    bool poll_result() {
        trace_.push('L');
        ++poll_count;
        const bool value = result_available;
        result_available = false;
        return value;
    }

    bool result_available = false;
    uint32_t poll_count = 0;

private:
    CallTrace& trace_;
};

class FakeTransportEndpoint {
public:
    explicit FakeTransportEndpoint(CallTrace& trace) : trace_(trace) {}

    void update(uint64_t now_us) {
        trace_.push('T');
        last_now_us = now_us;
        ++update_count;
    }

    uint64_t last_now_us = 0;
    uint32_t update_count = 0;

private:
    CallTrace& trace_;
};

class FakeObserver {
public:
    explicit FakeObserver(CallTrace& trace) : trace_(trace) {}

    void update() {
        trace_.push('O');
        ++update_count;
    }

    uint32_t update_count = 0;

private:
    CallTrace& trace_;
};

using Runtime = runtime::Core1ProtocolRuntime<
    FakeMeasurementRuntime,
    FakeLifecycleService,
    FakeTransportEndpoint,
    FakeObserver>;

void test_one_poll_services_every_owner_in_fixed_order() {
    CallTrace trace;
    FakeMeasurementRuntime measurement(trace);
    FakeLifecycleService lifecycle(trace);
    FakeTransportEndpoint transport(trace);
    FakeObserver observer(trace);
    lifecycle.result_available = true;
    Runtime core1(measurement, lifecycle, transport, observer, {7});

    assert(core1.poll(1234) == runtime::Core1ProtocolPollStatus::Ok);
    assert(trace.size == 4);
    assert(trace.calls[0] == 'M');
    assert(trace.calls[1] == 'L');
    assert(trace.calls[2] == 'T');
    assert(trace.calls[3] == 'O');
    assert(measurement.last_budget == 7);
    assert(transport.last_now_us == 1234);
    assert(core1.state().poll_count == 1);
    assert(core1.state().lifecycle_results_consumed == 1);
}

void test_ingress_fault_does_not_hide_transport_or_observer() {
    CallTrace trace;
    FakeMeasurementRuntime measurement(trace);
    FakeLifecycleService lifecycle(trace);
    FakeTransportEndpoint transport(trace);
    FakeObserver observer(trace);
    measurement.result = runtime::IngressDrainStatus::Faulted;
    Runtime core1(measurement, lifecycle, transport, observer, {1});

    assert(core1.poll(55) == runtime::Core1ProtocolPollStatus::Ok);
    assert(core1.state().last_ingress_status ==
           runtime::IngressDrainStatus::Faulted);
    assert(transport.update_count == 1);
    assert(observer.update_count == 1);
}

void test_invalid_policy_executes_nothing() {
    CallTrace trace;
    FakeMeasurementRuntime measurement(trace);
    FakeLifecycleService lifecycle(trace);
    FakeTransportEndpoint transport(trace);
    FakeObserver observer(trace);
    Runtime core1(measurement, lifecycle, transport, observer, {0});

    assert(core1.poll(1) == runtime::Core1ProtocolPollStatus::InvalidPolicy);
    assert(trace.size == 0);
    assert(core1.state().poll_count == 0);
}

void test_lifecycle_counter_advances_only_for_consumed_result() {
    CallTrace trace;
    FakeMeasurementRuntime measurement(trace);
    FakeLifecycleService lifecycle(trace);
    FakeTransportEndpoint transport(trace);
    FakeObserver observer(trace);
    Runtime core1(measurement, lifecycle, transport, observer);

    assert(core1.poll(1) == runtime::Core1ProtocolPollStatus::Ok);
    lifecycle.result_available = true;
    assert(core1.poll(2) == runtime::Core1ProtocolPollStatus::Ok);
    assert(core1.state().poll_count == 2);
    assert(core1.state().lifecycle_results_consumed == 1);
}

}  // namespace

int main() {
    test_one_poll_services_every_owner_in_fixed_order();
    test_ingress_fault_does_not_hide_transport_or_observer();
    test_invalid_policy_executes_nothing();
    test_lifecycle_counter_advances_only_for_consumed_result();
    return 0;
}
