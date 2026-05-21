#pragma once

#include <cstdint>

#include "common/device_identity.h"
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
