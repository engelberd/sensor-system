#include <cassert>

#include "runtime/operation_mailbox.h"
#include "runtime/sensor_runtime.h"

namespace {

struct FakeAcquisition {
    void pause() { ++pause_calls; }
    void resume() { ++resume_calls; }
    void update() { ++update_calls; }
    SensorStatus prepare_for_runtime() {
        ++prepare_calls;
        return prepare_status;
    }
    SensorStatus finish_recording() {
        ++finish_calls;
        ++pause_calls;
        return finish_status;
    }

    SensorStatus prepare_status = SensorStatus::Ok;
    SensorStatus finish_status = SensorStatus::Ok;
    uint32_t pause_calls = 0;
    uint32_t resume_calls = 0;
    uint32_t update_calls = 0;
    uint32_t prepare_calls = 0;
    uint32_t finish_calls = 0;
};

struct FakeSinkState {
    bool faulted = false;
};

struct FakeMeasurementSink {
    bool flush_pending() {
        ++flush_calls;
        return !faulted;
    }
    FakeSinkState state() const { return {faulted}; }
    bool settled() const { return settled_value && !faulted; }

    bool faulted = false;
    bool settled_value = true;
    uint32_t flush_calls = 0;
};

using Runtime = runtime::SensorRuntime<FakeAcquisition, FakeMeasurementSink>;
using Command = runtime::SensorRuntimeCommand;
using Result = runtime::SensorRuntimeResult;
using Mailbox = runtime::OperationMailbox<Command, Result, 4, 2>;
using Executor = runtime::OperationExecutor<Mailbox, Command, Result>;

void test_start_poll_stop_are_explicit_and_idempotent() {
    FakeAcquisition acquisition;
    FakeMeasurementSink sink;
    Runtime runtime(acquisition, sink);
    assert(acquisition.pause_calls == 1);
    assert(runtime.snapshot().state == runtime::SensorRuntimeState::Idle);

    Result result = runtime.handle({1, runtime::SensorRuntimeOperation::Start});
    assert(result.code == runtime::SensorRuntimeResultCode::Ok);
    assert(result.state == runtime::SensorRuntimeState::Recording);
    assert(acquisition.prepare_calls == 1 && acquisition.resume_calls == 1);
    result = runtime.handle({2, runtime::SensorRuntimeOperation::Start});
    assert(result.code == runtime::SensorRuntimeResultCode::Ok);
    assert(acquisition.prepare_calls == 1 && acquisition.resume_calls == 1);

    runtime.poll();
    assert(acquisition.update_calls == 1);
    result = runtime.handle({3, runtime::SensorRuntimeOperation::Stop});
    assert(result.state == runtime::SensorRuntimeState::Idle);
    runtime.poll();
    assert(acquisition.update_calls == 1);
    assert(runtime.snapshot().start_count == 1);
    assert(runtime.snapshot().stop_count == 1);
}

void test_failed_start_enters_terminal_fault() {
    FakeAcquisition acquisition;
    acquisition.prepare_status = SensorStatus::CommError;
    FakeMeasurementSink sink;
    Runtime runtime(acquisition, sink);
    Result result = runtime.handle({4, runtime::SensorRuntimeOperation::Start});
    assert(result.code == runtime::SensorRuntimeResultCode::TransitionFailed);
    assert(result.state == runtime::SensorRuntimeState::Faulted);
    assert(result.sensor_status == SensorStatus::CommError);
    result = runtime.handle({5, runtime::SensorRuntimeOperation::Stop});
    assert(result.code == runtime::SensorRuntimeResultCode::Faulted);
}

void test_ingress_fault_stops_acquisition() {
    FakeAcquisition acquisition;
    FakeMeasurementSink sink;
    Runtime runtime(acquisition, sink);
    assert(runtime.handle({1, runtime::SensorRuntimeOperation::Start}).code ==
           runtime::SensorRuntimeResultCode::Ok);
    sink.faulted = true;
    runtime.poll();
    assert(acquisition.update_calls == 0);
    assert(runtime.snapshot().state == runtime::SensorRuntimeState::Faulted);
    assert(runtime.snapshot().fault_count == 1);
}

void test_mailbox_executes_runtime_operations_on_owner_side() {
    FakeAcquisition acquisition;
    FakeMeasurementSink sink;
    Runtime runtime(acquisition, sink);
    Mailbox mailbox;
    Executor executor;
    assert(mailbox.try_send_command({71, runtime::SensorRuntimeOperation::Start}));
    assert(executor.poll(mailbox, [&](const Command& command) {
        return runtime.handle(command);
    }) == runtime::OperationExecutorStatus::CommandExecutedAndPublished);
    Result result{};
    assert(mailbox.try_receive_result(result));
    assert(result.operation_id == 71);
    assert(result.state == runtime::SensorRuntimeState::Recording);
}

void test_maintenance_is_explicit_and_exclusive() {
    FakeAcquisition acquisition;
    FakeMeasurementSink sink;
    Runtime runtime(acquisition, sink);

    Result result = runtime.handle({
        1, runtime::SensorRuntimeOperation::EnterMaintenance});
    assert(result.code == runtime::SensorRuntimeResultCode::Ok);
    assert(result.state == runtime::SensorRuntimeState::Maintenance);
    result = runtime.handle({2, runtime::SensorRuntimeOperation::Start});
    assert(result.code == runtime::SensorRuntimeResultCode::InvalidState);
    assert(acquisition.prepare_calls == 0);
    result = runtime.handle({
        3, runtime::SensorRuntimeOperation::ExitMaintenance});
    assert(result.state == runtime::SensorRuntimeState::Idle);
    assert(runtime.snapshot().maintenance_entry_count == 1);
}

void test_stop_waits_for_ingress_without_blocking() {
    FakeAcquisition acquisition;
    FakeMeasurementSink sink;
    Runtime runtime(acquisition, sink);
    assert(runtime.handle({1, runtime::SensorRuntimeOperation::Start}).state ==
           runtime::SensorRuntimeState::Recording);
    sink.settled_value = false;

    Result result = runtime.handle({2, runtime::SensorRuntimeOperation::Stop});
    assert(result.code == runtime::SensorRuntimeResultCode::InProgress);
    assert(result.state == runtime::SensorRuntimeState::Stopping);
    assert(acquisition.finish_calls == 1);
    runtime.poll();
    assert(runtime.snapshot().state == runtime::SensorRuntimeState::Stopping);
    assert(acquisition.update_calls == 0);

    sink.settled_value = true;
    runtime.poll();
    assert(runtime.snapshot().state == runtime::SensorRuntimeState::Idle);
    assert(runtime.snapshot().stop_count == 1);
    result = runtime.handle({2, runtime::SensorRuntimeOperation::Stop});
    assert(result.code == runtime::SensorRuntimeResultCode::Ok);
    assert(acquisition.finish_calls == 1);
}

void test_stop_drain_failure_is_fail_closed() {
    FakeAcquisition acquisition;
    acquisition.finish_status = SensorStatus::CommError;
    FakeMeasurementSink sink;
    Runtime runtime(acquisition, sink);
    assert(runtime.handle({1, runtime::SensorRuntimeOperation::Start}).state ==
           runtime::SensorRuntimeState::Recording);
    const Result result = runtime.handle({
        2, runtime::SensorRuntimeOperation::Stop});
    assert(result.code == runtime::SensorRuntimeResultCode::TransitionFailed);
    assert(result.state == runtime::SensorRuntimeState::Faulted);
    assert(result.sensor_status == SensorStatus::CommError);
}

void test_observability_profile_is_orthogonal_to_recording_state() {
    FakeAcquisition acquisition;
    FakeMeasurementSink sink;
    Runtime runtime(acquisition, sink);
    assert(runtime.handle({1, runtime::SensorRuntimeOperation::Start}).state ==
           runtime::SensorRuntimeState::Recording);

    Result result = runtime.handle({
        2,
        runtime::SensorRuntimeOperation::SetObservabilityProfile,
        runtime::ObservabilityProfile::Diagnostic,
    });
    assert(result.code == runtime::SensorRuntimeResultCode::Ok);
    assert(result.state == runtime::SensorRuntimeState::Recording);
    assert(result.observability == runtime::ObservabilityProfile::Diagnostic);
    runtime.poll();
    assert(acquisition.update_calls == 1);

    result = runtime.handle({
        3,
        runtime::SensorRuntimeOperation::SetObservabilityProfile,
        static_cast<runtime::ObservabilityProfile>(0xff),
    });
    assert(result.code == runtime::SensorRuntimeResultCode::InvalidArgument);
    assert(result.observability == runtime::ObservabilityProfile::Diagnostic);
    assert(runtime.snapshot().profile_change_count == 1);
}

}  // namespace

int main() {
    test_start_poll_stop_are_explicit_and_idempotent();
    test_failed_start_enters_terminal_fault();
    test_ingress_fault_stops_acquisition();
    test_mailbox_executes_runtime_operations_on_owner_side();
    test_maintenance_is_explicit_and_exclusive();
    test_stop_waits_for_ingress_without_blocking();
    test_stop_drain_failure_is_fail_closed();
    test_observability_profile_is_orthogonal_to_recording_state();
    return 0;
}
