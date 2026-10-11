#include <cassert>
#include <cstddef>
#include <cstdint>

#include "runtime/node_v2_composition.h"

namespace {

class FakeAcquisition {
public:
    explicit FakeAcquisition(node_v2::MeasurementSink& sink) : sink_(sink) {}
    void pause() { paused = true; }
    void resume() { paused = false; }
    SensorStatus prepare_for_runtime() { return SensorStatus::Ok; }
    SensorStatus finish_recording() {
        paused = true;
        sink_.on_continuity_break(
            measurement::ContinuityBreakReason::ProcessingReset);
        return SensorStatus::Ok;
    }
    void update() {}
    bool paused = false;

private:
    node_v2::MeasurementSink& sink_;
};

class FakePort {
public:
    void poll() {}
    bool read_byte(uint8_t&) { return false; }
    bool start_tx(const uint8_t*, size_t) { return false; }
    bool tx_busy() const { return false; }
    bool tx_completed() const { return false; }
    void clear_tx_completed() {}
    void abort_tx() {}
};

class FakeObserver {
public:
    void update() { ++updates; }
    uint32_t updates = 0;
};

class FakeFirmwareUpdateControl final
    : public runtime::IFirmwareUpdateControl {
public:
    runtime::FirmwareUpdatePreparationStatus prepare_firmware_update() override {
        ++calls;
        return runtime::FirmwareUpdatePreparationStatus::Prepared;
    }

    uint32_t calls = 0;
};

ProtocolV3::NodeIdentity identity() {
    ProtocolV3::NodeIdentity value{};
    value.boot_epoch = 123;
    value.node_address = 7;
    value.board_revision = 2;
    value.firmware = {0, 5, 0};
    return value;
}

ProtocolV3::NodeCapabilities capabilities() {
    ProtocolV3::NodeCapabilities value{};
    value.service_flags =
        ProtocolV3::service_capability_bit(ProtocolV3::ServiceId::Identity) |
        ProtocolV3::service_capability_bit(ProtocolV3::ServiceId::Measurement) |
        ProtocolV3::service_capability_bit(ProtocolV3::ServiceId::Lifecycle);
    value.feature_flags = ProtocolV3::kFeatureSampleJournal;
    value.max_payload_bytes = ProtocolV3::kMaxPayloadBytes;
    value.journal_capacity_samples = node_v2::Profile::kJournalSamples;
    value.output_rate_mask = ProtocolV3::kImplementedOutputRateMask;
    value.max_sample_chunk_samples = node_v2::Profile::kMaxChunkSamples;
    return value;
}

void test_production_types_form_one_valid_composition() {
    node_v2::DataPlane data_plane(1);
    FakeAcquisition acquisition(data_plane.sink());
    FakePort port;
    FakeObserver observer;
    node_v2::RuntimeComposition composition(
        data_plane,
        acquisition,
        port,
        observer,
        identity(),
        capabilities(),
        123,
        {64, 1000});

    assert(composition.configuration_valid());
    const runtime::SensorRuntimeCommand start{
        1, runtime::SensorRuntimeOperation::Start};
    uint8_t request[ProtocolV3::kLifecycleProfileRequestBytes]{};
    uint8_t message = 0;
    size_t request_size = 0;
    assert(ProtocolV3::encode_lifecycle_request(
               start,
               request,
               sizeof(request),
               request_size,
               message) == ProtocolV3::LifecycleCodecStatus::Ok);
    uint8_t response[ProtocolV3::kMaxPayloadBytes]{};
    assert(composition.lifecycle().handle(
               message,
               request,
               request_size,
               response,
               sizeof(response)).payload_size ==
           ProtocolV3::kLifecycleResponseBytes);
    composition.core0().poll();
    assert(!acquisition.paused);
    assert(composition.core1().poll(10) ==
           runtime::Core1ProtocolPollStatus::Ok);
    assert(composition.lifecycle().state().completed_operations == 1);
    assert(observer.updates == 1);
}

void test_invalid_config_revision_fails_composition_validation() {
    node_v2::DataPlane data_plane(0);
    FakeAcquisition acquisition(data_plane.sink());
    FakePort port;
    FakeObserver observer;
    node_v2::RuntimeComposition composition(
        data_plane,
        acquisition,
        port,
        observer,
        identity(),
        capabilities(),
        123);
    assert(!composition.configuration_valid());
}

void test_composition_admits_update_only_after_entering_maintenance() {
    node_v2::DataPlane data_plane(1);
    FakeAcquisition acquisition(data_plane.sink());
    FakePort port;
    FakeObserver observer;
    FakeFirmwareUpdateControl firmware_update;
    node_v2::RuntimeComposition composition(
        data_plane, acquisition, port, observer,
        identity(), capabilities(), 123, {}, {}, &firmware_update);
    uint8_t request[ProtocolV3::kLifecycleProfileRequestBytes]{};
    uint8_t response[ProtocolV3::kMaxPayloadBytes]{};
    uint8_t message = 0;
    size_t request_size = 0;
    const runtime::SensorRuntimeCommand enter_maintenance{
        70, runtime::SensorRuntimeOperation::EnterMaintenance};
    assert(ProtocolV3::encode_lifecycle_request(
               enter_maintenance, request, sizeof(request),
               request_size, message) == ProtocolV3::LifecycleCodecStatus::Ok);
    (void)composition.lifecycle().handle(
        message, request, request_size, response, sizeof(response));
    composition.poll_core0();
    assert(composition.core1().poll(1) == runtime::Core1ProtocolPollStatus::Ok);

    assert(ProtocolV3::encode_prepare_firmware_update_request(
               71, request, sizeof(request)) ==
           ProtocolV3::LifecycleCodecStatus::Ok);
    (void)composition.lifecycle().handle(
        ProtocolV3::kPrepareFirmwareUpdateMessageId,
        request,
        ProtocolV3::kLifecycleOperationRequestBytes,
        response,
        sizeof(response));
    composition.poll_core0();
    assert(composition.core1().poll(2) == runtime::Core1ProtocolPollStatus::Ok);
    const ProtocolV3::LifecycleHandlerResult prepared =
        composition.lifecycle().handle(
            ProtocolV3::kPrepareFirmwareUpdateMessageId,
            request,
            ProtocolV3::kLifecycleOperationRequestBytes,
            response,
            sizeof(response));
    assert(prepared.post_transmit_required);
    assert(firmware_update.calls == 1);
    assert(composition.lifecycle().settle_transmission(
        prepared.post_transmit_token, true));
    assert(composition.system_action().peek() ==
           runtime::DeferredSystemActionKind::EnterBootloader);
}

}  // namespace

int main() {
    test_production_types_form_one_valid_composition();
    test_invalid_config_revision_fails_composition_validation();
    test_composition_admits_update_only_after_entering_maintenance();
    return 0;
}
