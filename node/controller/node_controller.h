#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>

#include "acquisition/acquisition_engine.h"
#include "boot_shared/boot_runtime_api.h"
#include "common/device_identity.h"
#include "common/node_types.h"
#include "common/protocol_ids.h"
#include "common/sensor_types.h"
#include "config/config_manager.h"
#include "interfaces/i_data_plane.h"
#include "interfaces/i_temperature_sensor.h"
#include "storage/stored_sample.h"
#include "transport/command_payloads.h"
#include "transport/command_types.h"
#include "transport/frame_types.h"
#include "transport/rs485_port.h"
#include "transport/status_codes.h"

template <size_t BufferCapacity>
class NodeController {
public:
    enum class DeferredAction : uint8_t {
        None = 0,
        Restart,
        EnterBootloader,
        ApplyBaudRate
    };

    NodeController(ConfigManager& config,
                   AcquisitionEngine<BufferCapacity>& acquisition,
                   ITemperatureSensor& temperature_sensor,
                   IDataPlane& data_plane,
                   Rs485Port* rs485_port = nullptr)
        : config_(config),
          acquisition_(acquisition),
          temperature_sensor_(temperature_sensor),
          data_plane_(data_plane),
          rs485_port_(rs485_port) {
    }

    bool init() {
        const auto st = acquisition_.init(config_.current());
        init_status_ = st;
        return st == SensorStatus::Ok;
    }

    SensorStatus init_status() const {
        return init_status_;
    }

    uint8_t node_id() const {
        return config_.current().node_id;
    }

    bool is_unassigned() const {
        return config_.current().node_id == UNASSIGNED_NODE_ID;
    }

    bool accepts_broadcast_command(const uint8_t* payload, size_t payload_len) const {
        if (payload == nullptr || payload_len < 1) {
            return false;
        }

        switch (static_cast<CommandType>(payload[0])) {
            case CommandType::CommissionDiscover:
            case CommandType::CommissionAssignNodeId:
                return is_unassigned();
            default:
                return false;
        }
    }

    DeferredAction take_deferred_action() {
        const DeferredAction action = deferred_action_;
        deferred_action_ = DeferredAction::None;
        return action;
    }

    uint32_t take_deferred_baudrate() {
        const uint32_t baudrate = deferred_baudrate_;
        deferred_baudrate_ = 0;
        return baudrate;
    }

    StatusCode handle_command(uint8_t command,
                              uint8_t source,
                              const uint8_t* payload,
                              size_t payload_len,
                              uint8_t* response,
                              size_t& response_len) {
        response_len = 0;

        switch (static_cast<CommandType>(command)) {
            case CommandType::Ping:
                return build_simple_ok(command, response, response_len);

            case CommandType::Restart:
                return handle_restart(response, response_len);

            case CommandType::EnterBootloader:
                return handle_enter_bootloader(response, response_len);

            case CommandType::GetVersion:
                return handle_get_version(response, response_len);

            case CommandType::RunSelfTest:
                return handle_run_self_test(response, response_len);

            case CommandType::GetConfig:
                return handle_get_config(response, response_len);

            case CommandType::SetNodeId:
                return handle_set_node_id(payload, payload_len, response, response_len);

            case CommandType::SetOdr:
                return handle_set_odr(payload, payload_len, response, response_len);

            case CommandType::SetRange:
                return handle_set_range(payload, payload_len, response, response_len);

            case CommandType::SetHighPass:
                return handle_set_high_pass(payload, payload_len, response, response_len);

            case CommandType::SetOffsets:
                return handle_set_offsets(payload, payload_len, response, response_len);

            case CommandType::SetFifoWatermark:
                return handle_set_fifo_watermark(payload, payload_len, response, response_len);

            case CommandType::SetBaudRate:
                return handle_set_baudrate(payload, payload_len, response, response_len);

            case CommandType::CommissionDiscover:
                return handle_commission_discover(payload, payload_len, response, response_len);

            case CommandType::CommissionAssignNodeId:
                return handle_commission_assign(payload, payload_len, response, response_len);

            case CommandType::SaveConfig:
                return handle_save_config(response, response_len);

            case CommandType::LoadConfig:
                return handle_load_config(response, response_len);

            case CommandType::ResetConfigToDefaults:
                return handle_reset_config_to_defaults(response, response_len);

            case CommandType::GetStatus:
                return handle_get_status(response, response_len);

            case CommandType::GetBufferState:
                return handle_get_buffer_state(response, response_len);

            case CommandType::GetStats:
                return handle_get_stats(response, response_len);

            case CommandType::GetTemperature:
                return handle_get_temperature(response, response_len);

            case CommandType::ReadLatest:
                return handle_read_latest(payload, payload_len, response, response_len);

            case CommandType::ReadFromSeq:
                return handle_read_from_seq(payload, payload_len, response, response_len);

            case CommandType::GrantBurstRead:
                return handle_grant_burst(
                    source,
                    payload,
                    payload_len,
                    response,
                    response_len
                );

            case CommandType::CommitReadUpTo:
                return handle_commit_read_up_to(payload, payload_len, response, response_len);

            default:
                return build_error_response(
                    command,
                    StatusCode::Unsupported,
                    response,
                    response_len
                );
        }
    }

private:
    static constexpr size_t kMaxInlineReadSamples =
        (FRAME_MAX_PAYLOAD_SIZE - sizeof(ReadSamplesResponseHeader)) /
        sizeof(WireSample32);

