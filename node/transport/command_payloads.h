#pragma once

#include <cstdint>

#include "common/device_identity.h"
#include "diagnostics/diagnostic_types.h"
#include "transport/command_types.h"
#include "transport/status_codes.h"

#pragma pack(push, 1)

struct CommandPayloadHeader {
    uint8_t command;
};

struct ResponsePayloadHeader {
    uint8_t command;
    uint8_t status;
};

struct VersionResponsePayload {
    uint8_t command;
    uint8_t status;
    uint8_t fw_major;
    uint8_t fw_minor;
    uint8_t fw_patch;
    uint8_t protocol_version;
};

struct RunSelfTestResponsePayload {
    uint8_t command;
    uint8_t status;

    int32_t baseline_x;
    int32_t baseline_y;
    int32_t baseline_z;

    int32_t st1_x;
    int32_t st1_y;
    int32_t st1_z;

    int32_t st2_x;
    int32_t st2_y;
    int32_t st2_z;

    int32_t delta_x;
    int32_t delta_y;
    int32_t delta_z;

    uint8_t passed;
};

struct SetNodeIdCommandPayload {
    uint8_t command;
    uint8_t node_id;
};

struct SetOdrCommandPayload {
    uint8_t command;
    uint16_t odr_hz;
};

struct SetRangeCommandPayload {
    uint8_t command;
    uint8_t range_g;
};

struct SetHighPassCommandPayload {
    uint8_t command;
    uint8_t high_pass_corner;
};

struct SetOffsetsCommandPayload {
    uint8_t command;
    int32_t offset_x;
    int32_t offset_y;
    int32_t offset_z;
};

struct SetFifoWatermarkCommandPayload {
    uint8_t command;
    uint8_t fifo_watermark;
};

struct SetBaudRateCommandPayload {
    uint8_t command;
    uint32_t baudrate;
};

struct CommissionDiscoverCommandPayload {
    uint8_t command;
    uint16_t slot_count;
    uint16_t slot_index;
};

struct CommissionAssignNodeIdCommandPayload {
    uint8_t command;
    uint8_t hardware_id[DEVICE_HARDWARE_ID_SIZE];
    uint8_t node_id;
};

struct CommissionIdentityResponsePayload {
    uint8_t command;
    uint8_t status;
    uint8_t node_id;
    uint8_t hardware_id[DEVICE_HARDWARE_ID_SIZE];
};

struct GetConfigResponsePayload {
    uint8_t command;
    uint8_t status;
    uint8_t node_id;
    uint32_t baudrate;
    uint16_t odr_hz;
    uint8_t range_g;
    int32_t offset_x;
    int32_t offset_y;
    int32_t offset_z;
    uint8_t fifo_watermark;
    uint16_t act_threshold;
    uint8_t act_count;
    uint8_t high_pass_corner;
};

struct ReadLatestCommandPayload {
    uint8_t command;
    uint16_t max_samples;
};

struct ReadFromSeqCommandPayload {
    uint8_t command;
    uint64_t start_seq;
    uint16_t max_samples;
};

struct GetStatusResponsePayload {
    uint8_t command;
    uint8_t status;
    uint8_t node_id;
    uint8_t node_state;
    uint16_t odr_hz;
    uint8_t range_g;
    uint32_t protocol_version;
    uint32_t firmware_version;
    uint32_t dropped_samples;
    uint32_t uptime_ms;
    uint64_t last_sample_seq;
    uint32_t last_progress_ms_ago;
    uint16_t last_error_code;
    uint8_t reset_cause;
    uint8_t diagnostic_flags;
    uint32_t fifo_poll_fallback_reads;
    uint32_t soft_recover_count;
    uint32_t no_data_with_irq;
    uint32_t no_data_without_irq;
};

struct GetTemperatureResponsePayload {
    uint8_t command;
    uint8_t status;
    uint16_t raw;
    float celsius;
};

struct GetBufferStateResponsePayload {
    uint8_t command;
    uint8_t status;
    uint64_t oldest_seq;
    uint64_t newest_seq;
    uint32_t stored_samples;
    uint32_t capacity_samples;
    uint32_t overwrite_count;
    uint64_t oldest_packet_first_seq;
    uint64_t newest_packet_last_seq;
    uint64_t committed_sample_seq;
    uint32_t queued_packets;
    uint32_t packet_capacity;
    uint32_t packet_overwrite_count;
};

struct GetStatsResponsePayload {
    uint8_t command;
    uint8_t status;
    uint64_t next_sample_seq;
    uint32_t pushed_samples;
    uint32_t dropped_samples;
    uint32_t sample_buffer_overwrite_count;
    uint32_t update_calls;
    uint32_t fifo_reads;
    uint32_t fifo_no_data;
    uint32_t sensor_errors;
    uint32_t fifo_irq_events;
    uint32_t fifo_batches;
    uint32_t fifo_samples_read;
    uint32_t rx_overflow_count;
    uint32_t packet_overwrite_count;
    uint64_t last_sample_seq;
    uint32_t last_progress_ms;
    uint32_t consecutive_no_data_reads;
    uint32_t consecutive_sensor_errors;
    uint32_t fifo_poll_fallback_reads;
    uint32_t no_data_with_irq;
    uint32_t no_data_without_irq;
    uint32_t soft_recover_count;
    uint32_t last_irq_event_ms;
    uint32_t last_soft_recover_ms;
};

