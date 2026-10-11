#include <atomic>
#include <cstdio>

#include "hardware/spi.h"
#include "hardware/watchdog.h"
#include "pico/multicore.h"
#include "pico/rand.h"
#include "pico/stdlib.h"

#include "acquisition/acquisition_engine.h"
#include "adxl355/adxl355_driver.h"
#include "boot_shared/boot_runtime_api.h"
#include "boot_shared/boot_shared_metadata_io.h"
#include "common/device_identity.h"
#include "common/protocol_ids.h"
#include "config/board_profile.h"
#include "config/config_manager.h"
#include "config/config_store_flash.h"
#include "config/node_v2_capabilities.h"
#include "diagnostics/diagnostic_store.h"
#include "diagnostics/persistent_diagnostic_store.h"
#include "observer/config_usb_observer_identity_source.h"
#include "observer/observer_snapshot_sink.h"
#include "observer/tinyusb_cdc_port.h"
#include "observer/usb_observer_endpoint.h"
#include "processing/sample_sink_fanout.h"
#include "protocol_v3/identity.h"
#include "runtime/core0_acquisition_loop.h"
#include "runtime/node_v2_composition.h"
#include "storage/acquisition_buffer.h"
#include "transport/rs485_port.h"

#define SPI_PORT spi1
#define RS485_UART uart0

namespace {

constexpr uint32_t kSafeBootDelayMs = 5000;
constexpr uint32_t kWatchdogTimeoutMs = 3000;
constexpr uint64_t kCore1StartupTimeoutUs = 1000ull * 1000ull;
constexpr size_t kCore1StackBytes = 8u * 1024u;
constexpr uint64_t kTrialConfirmationTimeoutUs = 30ull * 1000ull * 1000ull;
constexpr uint32_t kSensorSpiBaud = 5 * 1000 * 1000;
constexpr size_t kObserverSnapshotCapacity = 32;

using Acquisition = AcquisitionEngine<
    node_v2::Profile::kAcquisitionBufferSamples>;
using Observer = UsbObserverEndpoint<kObserverSnapshotCapacity>;
using Composition = node_v2::RuntimeComposition<
    Acquisition,
    Rs485Port,
    Observer>;

Composition* g_composition = nullptr;
std::atomic<bool> g_core1_ready{false};
alignas(8) uint32_t g_core1_stack[kCore1StackBytes / sizeof(uint32_t)]{};

class BootControl final : public runtime::IFirmwareUpdateControl,
                          public runtime::ITrialBootControl {
public:
    BootControl() : state_(boot::app_trial_boot_state()) {}

    runtime::FirmwareUpdatePreparationStatus prepare_firmware_update() override {
        return boot::app_request_enter_bootloader()
            ? runtime::FirmwareUpdatePreparationStatus::Prepared
            : runtime::FirmwareUpdatePreparationStatus::StorageError;
    }

    runtime::TrialBootConfirmationStatus confirm_trial_boot() override {
        const boot::TrialBootConfirmResult result =
            boot::app_confirm_current_trial();
        switch (result) {
            case boot::TrialBootConfirmResult::Confirmed:
                state_ = boot::TrialBootState::Stable;
                return runtime::TrialBootConfirmationStatus::Confirmed;
            case boot::TrialBootConfirmResult::AlreadyConfirmed:
                state_ = boot::TrialBootState::Stable;
                return runtime::TrialBootConfirmationStatus::AlreadyConfirmed;
            case boot::TrialBootConfirmResult::NotRunningTrial:
                return runtime::TrialBootConfirmationStatus::NotRunningTrial;
            case boot::TrialBootConfirmResult::InvalidMetadata:
            default:
                return runtime::TrialBootConfirmationStatus::StorageError;
        }
    }

    bool confirmation_required() const {
        return state_ == boot::TrialBootState::PendingCurrentSlot;
    }

private:
    boot::TrialBootState state_ = boot::TrialBootState::Unavailable;
};

[[noreturn]] void restart_for_trial_rollback() {
    watchdog_reboot(0, 0, 0);
    while (true) tight_loop_contents();
}

void setup_spi() {
    spi_init(SPI_PORT, kSensorSpiBaud);
    gpio_set_function(BoardProfile::kSpiMiso, GPIO_FUNC_SPI);
    gpio_set_function(BoardProfile::kSpiSck, GPIO_FUNC_SPI);
    gpio_set_function(BoardProfile::kSpiMosi, GPIO_FUNC_SPI);
    spi_set_format(SPI_PORT, 8, SPI_CPOL_0, SPI_CPHA_0, SPI_MSB_FIRST);
}

void init_status_led() {
    gpio_init(PICO_DEFAULT_LED_PIN);
    gpio_set_dir(PICO_DEFAULT_LED_PIN, GPIO_OUT);
    gpio_put(PICO_DEFAULT_LED_PIN, 0);
}

void core1_entry() {
    multicore_lockout_victim_init();
    g_core1_ready.store(true, std::memory_order_release);
    while (g_composition != nullptr) {
        (void)g_composition->core1().poll(time_us_64());
        tight_loop_contents();
    }
    while (true) tight_loop_contents();
}

ProtocolV3::NodeIdentity make_identity(uint8_t node_address,
                                       uint64_t boot_epoch) {
    ProtocolV3::NodeIdentity identity{};
    const DeviceHardwareId hardware_id = read_device_hardware_id();
    for (size_t index = 0; index < ProtocolV3::kHardwareIdBytes; ++index) {
        identity.hardware_id[index] = hardware_id.bytes[index];
    }
    identity.boot_epoch = boot_epoch;
    identity.node_address = node_address;
    identity.board_revision = BoardProfile::kRevision;
    identity.firmware = {
        FW_VERSION_MAJOR,
        FW_VERSION_MINOR,
        FW_VERSION_PATCH,
    };
    return identity;
}

ProtocolV3::NodeCapabilities make_capabilities() {
#if SENSOR_SYSTEM_APP_SLOT == 1 || SENSOR_SYSTEM_APP_SLOT == 2
    constexpr auto image_kind = node_v2::FirmwareImageKind::AbSlot;
#else
    constexpr auto image_kind = node_v2::FirmwareImageKind::Standalone;
#endif
    return node_v2::make_node_capabilities(
        image_kind, BoardProfile::kTimestampingV2Enabled);
}

}  // namespace

