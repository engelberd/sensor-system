#include <cassert>
#include <cstddef>
#include <cstdint>

#include "measurement/measurement_ingress_sink.h"
#include "protocol_v3/dispatcher.h"
#include "protocol_v3/measurement.h"
#include "runtime/core0_sensor_runtime.h"
#include "runtime/core1_measurement_runtime.h"

namespace {

constexpr uint64_t kBootEpoch = 0x1234;

using Ingress = measurement::SampleIngress<8>;
using Sink = measurement::MeasurementIngressSink<Ingress>;
using Mailbox = runtime::OperationMailbox<
    runtime::SensorRuntimeCommand,
    runtime::SensorRuntimeResult,
    4,
    4>;

class FakeAcquisition {
public:
    explicit FakeAcquisition(Sink& sink) : sink_(sink) {}

    void pause() { paused_ = true; }
    void resume() { paused_ = false; }
    SensorStatus prepare_for_runtime() {
        ++prepare_count;
        return SensorStatus::Ok;
    }
    SensorStatus finish_recording() {
        paused_ = true;
        ++finish_count;
        sink_.on_continuity_break(
            measurement::ContinuityBreakReason::ProcessingReset
        );
        return SensorStatus::Ok;
    }

    void update() {
        assert(!paused_);
        ++update_count;
        if (update_count == 2) {
            sink_.on_continuity_break(
                measurement::ContinuityBreakReason::SensorReadUncertain
            );
        }
        const StoredSample sample{
            update_count - 1,
            static_cast<int32_t>(update_count),
            -static_cast<int32_t>(update_count),
            static_cast<int32_t>(update_count + 1),
        };
        const SampleDeviceTime timestamp{
            1000u + static_cast<uint64_t>(update_count) * 4000u,
            update_count == 1 ? 1u : 2u,
            TIMING_QUALITY_LOCKED,
            TimestampSource::DrdyTimeUs64,
            0,
        };
        sink_.on_timed_samples(&sample, &timestamp, 1);
    }