    static constexpr int32_t kMinOffset = -32768;
    static constexpr int32_t kMaxOffset = 32767;

private:
    static StatusCode map_sensor_status(SensorStatus status) {
        switch (status) {
            case SensorStatus::Ok:
                return StatusCode::Ok;
            case SensorStatus::Busy:
                return StatusCode::Busy;
            case SensorStatus::InvalidParam:
                return StatusCode::InvalidParam;
            case SensorStatus::NotSupported:
                return StatusCode::Unsupported;
            case SensorStatus::NoData:
                return StatusCode::NoData;
            default:
                return StatusCode::SensorError;
        }
    }

    static bool is_valid_node_id(uint8_t node_id) {
        return node_id != UNASSIGNED_NODE_ID &&
               node_id != HOST_NODE_ID &&
               node_id != BROADCAST_NODE_ID;
    }

    static bool is_valid_fifo_watermark(uint8_t fifo_watermark) {
        return fifo_watermark >= 3 &&
               fifo_watermark <= 96 &&
               (fifo_watermark % 3) == 0;
    }

    static bool is_valid_odr_hz(uint16_t odr_hz) {
        switch (odr_hz) {
            case 4000:
            case 2000:
            case 1000:
            case 500:
            case 250:
            case 125:
                return true;
            default:
                return false;
        }
    }

    static bool is_valid_high_pass_corner(uint8_t high_pass_corner) {
        return high_pass_corner <= 7;
    }

    static bool is_valid_baudrate(uint32_t baudrate) {
        switch (baudrate) {
            case 9600u:
            case 19200u:
            case 38400u:
            case 57600u:
            case 115200u:
                return true;
            default:
                return false;
        }
    }

    static bool is_valid_offset(int32_t offset) {
        return offset >= kMinOffset && offset <= kMaxOffset;
    }

    StatusCode build_error_response(uint8_t cmd,
                                    StatusCode status,
                                    uint8_t* out,
                                    size_t& out_len) {
        ResponsePayloadHeader resp{};
        resp.command = cmd;
        resp.status = static_cast<uint8_t>(status);

        std::memcpy(out, &resp, sizeof(resp));
        out_len = sizeof(resp);
        return status;
    }

    StatusCode build_simple_ok(uint8_t cmd,
                               uint8_t* out,
                               size_t& out_len) {
        ResponsePayloadHeader resp{};
        resp.command = cmd;
        resp.status = static_cast<uint8_t>(StatusCode::Ok);

        std::memcpy(out, &resp, sizeof(resp));
        out_len = sizeof(resp);
        return StatusCode::Ok;
    }

