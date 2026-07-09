#pragma once

#include <cstdint>

enum class DiagnosticSeverity : uint8_t {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
    Critical = 4,
};

enum class DiagnosticResetCause : uint8_t {
    Unknown = 0,
    PowerOn = 1,
    Watchdog = 2,
    WatchdogTimeout = 3,
};

enum class DiagnosticEventCode : uint16_t {
    Boot = 1,
    ControllerInitOk = 2,
    ControllerInitFailed = 3,
    Rs485InitFailed = 4,
    TransportInitFailed = 5,

    SensorInitFailed = 16,
    SensorCheckFailed = 17,
    SensorConfigFailed = 18,
    SensorOffsetsFailed = 19,

    FifoOverrun = 32,
    FifoStatusReadError = 33,
    FifoRepeatedNoData = 34,
    SensorReadError = 35,
    InvalidSample = 36,
    AcquisitionRecovered = 37,

    TemperatureReadFailed = 48,
    TemperatureReadRecovered = 49,

    RuntimeConfigApplyFailed = 64,
    RuntimeConfigApplied = 65,

    RestartCommand = 80,
    EnterBootloaderCommand = 81,
    DiagnosticsCleared = 82,
};

struct DiagnosticEvent {
    uint32_t event_id = 0;
    uint32_t time_ms = 0;
    uint16_t code = 0;
    uint8_t severity = 0;
    uint8_t repeat_count = 0;
    uint64_t sample_seq = 0;
    int32_t arg0 = 0;
    int32_t arg1 = 0;
};

struct DiagnosticFaultContext {
    uint64_t sample_seq = 0;
    uint32_t last_progress_ms = 0;
    uint32_t fifo_no_data = 0;
    uint32_t sensor_errors = 0;
    uint32_t dropped_samples = 0;
    uint32_t rx_overflow_count = 0;
    uint32_t packet_overwrite_count = 0;
    uint32_t debug_gpio_int1_edges = 0;
    uint32_t debug_gpio_drdy_edges = 0;
    uint32_t debug_config_snapshot = 0;
    uint32_t debug_irq_snapshot = 0;
    int32_t arg0 = 0;
    int32_t arg1 = 0;
};

struct DiagnosticFaultSnapshot {
    uint32_t event_id = 0;
    uint32_t time_ms = 0;
    uint16_t code = 0;
    uint8_t severity = 0;
    uint8_t reset_cause = 0;
    uint64_t sample_seq = 0;
    uint32_t last_progress_ms = 0;
    uint32_t fifo_no_data = 0;
    uint32_t sensor_errors = 0;
    uint32_t dropped_samples = 0;
    uint32_t rx_overflow_count = 0;
    uint32_t packet_overwrite_count = 0;
    uint32_t debug_gpio_int1_edges = 0;
    uint32_t debug_gpio_drdy_edges = 0;
    uint32_t debug_config_snapshot = 0;
    uint32_t debug_irq_snapshot = 0;
    int32_t arg0 = 0;
    int32_t arg1 = 0;
};

struct DiagnosticInfoSnapshot {
    uint32_t first_event_id = 1;
    uint32_t next_event_id = 1;
    uint32_t dropped_event_count = 0;
    uint32_t last_error_event_id = 0;
    uint16_t last_error_code = 0;
    uint16_t event_capacity = 0;
    uint16_t stored_event_count = 0;
    uint8_t reset_cause = 0;
    uint8_t live_usb_enabled = 0;
};

static constexpr uint32_t PERSISTENT_DIAGNOSTIC_MAGIC = 0x47414944; // DIAG
static constexpr uint16_t PERSISTENT_DIAGNOSTIC_VERSION = 3;

#pragma pack(push, 1)
struct PersistentDiagnosticRecord {
    uint32_t magic = PERSISTENT_DIAGNOSTIC_MAGIC;
    uint16_t version = PERSISTENT_DIAGNOSTIC_VERSION;
    uint8_t reset_cause = 0;
    uint8_t repeat_count = 0;
    uint32_t generation = 0;
    uint32_t boot_counter = 0;
    uint32_t firmware_version = 0;
    uint32_t event_id = 0;
    uint32_t time_ms = 0;
    uint16_t event_code = 0;
    uint8_t severity = 0;
    uint8_t reserved0 = 0;
    uint64_t sample_seq = 0;
    uint32_t last_progress_ms = 0;
    uint32_t fifo_no_data = 0;
    uint32_t sensor_errors = 0;
    uint32_t dropped_samples = 0;
    uint32_t rx_overflow_count = 0;
    uint32_t packet_overwrite_count = 0;
    uint32_t debug_gpio_int1_edges = 0;
    uint32_t debug_gpio_drdy_edges = 0;
    uint32_t debug_config_snapshot = 0;
    uint32_t debug_irq_snapshot = 0;
    int32_t arg0 = 0;
    int32_t arg1 = 0;
    uint32_t crc32 = 0;
};
#pragma pack(pop)