int main() {
    stdio_init_all();
    multicore_lockout_victim_init();
    init_status_led();
    sleep_ms(kSafeBootDelayMs);
    watchdog_enable(kWatchdogTimeoutMs, 1);
    setup_spi();

    static FlashConfigStore config_store;
    static ConfigManager config_manager(config_store);
    const bool config_loaded = config_manager.init();
    const DeviceConfig& config = config_manager.current();

    boot::BootMetadata boot_metadata{};
    const bool boot_metadata_loaded = boot::app_boot_metadata_load(boot_metadata);
    static PersistentDiagnosticStore persistent_diagnostics;
    static DiagnosticStore diagnostics;
    diagnostics.set_persistent_store(&persistent_diagnostics);
    diagnostics.set_boot_counter(
        boot_metadata_loaded ? boot_metadata.boot_counter : 0u);

    static node_v2::DataPlane data_plane(config_manager.generation());
    static AcquisitionBuffer<node_v2::Profile::kAcquisitionBufferSamples>
        acquisition_buffer;
    static Adxl355Driver driver(
        SPI_PORT,
        BoardProfile::kSpiCs,
        BoardProfile::kDrdy,
        BoardProfile::kInt1,
        BoardProfile::kTimestampingV2Enabled);
    static ObserverSnapshotSink<kObserverSnapshotCapacity> observer_snapshot;
    static SampleSinkFanout sample_sinks(data_plane.sink(), observer_snapshot);
    static Acquisition acquisition(
        driver,
        acquisition_buffer,
        &sample_sinks,
        &diagnostics,
        &data_plane.sink());
    const SensorStatus sensor_status = acquisition.init(config);

    static Rs485Port rs485_port(
        RS485_UART,
        BoardProfile::kRs485Tx,
        BoardProfile::kRs485Rx,
        config.baudrate,
        BoardProfile::kRs485De);
    const bool rs485_ready = rs485_port.init();

    UsbObserverIdentity observer_identity{};
    observer_identity.hardware_id = read_device_hardware_id();
    observer_identity.firmware_major = FW_VERSION_MAJOR;
    observer_identity.firmware_minor = FW_VERSION_MINOR;
    observer_identity.firmware_patch = FW_VERSION_PATCH;
    observer_identity.product_protocol_version =
        ProtocolV3::kApplicationProtocolVersion;
    observer_identity.board_revision = BoardProfile::kRevision;
    observer_identity.timestamping_enabled =
        BoardProfile::kTimestampingV2Enabled;
    static ConfigUsbObserverIdentitySource observer_identity_source(
        config_manager, observer_identity);
    static TinyUsbCdcPort usb_port;
    static Observer observer(
        usb_port, observer_snapshot, observer_identity_source);

    uint64_t boot_epoch = get_rand_64();
    if (boot_epoch == 0) boot_epoch = 1;
    static BootControl boot_control;
    static Composition composition(
        data_plane,
        acquisition,
        rs485_port,
        observer,
        make_identity(config.node_id, boot_epoch),
        make_capabilities(),
        boot_epoch,
        {256, 100000, 1000},
        {node_v2::Profile::kIngressDrainBudgetPerPoll},
        &boot_control,
        &boot_control);

    const bool runtime_ready =
        config_loaded && sensor_status == SensorStatus::Ok && rs485_ready &&
        composition.configuration_valid();
    if (!runtime_ready) {
        gpio_put(PICO_DEFAULT_LED_PIN, 0);
        if (boot_control.confirmation_required()) {
            restart_for_trial_rollback();
        }
        while (true) {
            watchdog_update();
            sleep_ms(100);
        }
    }

    g_composition = &composition;
    multicore_launch_core1_with_stack(
        core1_entry, g_core1_stack, sizeof(g_core1_stack));
    const uint64_t core1_start_deadline_us =
        time_us_64() + kCore1StartupTimeoutUs;
    while (!g_core1_ready.load(std::memory_order_acquire) &&
           time_us_64() < core1_start_deadline_us) {
        watchdog_update();
        tight_loop_contents();
    }
    if (!g_core1_ready.load(std::memory_order_acquire)) {
        gpio_put(PICO_DEFAULT_LED_PIN, 0);
        if (boot_control.confirmation_required()) {
            restart_for_trial_rollback();
        }
        while (true) {
            watchdog_update();
            sleep_ms(100);
        }
    }
    (void)boot::app_sync_boot_settings(config.node_id);
    gpio_put(PICO_DEFAULT_LED_PIN, 1);
    const uint64_t trial_deadline_us =
        time_us_64() + kTrialConfirmationTimeoutUs;

    while (true) {
        watchdog_update();
        composition.poll_core0();
        if (boot_control.confirmation_required() &&
            time_us_64() >= trial_deadline_us) {
            restart_for_trial_rollback();
        }
        runtime::DeferredSystemActionKind action{};
        if (composition.system_action().try_consume(action) &&
            action == runtime::DeferredSystemActionKind::EnterBootloader) {
            sleep_ms(20);
            run_system_action(RuntimeSystemActionEnterBootloader);
        }
        tight_loop_contents();
    }
}