    uint32_t prepare_count = 0;
    uint32_t update_count = 0;
    uint32_t finish_count = 0;

private:
    Sink& sink_;
    bool paused_ = true;
};

ProtocolV3::NodeIdentity identity() {
    ProtocolV3::NodeIdentity value{};
    value.boot_epoch = kBootEpoch;
    value.node_address = 7;
    value.board_revision = 2;
    value.firmware = {0, 5, 0};
    return value;
}

ProtocolV3::NodeCapabilities capabilities() {
    ProtocolV3::NodeCapabilities value{};
    value.service_flags = ProtocolV3::service_capability_bit(
        ProtocolV3::ServiceId::Measurement
    );
    value.feature_flags = ProtocolV3::kFeatureSampleJournal;
    value.max_payload_bytes = ProtocolV3::kMaxPayloadBytes;
    value.journal_capacity_samples = 16;
    value.max_sample_chunk_samples = 4;
    value.output_rate_mask = ProtocolV3::kImplementedOutputRateMask;
    return value;
}

ProtocolV3::Frame measurement_request(uint8_t message) {
    ProtocolV3::Frame request{};
    request.role = ProtocolV3::Role::Request;
    request.destination = 7;
    request.source = ProtocolV3::kHostAddress;
    request.service = ProtocolV3::kMeasurementServiceId;
    request.message = message;
    request.transaction_id = 42;
    return request;
}

void test_mailbox_ingress_journal_and_dispatcher_form_one_vertical_slice() {
    runtime::SpscQueue<measurement::IngressEvent, 8> ingress_queue;
    Ingress ingress(ingress_queue);
    Sink sink(ingress, 3);
    FakeAcquisition acquisition(sink);
    runtime::SensorRuntime<FakeAcquisition, Sink> sensor_runtime(
        acquisition, sink);
    Mailbox mailbox;
    runtime::RuntimeFaultLatch fault_latch;
    runtime::Core0SensorRuntime<decltype(sensor_runtime), Mailbox> core0(
        sensor_runtime, mailbox, fault_latch);
    runtime::Core1MeasurementRuntime<8, 16, 4, 4> core1(
        ingress_queue, kBootEpoch, fault_latch);

    assert(mailbox.try_send_command({
        10, runtime::SensorRuntimeOperation::Start}));
    core0.poll();
    runtime::SensorRuntimeResult operation_result{};
    assert(mailbox.try_receive_result(operation_result));
    assert(operation_result.operation_id == 10);
    assert(operation_result.state == runtime::SensorRuntimeState::Recording);

    core0.poll();
    assert(acquisition.update_count == 2);
    assert(core1.poll_ingress(8) == runtime::IngressDrainStatus::Progress);
    const auto measurement_state = core1.state();
    assert(measurement_state.journal.produced == 3);
    assert(measurement_state.consumed_samples == 2);
    assert(measurement_state.consumed_continuity_breaks == 1);
    assert(!measurement_state.faulted);

    ProtocolV3::ApplicationDispatcher dispatcher(
        identity(), capabilities(), &core1.handler());
    ProtocolV3::Frame state_response{};
    assert(dispatcher.dispatch(
        measurement_request(ProtocolV3::kGetStreamStateMessageId),
        state_response));
    ProtocolV3::StreamState wire_state{};
    ProtocolV3::ResponseStatus remote =
        ProtocolV3::ResponseStatus::InternalError;
    assert(ProtocolV3::decode_stream_state_success(
               state_response.payload, state_response.payload_size,
               wire_state, remote) ==
           ProtocolV3::MeasurementCodecStatus::Ok);
    assert(remote == ProtocolV3::ResponseStatus::Ok);
    assert(wire_state.boot_epoch == kBootEpoch);
    assert(wire_state.produced == 3);

    ProtocolV3::Frame pull = measurement_request(
        ProtocolV3::kPullSamplesMessageId);
    assert(ProtocolV3::encode_pull_request(
               {kBootEpoch, 0, 0, 4},
               pull.payload, sizeof(pull.payload)) ==
           ProtocolV3::MeasurementCodecStatus::Ok);
    pull.payload_size = ProtocolV3::kPullRequestBytes;
    ProtocolV3::Frame pull_response{};
    const ProtocolV3::DispatchResult dispatched =
        dispatcher.dispatch(pull, pull_response);
    assert(dispatched.response_produced && dispatched.completion.required);
    ProtocolV3::PullResponse decoded{};
    assert(ProtocolV3::decode_pull_response(
               pull_response.payload, pull_response.payload_size,
               decoded, remote) == ProtocolV3::MeasurementCodecStatus::Ok);
    assert(decoded.result == ProtocolV3::PullResultCode::SampleChunk);
    assert(decoded.chunk.first == 0);
    assert(decoded.chunk.sample_count == 1);
    assert(dispatcher.settle_response(dispatched.completion, true) ==
           ProtocolV3::CompletionSettlementStatus::Ok);

    assert(mailbox.try_send_command({
        11, runtime::SensorRuntimeOperation::Stop}));
    core0.poll();
    assert(mailbox.try_receive_result(operation_result));
    assert(operation_result.operation_id == 11);
    assert(operation_result.code == runtime::SensorRuntimeResultCode::InProgress);
    assert(operation_result.state == runtime::SensorRuntimeState::Stopping);
    assert(acquisition.update_count == 2);
    assert(acquisition.finish_count == 1);
    assert(core1.poll_ingress(8) == runtime::IngressDrainStatus::Progress);
    core0.poll();
    assert(core0.state().sensor.state == runtime::SensorRuntimeState::Idle);
}

void test_core1_budget_and_rejection_are_fail_closed() {
    runtime::SpscQueue<measurement::IngressEvent, 2> queue;
    runtime::RuntimeFaultLatch fault_latch;
    runtime::Core1MeasurementRuntime<2, 4, 2, 2> core1(
        queue, kBootEpoch, fault_latch);
    assert(core1.poll_ingress(0) == runtime::IngressDrainStatus::InvalidBudget);

    const measurement::SampleRecord sample{
        1000, 1, 2, 3, 1, 1, measurement::kTimingValidFlag, 0};
    assert(queue.try_push(
        measurement::IngressEvent::sample_event(1, sample)));
    assert(core1.poll_ingress(1) == runtime::IngressDrainStatus::Faulted);
    assert(core1.state().faulted);
    assert(core1.state().rejected_events == 1);
    assert(queue.size_approx() == 1);
    assert(fault_latch.reason() ==
           runtime::RuntimeFaultReason::MeasurementJournalRejected);
}

void test_core1_fault_stops_core0_before_another_sensor_update() {
    runtime::SpscQueue<measurement::IngressEvent, 8> ingress_queue;
    Ingress ingress(ingress_queue);
    Sink sink(ingress, 3);
    FakeAcquisition acquisition(sink);
    runtime::SensorRuntime<FakeAcquisition, Sink> sensor_runtime(
        acquisition, sink);
    Mailbox mailbox;
    runtime::RuntimeFaultLatch fault_latch;
    runtime::Core0SensorRuntime<decltype(sensor_runtime), Mailbox> core0(
        sensor_runtime, mailbox, fault_latch);

    assert(mailbox.try_send_command({
        1, runtime::SensorRuntimeOperation::Start}));
    core0.poll();
    assert(acquisition.update_count == 1);
    assert(fault_latch.trip(
        runtime::RuntimeFaultReason::MeasurementJournalRejected));
    core0.poll();
    assert(acquisition.update_count == 1);
    assert(core0.state().sensor.state == runtime::SensorRuntimeState::Faulted);
    assert(core0.state().shared_fault ==
           runtime::RuntimeFaultReason::MeasurementJournalRejected);
}

void test_invalid_epoch_latches_the_first_fault_reason() {
    runtime::SpscQueue<measurement::IngressEvent, 2> queue;
    runtime::RuntimeFaultLatch fault_latch;
    runtime::Core1MeasurementRuntime<2, 4, 2, 2> core1(
        queue, 0, fault_latch);
    assert(core1.state().faulted);
    assert(fault_latch.reason() ==
           runtime::RuntimeFaultReason::InvalidBootEpoch);
    assert(!fault_latch.trip(
        runtime::RuntimeFaultReason::MeasurementJournalRejected));
    assert(fault_latch.reason() ==
           runtime::RuntimeFaultReason::InvalidBootEpoch);
}

void test_result_backpressure_does_not_pause_measurement() {
    runtime::SpscQueue<measurement::IngressEvent, 8> ingress_queue;
    Ingress ingress(ingress_queue);
    Sink sink(ingress, 3);
    FakeAcquisition acquisition(sink);
    runtime::SensorRuntime<FakeAcquisition, Sink> sensor_runtime(
        acquisition, sink);
    Mailbox mailbox;
    runtime::RuntimeFaultLatch fault_latch;
    runtime::Core0SensorRuntime<decltype(sensor_runtime), Mailbox> core0(
        sensor_runtime, mailbox, fault_latch);

    for (uint32_t id = 100; id < 104; ++id) {
        assert(mailbox.try_send_result({
            id,
            runtime::SensorRuntimeResultCode::Ok,
            runtime::SensorRuntimeState::Idle,
            runtime::ObservabilityProfile::Production,
            SensorStatus::Ok,
        }));
    }
    assert(mailbox.try_send_command({
        1, runtime::SensorRuntimeOperation::Start}));
    core0.poll();
    assert(core0.state().last_command_status ==
           runtime::OperationExecutorStatus::CommandExecutedResultPending);
    assert(acquisition.update_count == 1);
    core0.poll();
    assert(core0.state().last_command_status ==
           runtime::OperationExecutorStatus::PendingResultBlocked);
    assert(acquisition.update_count == 2);
}

}  // namespace

int main() {
    test_mailbox_ingress_journal_and_dispatcher_form_one_vertical_slice();
    test_core1_budget_and_rejection_are_fail_closed();
    test_core1_fault_stops_core0_before_another_sensor_update();
    test_invalid_epoch_latches_the_first_fault_reason();
    test_result_backpressure_does_not_pause_measurement();
    return 0;
}
