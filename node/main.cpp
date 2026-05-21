#include <stdio.h>

#include "pico/stdlib.h"
#include "pico/multicore.h"
#include "hardware/spi.h"
#include "hardware/watchdog.h"
#include "boards/pico.h"

#include "acquisition/acquisition_engine.h"
#include "adxl355/adxl355_driver.h"
#include "boot_shared/boot_runtime_api.h"
#include "config/config_manager.h"
#include "config/config_store_flash.h"
#include "controller/node_controller.h"
#include "runtime/core0_acquisition_loop.h"
#include "runtime/core1_transport_loop.h"
#include "storage/acquisition_buffer.h"
#include "system/runtime_context.h"
#include "transport/data_plane.h"
#include "transport/rs485_port.h"
#include "transport/transport.h"

#define SPI_PORT   spi0
#define PIN_MISO   16
#define PIN_CS     17
#define PIN_SCK    18
#define PIN_MOSI   19
#define PIN_DRDY   22
#define PIN_INT1   21  // preferred: FIFO watermark/overrun interrupt (INT1)

#define RS485_UART uart0
#define PIN_RS485_TX 0
#define PIN_RS485_RX 1
#define PIN_RS485_DE 2

static constexpr uint32_t SAFE_BOOT_DELAY_MS = 5000;
static constexpr uint32_t WATCHDOG_TIMEOUT_MS = 3000;
static constexpr uint32_t SENSOR_SPI_BAUD = 5 * 1000 * 1000;
static constexpr uint32_t RS485_BAUD = 115200;
static constexpr size_t ACQ_BUFFER_CAPACITY = 8192;
static constexpr size_t PACKET_QUEUE_CAPACITY = 128;

using DataPlaneT = DataPlane<PACKET_QUEUE_CAPACITY, 32>;
using AcquisitionT = AcquisitionEngine<ACQ_BUFFER_CAPACITY>;
using ControllerT = NodeController<ACQ_BUFFER_CAPACITY>;
using TransportT = Transport<ACQ_BUFFER_CAPACITY, ControllerT>;
using RuntimeT = RuntimeContext<AcquisitionT, TransportT>;

static RuntimeT* g_runtime = nullptr;

static void init_status_led() {
    gpio_init(PICO_DEFAULT_LED_PIN);
    gpio_set_dir(PICO_DEFAULT_LED_PIN, GPIO_OUT);
    gpio_put(PICO_DEFAULT_LED_PIN, 0);
}

static void set_status_led(bool on) {
    gpio_put(PICO_DEFAULT_LED_PIN, on ? 1 : 0);
}

static void signal_starting() {
    for (uint32_t i = 0; i < 5; ++i) {
        set_status_led(true);
        sleep_ms(60);
        set_status_led(false);
        sleep_ms(60);
    }
}

static void setup_spi() {
    spi_init(SPI_PORT, SENSOR_SPI_BAUD);

    gpio_set_function(PIN_MISO, GPIO_FUNC_SPI);
    gpio_set_function(PIN_SCK,  GPIO_FUNC_SPI);
    gpio_set_function(PIN_MOSI, GPIO_FUNC_SPI);

    spi_set_format(
        SPI_PORT,
        8,
        SPI_CPOL_0,
        SPI_CPHA_0,
        SPI_MSB_FIRST
    );
}

static void core1_entry() {
    multicore_lockout_victim_init();

    if (g_runtime != nullptr) {
        run_core1_transport_loop(*g_runtime);
    }

    while (true) {
        tight_loop_contents();
    }
}

int main() {
    stdio_init_all();
    multicore_lockout_victim_init();
    init_status_led();
    signal_starting();
    sleep_ms(SAFE_BOOT_DELAY_MS);

    printf("\n=== ADXL355 NODE START ===\n");

    watchdog_enable(WATCHDOG_TIMEOUT_MS, 1);

    setup_spi();

    static FlashConfigStore config_store;
    static ConfigManager config_manager(config_store);

    const bool loaded = config_manager.init();
    printf("config.init() -> %s\n", loaded ? "loaded" : "defaults");

    static AcquisitionBuffer<ACQ_BUFFER_CAPACITY> acq_buffer;
    static Adxl355Driver driver(SPI_PORT, PIN_CS, PIN_DRDY, PIN_INT1);
    static DataPlaneT data_plane;
    static AcquisitionT acquisition(driver, acq_buffer, &data_plane);

    static Rs485Port rs485_port(
        RS485_UART,
        PIN_RS485_TX,
        PIN_RS485_RX,
        RS485_BAUD,
        PIN_RS485_DE
    );

    if (!rs485_port.init()) {
        printf("rs485_port.init() failed\n");
        set_status_led(false);
        while (true) {
            sleep_ms(100);
        }
    }

    static ControllerT controller(
        config_manager,
        acquisition,
        driver,
        data_plane,
        &rs485_port
    );

    const bool controller_ready = controller.init();
    if (!controller_ready) {
        printf("controller.init() failed, status=%d\n",
               static_cast<int>(controller.init_status()));
    }

    const auto cfg = config_manager.current();
    const uint32_t output_odr_millihz =
        (static_cast<uint32_t>(cfg.odr_hz) * 1000u) /
        DecimatingFilterX2::kDecimationFactor;

    printf(
        "CONFIG: node_id=%u baud=%lu sensor_odr=%u output_odr=%lu.%03lu range=%u high_pass_corner=%u offsets=(%ld,%ld,%ld) fifo_watermark=%u\n",
        static_cast<unsigned>(cfg.node_id),
        static_cast<unsigned long>(cfg.baudrate),
        static_cast<unsigned>(cfg.odr_hz),
        static_cast<unsigned long>(output_odr_millihz / 1000u),
        static_cast<unsigned long>(output_odr_millihz % 1000u),
        static_cast<unsigned>(cfg.range_g),
        static_cast<unsigned>(cfg.high_pass_corner),
        static_cast<long>(cfg.offset_x),
        static_cast<long>(cfg.offset_y),
        static_cast<long>(cfg.offset_z),
        static_cast<unsigned>(cfg.fifo_watermark)
    );

    static RuntimeT runtime{};
    runtime.acquisition = controller_ready ? &acquisition : nullptr;
    runtime.core1_ready = false;
    runtime.stop_requested = false;
    runtime.requested_action = RuntimeSystemActionNone;

    static TransportT transport(
        rs485_port,
        controller,
        data_plane,
        &runtime.requested_action
    );

    if (!transport.init()) {
        printf("transport.init() failed\n");
        set_status_led(false);
        while (true) {
            sleep_ms(100);
        }
    }

    g_runtime = &runtime;
    runtime.transport = &transport;
    multicore_launch_core1(core1_entry);

    while (!runtime.core1_ready) {
        sleep_ms(1);
    }

    const bool boot_settings_synced =
        boot::app_sync_boot_settings(config_manager.current().node_id);
    const bool boot_confirmed = controller_ready
        ? boot::app_confirm_boot_success()
        : false;

    printf("boot.sync() -> %s, boot.confirm() -> %s\n",
           boot_settings_synced ? "ok" : "skip/fail",
           boot_confirmed ? "confirmed" :
               (controller_ready ? "not_pending" : "blocked_not_ready"));

    printf("core0=acquisition, core1=transport\n");
    set_status_led(controller_ready);

    run_core0_acquisition_loop(runtime);

    return 0;
}