struct GetDiagnosticInfoResponsePayload {
    uint8_t command;
    uint8_t status;
    uint32_t uptime_ms;
    uint8_t reset_cause;
    uint8_t live_usb_enabled;
    uint16_t stored_event_count;
    uint16_t event_capacity;
    uint32_t dropped_event_count;
    uint32_t first_event_id;
    uint32_t next_event_id;
    uint32_t last_error_event_id;
    uint16_t last_error_code;
};

struct GetFaultSnapshotResponsePayload {
    uint8_t command;
    uint8_t status;
    uint32_t event_id;
    uint32_t time_ms;
    uint16_t event_code;
    uint8_t severity;
    uint8_t reset_cause;
    uint64_t sample_seq;
    uint32_t last_progress_ms;
    uint32_t fifo_no_data;
    uint32_t sensor_errors;
    uint32_t dropped_samples;
    uint32_t rx_overflow_count;
    uint32_t packet_overwrite_count;
    int32_t arg0;
    int32_t arg1;
};

struct GetPersistentDiagnosticRecordResponsePayload {
    uint8_t command;
    uint8_t status;
    uint32_t generation;
    uint32_t boot_counter;
    uint32_t firmware_version;
    uint32_t event_id;
    uint32_t time_ms;
    uint16_t event_code;
    uint8_t severity;
    uint8_t repeat_count;
    uint8_t reset_cause;
    uint8_t reserved0;
    uint16_t reserved1;
    uint64_t sample_seq;
    uint32_t last_progress_ms;
    uint32_t fifo_no_data;
    uint32_t sensor_errors;
    uint32_t dropped_samples;
    uint32_t rx_overflow_count;
    uint32_t packet_overwrite_count;
    int32_t arg0;
    int32_t arg1;
};

struct ReadDiagnosticEventsCommandPayload {
    uint8_t command;
    uint32_t start_event_id;
    uint8_t max_events;
};

struct DiagnosticEventWirePayload {
    uint32_t event_id;
    uint32_t time_ms;
    uint16_t event_code;
    uint8_t severity;
    uint8_t repeat_count;
    uint64_t sample_seq;
    int32_t arg0;
    int32_t arg1;
};

struct ReadDiagnosticEventsResponseHeader {
    uint8_t command;
    uint8_t status;
    uint8_t returned_count;
    uint8_t reserved;
    uint32_t first_event_id;
    uint32_t next_event_id;
};

struct ReadSamplesResponseHeader {
    uint8_t command;
    uint8_t status;
    uint16_t sample_count;
    uint64_t first_seq;
};

struct WireSample32 {
    int32_t x;
    int32_t y;
    int32_t z;
};

struct GrantBurstReadCommandPayload {
    uint8_t command;
    uint64_t start_seq;
    uint16_t max_frames;
};

struct GrantBurstReadResponsePayload {
    uint8_t command;
    uint8_t status;
    uint64_t granted_start_seq;
    uint16_t granted_max_frames;
};

struct CommitReadUpToCommandPayload {
    uint8_t command;
    uint64_t last_sample_seq;
};

struct CommitReadUpToResponsePayload {
    uint8_t command;
    uint8_t status;
    uint64_t committed_sample_seq;
};

enum class SampleEncoding : uint8_t {
    RawXYZ24 = 1
};

struct BurstDataPayloadHeader {
    uint8_t command;
    uint8_t status;
    uint32_t packet_seq;
    uint64_t first_sample_seq;
    uint16_t sample_count;
    uint8_t sample_encoding;
};

#pragma pack(pop)

static_assert(sizeof(ResponsePayloadHeader) == 2, "ResponsePayloadHeader size mismatch");
static_assert(sizeof(SetBaudRateCommandPayload) == 5, "SetBaudRateCommandPayload size mismatch");
static_assert(sizeof(CommissionDiscoverCommandPayload) == 5, "CommissionDiscoverCommandPayload size mismatch");
static_assert(sizeof(CommissionAssignNodeIdCommandPayload) == 10, "CommissionAssignNodeIdCommandPayload size mismatch");
static_assert(sizeof(CommissionIdentityResponsePayload) == 11, "CommissionIdentityResponsePayload size mismatch");
static_assert(sizeof(ReadSamplesResponseHeader) == 12, "ReadSamplesResponseHeader size mismatch");
static_assert(sizeof(GrantBurstReadCommandPayload) == 11, "GrantBurstReadCommandPayload size mismatch");
static_assert(sizeof(GrantBurstReadResponsePayload) == 12, "GrantBurstReadResponsePayload size mismatch");
static_assert(sizeof(CommitReadUpToCommandPayload) == 9, "CommitReadUpToCommandPayload size mismatch");
static_assert(sizeof(CommitReadUpToResponsePayload) == 10, "CommitReadUpToResponsePayload size mismatch");
static_assert(sizeof(BurstDataPayloadHeader) == 17, "BurstDataPayloadHeader size mismatch");