    StatusCode apply_runtime_config(const DeviceConfig& config,
                                    uint8_t cmd,
                                    uint8_t* out,
                                    size_t& out_len) {
        const SensorStatus sensor_status =
            acquisition_.reload_runtime_config(config);
        if (sensor_status != SensorStatus::Ok) {
            return build_error_response(
                cmd,
                map_sensor_status(sensor_status),
                out,
                out_len
            );
        }

        config_.replace_device_config(config);
        return build_simple_ok(cmd, out, out_len);
    }

    bool apply_runtime_config_only(const DeviceConfig& config) {
        const SensorStatus sensor_status =
            acquisition_.reload_runtime_config(config);
        if (sensor_status != SensorStatus::Ok) {
            return false;
        }

        config_.replace_device_config(config);
        return true;
    }

    StatusCode build_commission_identity_response(uint8_t command,
                                                  uint8_t node_id,
                                                  uint8_t* out,
                                                  size_t& out_len) {
        CommissionIdentityResponsePayload resp{};
        resp.command = command;
        resp.status = static_cast<uint8_t>(StatusCode::Ok);
        resp.node_id = node_id;
        const DeviceHardwareId hardware_id = read_device_hardware_id();
        for (size_t i = 0; i < DEVICE_HARDWARE_ID_SIZE; ++i) {
            resp.hardware_id[i] = hardware_id.bytes[i];
        }

        std::memcpy(out, &resp, sizeof(resp));
        out_len = sizeof(resp);
        return StatusCode::Ok;
    }

    bool matches_discovery_slot(const CommissionDiscoverCommandPayload& cmd) const {
        if (cmd.slot_count == 0u) {
            return false;
        }

        if (cmd.slot_index >= cmd.slot_count) {
            return false;
        }

        const DeviceHardwareId hardware_id = read_device_hardware_id();
        const uint32_t slot =
            device_hardware_id_hash32(hardware_id) % static_cast<uint32_t>(cmd.slot_count);
        return slot == cmd.slot_index;
    }

    StatusCode build_read_samples_response(uint8_t cmd,
                                           const StoredSample* samples,
                                           size_t sample_count,
                                           uint8_t* out,
                                           size_t& out_len) {
        if (samples == nullptr || sample_count == 0) {
            return build_error_response(cmd, StatusCode::NoData, out, out_len);
        }

        ReadSamplesResponseHeader header{};
        header.command = cmd;
        header.status = static_cast<uint8_t>(StatusCode::Ok);
        header.sample_count = static_cast<uint16_t>(sample_count);
        header.first_seq = samples[0].sample_seq;

        size_t offset = 0;
        std::memcpy(out + offset, &header, sizeof(header));
        offset += sizeof(header);

        for (size_t i = 0; i < sample_count; ++i) {
            WireSample32 sample{};
            sample.x = samples[i].x;
            sample.y = samples[i].y;
            sample.z = samples[i].z;

            std::memcpy(out + offset, &sample, sizeof(sample));
            offset += sizeof(sample);
        }

        out_len = offset;
        return StatusCode::Ok;
    }

    StatusCode handle_restart(uint8_t* out, size_t& out_len) {
        deferred_action_ = DeferredAction::Restart;
        return build_simple_ok(static_cast<uint8_t>(CommandType::Restart), out, out_len);
    }

    StatusCode handle_enter_bootloader(uint8_t* out, size_t& out_len) {
        if (!boot::app_request_enter_bootloader()) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::EnterBootloader),
                StatusCode::StorageError,
                out,
                out_len
            );
        }

        deferred_action_ = DeferredAction::EnterBootloader;
        return build_simple_ok(
            static_cast<uint8_t>(CommandType::EnterBootloader),
            out,
            out_len
        );
    }

    StatusCode handle_get_version(uint8_t* out, size_t& out_len) {
        VersionResponsePayload resp{};
        resp.command = static_cast<uint8_t>(CommandType::GetVersion);
        resp.status = static_cast<uint8_t>(StatusCode::Ok);
        resp.fw_major = FW_VERSION_MAJOR;
        resp.fw_minor = FW_VERSION_MINOR;
        resp.fw_patch = FW_VERSION_PATCH;
        resp.protocol_version = PROTOCOL_VERSION;

        std::memcpy(out, &resp, sizeof(resp));
        out_len = sizeof(resp);
        return StatusCode::Ok;
    }

    StatusCode handle_run_self_test(uint8_t* out, size_t& out_len) {
        SelfTestResult result{};
        const SensorStatus st = acquisition_.run_self_test(result);
        if (st != SensorStatus::Ok) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::RunSelfTest),
                map_sensor_status(st),
                out,
                out_len
            );
        }

        RunSelfTestResponsePayload resp{};
        resp.command = static_cast<uint8_t>(CommandType::RunSelfTest);
        resp.status = static_cast<uint8_t>(StatusCode::Ok);
        resp.baseline_x = result.baseline.x;
        resp.baseline_y = result.baseline.y;
        resp.baseline_z = result.baseline.z;
        resp.st1_x = result.st1.x;
        resp.st1_y = result.st1.y;
        resp.st1_z = result.st1.z;
        resp.st2_x = result.st2.x;
        resp.st2_y = result.st2.y;
        resp.st2_z = result.st2.z;
        resp.delta_x = result.delta.x;
        resp.delta_y = result.delta.y;
        resp.delta_z = result.delta.z;
        resp.passed = result.passed ? 1u : 0u;

        std::memcpy(out, &resp, sizeof(resp));
        out_len = sizeof(resp);
        return StatusCode::Ok;
    }

    StatusCode handle_get_config(uint8_t* out, size_t& out_len) {
        const auto cfg = config_.current();

        GetConfigResponsePayload resp{};
        resp.command = static_cast<uint8_t>(CommandType::GetConfig);
        resp.status = static_cast<uint8_t>(StatusCode::Ok);
        resp.node_id = cfg.node_id;
        resp.baudrate = cfg.baudrate;
        resp.odr_hz = cfg.odr_hz;
        resp.range_g = cfg.range_g;
        resp.offset_x = cfg.offset_x;
        resp.offset_y = cfg.offset_y;
        resp.offset_z = cfg.offset_z;
        resp.fifo_watermark = cfg.fifo_watermark;
        resp.act_threshold = cfg.act_threshold;
        resp.act_count = cfg.act_count;
        resp.high_pass_corner = cfg.high_pass_corner;

        std::memcpy(out, &resp, sizeof(resp));
        out_len = sizeof(resp);
        return StatusCode::Ok;
    }

    StatusCode handle_set_node_id(const uint8_t* payload,
                                  size_t len,
                                  uint8_t* out,
                                  size_t& out_len) {
        if (payload == nullptr || len < sizeof(SetNodeIdCommandPayload)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::SetNodeId),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        const auto* cmd = reinterpret_cast<const SetNodeIdCommandPayload*>(payload);
        if (!is_valid_node_id(cmd->node_id)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::SetNodeId),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        DeviceConfig config = config_.current();
        config.node_id = cmd->node_id;
        return apply_runtime_config(
            config,
            static_cast<uint8_t>(CommandType::SetNodeId),
            out,
            out_len
        );
    }

    StatusCode handle_commission_discover(const uint8_t* payload,
                                          size_t len,
                                          uint8_t* out,
                                          size_t& out_len) {
        if (!is_unassigned()) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::CommissionDiscover),
                StatusCode::InvalidState,
                out,
                out_len
            );
        }

        if (payload == nullptr || len < sizeof(CommissionDiscoverCommandPayload)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::CommissionDiscover),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        const auto* cmd =
            reinterpret_cast<const CommissionDiscoverCommandPayload*>(payload);
        if (!matches_discovery_slot(*cmd)) {
            out_len = 0;
            return StatusCode::NoData;
        }

        return build_commission_identity_response(
            static_cast<uint8_t>(CommandType::CommissionDiscover),
            config_.current().node_id,
            out,
            out_len
        );
    }

    StatusCode handle_commission_assign(const uint8_t* payload,
                                        size_t len,
                                        uint8_t* out,
                                        size_t& out_len) {
        if (!is_unassigned()) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::CommissionAssignNodeId),
                StatusCode::InvalidState,
                out,
                out_len
            );
        }

        if (payload == nullptr || len < sizeof(CommissionAssignNodeIdCommandPayload)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::CommissionAssignNodeId),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        const auto* cmd =
            reinterpret_cast<const CommissionAssignNodeIdCommandPayload*>(payload);
        if (!is_valid_node_id(cmd->node_id)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::CommissionAssignNodeId),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        const DeviceHardwareId hardware_id = read_device_hardware_id();
        if (!device_hardware_id_equals(hardware_id, cmd->hardware_id)) {
            out_len = 0;
            return StatusCode::NoData;
        }

        const DeviceConfig previous_config = config_.current();
        DeviceConfig next_config = previous_config;
        next_config.node_id = cmd->node_id;

        if (!apply_runtime_config_only(next_config)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::CommissionAssignNodeId),
                StatusCode::SensorError,
                out,
                out_len
            );
        }

        if (!config_.save() || !boot::app_sync_boot_settings(next_config.node_id)) {
            (void)apply_runtime_config_only(previous_config);
            (void)config_.save();
            (void)boot::app_sync_boot_settings(previous_config.node_id);
            return build_error_response(
                static_cast<uint8_t>(CommandType::CommissionAssignNodeId),
                StatusCode::StorageError,
                out,
                out_len
            );
        }

        return build_commission_identity_response(
            static_cast<uint8_t>(CommandType::CommissionAssignNodeId),
            next_config.node_id,
            out,
            out_len
        );
    }

    StatusCode handle_set_odr(const uint8_t* payload,
                              size_t len,
                              uint8_t* out,
                              size_t& out_len) {
        if (payload == nullptr || len < sizeof(SetOdrCommandPayload)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::SetOdr),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        const auto* cmd = reinterpret_cast<const SetOdrCommandPayload*>(payload);
        if (!is_valid_odr_hz(cmd->odr_hz)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::SetOdr),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        DeviceConfig config = config_.current();
        config.odr_hz = cmd->odr_hz;
        return apply_runtime_config(
            config,
            static_cast<uint8_t>(CommandType::SetOdr),
            out,
            out_len
        );
    }

    StatusCode handle_set_range(const uint8_t* payload,
                                size_t len,
                                uint8_t* out,
                                size_t& out_len) {
        if (payload == nullptr || len < sizeof(SetRangeCommandPayload)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::SetRange),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        const auto* cmd = reinterpret_cast<const SetRangeCommandPayload*>(payload);
        DeviceConfig config = config_.current();
        config.range_g = cmd->range_g;
        return apply_runtime_config(
            config,
            static_cast<uint8_t>(CommandType::SetRange),
            out,
            out_len
        );
    }

    StatusCode handle_set_high_pass(const uint8_t* payload,
                                    size_t len,
                                    uint8_t* out,
                                    size_t& out_len) {
        if (payload == nullptr || len < sizeof(SetHighPassCommandPayload)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::SetHighPass),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        const auto* cmd = reinterpret_cast<const SetHighPassCommandPayload*>(payload);
        if (!is_valid_high_pass_corner(cmd->high_pass_corner)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::SetHighPass),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        DeviceConfig config = config_.current();
        config.high_pass_corner = cmd->high_pass_corner;
        return apply_runtime_config(
            config,
            static_cast<uint8_t>(CommandType::SetHighPass),
            out,
            out_len
        );
    }

    StatusCode handle_set_offsets(const uint8_t* payload,
                                  size_t len,
                                  uint8_t* out,
                                  size_t& out_len) {
        if (payload == nullptr || len < sizeof(SetOffsetsCommandPayload)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::SetOffsets),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        const auto* cmd = reinterpret_cast<const SetOffsetsCommandPayload*>(payload);
        if (!is_valid_offset(cmd->offset_x) ||
            !is_valid_offset(cmd->offset_y) ||
            !is_valid_offset(cmd->offset_z)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::SetOffsets),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        DeviceConfig config = config_.current();
        config.offset_x = cmd->offset_x;
        config.offset_y = cmd->offset_y;
        config.offset_z = cmd->offset_z;
        return apply_runtime_config(
            config,
            static_cast<uint8_t>(CommandType::SetOffsets),
            out,
            out_len
        );
    }

    StatusCode handle_set_fifo_watermark(const uint8_t* payload,
                                         size_t len,
                                         uint8_t* out,
                                         size_t& out_len) {
        if (payload == nullptr || len < sizeof(SetFifoWatermarkCommandPayload)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::SetFifoWatermark),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        const auto* cmd =
            reinterpret_cast<const SetFifoWatermarkCommandPayload*>(payload);
        if (!is_valid_fifo_watermark(cmd->fifo_watermark)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::SetFifoWatermark),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        DeviceConfig config = config_.current();
        config.fifo_watermark = cmd->fifo_watermark;
        return apply_runtime_config(
            config,
            static_cast<uint8_t>(CommandType::SetFifoWatermark),
            out,
            out_len
        );
    }

    StatusCode handle_set_baudrate(const uint8_t* payload,
                                   size_t len,
                                   uint8_t* out,
                                   size_t& out_len) {
        if (payload == nullptr || len < sizeof(SetBaudRateCommandPayload)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::SetBaudRate),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        const auto* cmd =
            reinterpret_cast<const SetBaudRateCommandPayload*>(payload);
        if (!is_valid_baudrate(cmd->baudrate)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::SetBaudRate),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        DeviceConfig config = config_.current();
        config.baudrate = cmd->baudrate;
        config_.replace_device_config(config);

        deferred_baudrate_ = cmd->baudrate;
        deferred_action_ = DeferredAction::ApplyBaudRate;
        return build_simple_ok(
            static_cast<uint8_t>(CommandType::SetBaudRate),
            out,
            out_len
        );
    }

    StatusCode handle_save_config(uint8_t* out, size_t& out_len) {
        if (!config_.save()) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::SaveConfig),
                StatusCode::SaveFailed,
                out,
                out_len
            );
        }

        if (!boot::app_sync_boot_settings(config_.current().node_id)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::SaveConfig),
                StatusCode::StorageError,
                out,
                out_len
            );
        }

        return build_simple_ok(static_cast<uint8_t>(CommandType::SaveConfig), out, out_len);
    }

    StatusCode handle_load_config(uint8_t* out, size_t& out_len) {
        DeviceConfig config{};
        if (!config_.load_device_config(config)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::LoadConfig),
                StatusCode::LoadFailed,
                out,
                out_len
            );
        }

        return apply_runtime_config(
            config,
            static_cast<uint8_t>(CommandType::LoadConfig),
            out,
            out_len
        );
    }

    StatusCode handle_reset_config_to_defaults(uint8_t* out, size_t& out_len) {
        const DeviceConfig config = ConfigManager::default_device_config();
        return apply_runtime_config(
            config,
            static_cast<uint8_t>(CommandType::ResetConfigToDefaults),
            out,
            out_len
        );
    }

    StatusCode handle_get_status(uint8_t* out, size_t& out_len) {
        GetStatusResponsePayload resp{};
        resp.command = static_cast<uint8_t>(CommandType::GetStatus);
        resp.status = static_cast<uint8_t>(StatusCode::Ok);
        resp.node_id = config_.current().node_id;
        if (init_status_ != SensorStatus::Ok) {
            resp.node_state = static_cast<uint8_t>(NodeState::Fault);
        } else {
            resp.node_state = static_cast<uint8_t>(
                acquisition_.is_paused() ? NodeState::Idle : NodeState::Acquiring
            );
        }
        resp.odr_hz = config_.current().odr_hz;
        resp.range_g = config_.current().range_g;
        resp.protocol_version = PROTOCOL_VERSION;
        resp.firmware_version =
            (static_cast<uint32_t>(FW_VERSION_MAJOR) << 16) |
            (static_cast<uint32_t>(FW_VERSION_MINOR) << 8) |
            static_cast<uint32_t>(FW_VERSION_PATCH);
        resp.dropped_samples = acquisition_.stats().dropped_samples;

        std::memcpy(out, &resp, sizeof(resp));
        out_len = sizeof(resp);
        return StatusCode::Ok;
    }

    StatusCode handle_get_buffer_state(uint8_t* out, size_t& out_len) {
        const auto st = acquisition_.buffer_state();
        const auto dp = data_plane_.state();

        GetBufferStateResponsePayload resp{};
        resp.command = static_cast<uint8_t>(CommandType::GetBufferState);
        resp.status = static_cast<uint8_t>(StatusCode::Ok);
        resp.oldest_seq = st.oldest_seq;
        resp.newest_seq = st.newest_seq;
        resp.stored_samples = static_cast<uint32_t>(st.stored_samples);
        resp.capacity_samples = static_cast<uint32_t>(st.capacity_samples);
        resp.overwrite_count = st.overwrite_count;
        resp.oldest_packet_first_seq = dp.oldest_packet_first_seq;
        resp.newest_packet_last_seq = dp.newest_packet_last_seq;
        resp.committed_sample_seq = dp.committed_sample_seq;
        resp.queued_packets = dp.queued_packets;
        resp.packet_capacity = dp.packet_capacity;
        resp.packet_overwrite_count = dp.packet_overwrite_count;

        std::memcpy(out, &resp, sizeof(resp));
        out_len = sizeof(resp);
        return StatusCode::Ok;
    }

    StatusCode handle_get_stats(uint8_t* out, size_t& out_len) {
        const auto st = acquisition_.stats();

        GetStatsResponsePayload resp{};
        resp.command = static_cast<uint8_t>(CommandType::GetStats);
        resp.status = static_cast<uint8_t>(StatusCode::Ok);
        resp.next_sample_seq = st.next_sample_seq;
        resp.pushed_samples = st.pushed_samples;
        resp.dropped_samples = st.dropped_samples;
        resp.sample_buffer_overwrite_count = st.sample_buffer_overwrite_count;
        resp.update_calls = st.update_calls;
        resp.fifo_reads = st.fifo_reads;
        resp.fifo_no_data = st.fifo_no_data;
        resp.sensor_errors = st.sensor_errors;
        resp.fifo_irq_events = st.fifo_irq_events;
        resp.fifo_batches = st.fifo_batches;
        resp.fifo_samples_read = st.fifo_samples_read;
        resp.rx_overflow_count = (rs485_port_ != nullptr)
            ? rs485_port_->rx_overflow_count()
            : 0;
        resp.packet_overwrite_count = data_plane_.state().packet_overwrite_count;

        std::memcpy(out, &resp, sizeof(resp));
        out_len = sizeof(resp);
        return StatusCode::Ok;
    }

    StatusCode handle_get_temperature(uint8_t* out, size_t& out_len) {
        TemperatureSample t{};
        const auto st = temperature_sensor_.read_temperature(t);
        if (st != SensorStatus::Ok) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::GetTemperature),
                map_sensor_status(st),
                out,
                out_len
            );
        }

        GetTemperatureResponsePayload resp{};
        resp.command = static_cast<uint8_t>(CommandType::GetTemperature);
        resp.status = static_cast<uint8_t>(StatusCode::Ok);
        resp.raw = t.raw;
        resp.celsius = t.celsius;

        std::memcpy(out, &resp, sizeof(resp));
        out_len = sizeof(resp);
        return StatusCode::Ok;
    }

    StatusCode handle_read_latest(const uint8_t* payload,
                                  size_t len,
                                  uint8_t* out,
                                  size_t& out_len) {
        if (payload == nullptr || len < sizeof(ReadLatestCommandPayload)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::ReadLatest),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        const auto* cmd = reinterpret_cast<const ReadLatestCommandPayload*>(payload);
        if (cmd->max_samples == 0) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::ReadLatest),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        const size_t requested =
            (cmd->max_samples < kMaxInlineReadSamples)
                ? cmd->max_samples
                : kMaxInlineReadSamples;

        StoredSample samples[kMaxInlineReadSamples]{};
        const size_t read = acquisition_.read_latest(samples, requested);
        return build_read_samples_response(
            static_cast<uint8_t>(CommandType::ReadLatest),
            samples,
            read,
            out,
            out_len
        );
    }

    StatusCode handle_read_from_seq(const uint8_t* payload,
                                    size_t len,
                                    uint8_t* out,
                                    size_t& out_len) {
        if (payload == nullptr || len < sizeof(ReadFromSeqCommandPayload)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::ReadFromSeq),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        const auto* cmd = reinterpret_cast<const ReadFromSeqCommandPayload*>(payload);
        if (cmd->max_samples == 0) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::ReadFromSeq),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        const size_t requested =
            (cmd->max_samples < kMaxInlineReadSamples)
                ? cmd->max_samples
                : kMaxInlineReadSamples;

        StoredSample samples[kMaxInlineReadSamples]{};
        const size_t read = acquisition_.read_from_seq(
            cmd->start_seq,
            samples,
            requested
        );

        return build_read_samples_response(
            static_cast<uint8_t>(CommandType::ReadFromSeq),
            samples,
            read,
            out,
            out_len
        );
    }

    StatusCode handle_grant_burst(uint8_t requester,
                                  const uint8_t* payload,
                                  size_t len,
                                  uint8_t* out,
                                  size_t& out_len) {
        if (payload == nullptr || len < sizeof(GrantBurstReadCommandPayload)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::GrantBurstRead),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        const auto* cmd =
            reinterpret_cast<const GrantBurstReadCommandPayload*>(payload);

        const auto st = data_plane_.start_burst(
            cmd->start_seq,
            cmd->max_frames,
            requester
        );

        GrantBurstReadResponsePayload resp{};
        resp.command = static_cast<uint8_t>(CommandType::GrantBurstRead);
        resp.status = static_cast<uint8_t>(st);
        resp.granted_start_seq = cmd->start_seq;
        resp.granted_max_frames = cmd->max_frames;

        std::memcpy(out, &resp, sizeof(resp));
        out_len = sizeof(resp);
        return st;
    }

    StatusCode handle_commit_read_up_to(const uint8_t* payload,
                                        size_t len,
                                        uint8_t* out,
                                        size_t& out_len) {
        if (payload == nullptr || len < sizeof(CommitReadUpToCommandPayload)) {
            return build_error_response(
                static_cast<uint8_t>(CommandType::CommitReadUpTo),
                StatusCode::InvalidParam,
                out,
                out_len
            );
        }

        const auto* cmd =
            reinterpret_cast<const CommitReadUpToCommandPayload*>(payload);

        const auto st = data_plane_.commit_read_up_to(cmd->last_sample_seq);
        const auto dp = data_plane_.state();

        CommitReadUpToResponsePayload resp{};
        resp.command = static_cast<uint8_t>(CommandType::CommitReadUpTo);
        resp.status = static_cast<uint8_t>(st);
        resp.committed_sample_seq = dp.committed_sample_seq;

        std::memcpy(out, &resp, sizeof(resp));
        out_len = sizeof(resp);
        return st;
    }

private:
    ConfigManager& config_;
    AcquisitionEngine<BufferCapacity>& acquisition_;
    ITemperatureSensor& temperature_sensor_;
    IDataPlane& data_plane_;
    Rs485Port* rs485_port_ = nullptr;
    SensorStatus init_status_ = SensorStatus::NotInitialized;
    DeferredAction deferred_action_ = DeferredAction::None;
    uint32_t deferred_baudrate_ = 0;
};
