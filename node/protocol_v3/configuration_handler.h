#pragma once

#include <cstddef>
#include <cstdint>

#include "protocol_v3/configuration.h"
#include "runtime/configuration_runtime.h"

namespace ProtocolV3 {

struct ConfigurationHandlerResult { size_t payload_size = 0; };

class IConfigurationServiceHandler {
public:
    virtual ~IConfigurationServiceHandler() = default;
    virtual ConfigurationHandlerResult handle(
        uint8_t message,
        const uint8_t* request_payload,
        size_t request_payload_size,
        uint8_t* response_payload,
        size_t response_capacity) = 0;
};

template <typename Mailbox, size_t ResultCacheCapacity>
class ConfigurationServiceHandler final : public IConfigurationServiceHandler {
    static_assert(ResultCacheCapacity > 0,
                  "configuration result cache must be non-zero");

public:
    ConfigurationServiceHandler(
        Mailbox& mailbox,
        const runtime::ConfigurationResult& initial)
        : mailbox_(mailbox), observed_(initial) {}

    ConfigurationHandlerResult handle(
        uint8_t message,
        const uint8_t* request_payload,
        size_t request_payload_size,
        uint8_t* response_payload,
        size_t response_capacity) override {
        poll_result();
        if (response_payload == nullptr || response_capacity == 0) return {};

        if (message == kGetConfigMessageId) {
            if (request_payload_size != 0) {
                return error(ResponseStatus::MalformedRequest,
                             response_payload, response_capacity);
            }
            return encode_observed(response_payload, response_capacity);
        }
        if (message == kValidateConfigMessageId) {
            MeasurementConfiguration value{};
            if (decode_configuration(request_payload, request_payload_size,
                                     value) != ConfigurationCodecStatus::Ok) {
                return error(ResponseStatus::MalformedRequest,
                             response_payload, response_capacity);
            }
            const auto validation = validate_for_node(value);
            if (encode_validate_config_success(
                    validation, response_payload, response_capacity) !=
                ConfigurationCodecStatus::Ok) {
                return error(ResponseStatus::InternalError,
                             response_payload, response_capacity);
            }
            return {kValidateConfigResponseBytes};
        }
        if (message != kApplyConfigMessageId &&
            message != kResetConfigMessageId) {
            return error(ResponseStatus::UnsupportedMessage,
                         response_payload, response_capacity);
        }

        runtime::ConfigurationCommand command{};
        command.operation = message == kApplyConfigMessageId
            ? runtime::ConfigurationOperation::Apply
            : runtime::ConfigurationOperation::Reset;
        if (message == kApplyConfigMessageId) {
            ConfigOperationRequest decoded{};
            if (decode_apply_config_request(
                    request_payload, request_payload_size, decoded) !=
                ConfigurationCodecStatus::Ok) {
                return error(ResponseStatus::MalformedRequest,
                             response_payload, response_capacity);
            }
            command.operation_id = decoded.operation_id;
            command.expected_revision = decoded.expected_revision;
            command.configuration = to_runtime(decoded.configuration);
        } else {
            if (decode_reset_config_request(
                    request_payload, request_payload_size,
                    command.operation_id, command.expected_revision) !=
                ConfigurationCodecStatus::Ok) {
                return error(ResponseStatus::MalformedRequest,
                             response_payload, response_capacity);
            }
        }

        CacheEntry* cached = find_cached(command.operation_id);
        if (cached != nullptr) {
            if (!same_request(cached->request, command)) {
                return error(ResponseStatus::Conflict,
                             response_payload, response_capacity);
            }
            return encode_result(
                cached->result, response_payload, response_capacity);
        }
        if (active_) {
            if (active_command_.operation_id != command.operation_id) {
                return error(ResponseStatus::Busy,
                             response_payload, response_capacity);
            }
            if (!same_request(active_command_, command)) {
                return error(ResponseStatus::Conflict,
                             response_payload, response_capacity);
            }
            return error(ResponseStatus::Busy,
                         response_payload, response_capacity);
        }
        if (!mailbox_.try_send_command(command)) {
            return error(ResponseStatus::Busy,
                         response_payload, response_capacity);
        }
        active_command_ = command;
        active_ = true;
        return error(ResponseStatus::Busy,
                     response_payload, response_capacity);
    }

    bool poll_result() {
        runtime::ConfigurationResult result{};
        if (!mailbox_.try_receive_result(result)) return false;
        if (!active_ || result.operation_id != active_command_.operation_id) {
            faulted_ = true;
            return true;
        }
        observed_ = result;
        cache(active_command_, result);
        active_ = false;
        return true;
    }

    bool faulted() const { return faulted_; }

private:
    struct CacheEntry {
        runtime::ConfigurationCommand request{};
        runtime::ConfigurationResult result{};
        bool valid = false;
    };

    static runtime::RuntimeMeasurementConfiguration to_runtime(
        const MeasurementConfiguration& value) {
        return {
            static_cast<uint8_t>(value.output_rate),
            value.range_g,
            value.high_pass_corner,
            static_cast<uint8_t>(value.filter_profile),
            value.fifo_watermark,
            value.offset_x,
            value.offset_y,
            value.offset_z,
        };
    }

    static bool same_configuration(
        const runtime::RuntimeMeasurementConfiguration& left,
        const runtime::RuntimeMeasurementConfiguration& right) {
        return left.output_rate_code == right.output_rate_code &&
               left.range_g == right.range_g &&
               left.high_pass_corner == right.high_pass_corner &&
               left.filter_profile == right.filter_profile &&
               left.fifo_watermark == right.fifo_watermark &&
               left.offset_x == right.offset_x &&
               left.offset_y == right.offset_y &&
               left.offset_z == right.offset_z;
    }

    static bool same_request(
        const runtime::ConfigurationCommand& left,
        const runtime::ConfigurationCommand& right) {
        return left.operation_id == right.operation_id &&
               left.expected_revision == right.expected_revision &&
               left.operation == right.operation &&
               (left.operation == runtime::ConfigurationOperation::Reset ||
                same_configuration(left.configuration, right.configuration));
    }

    static ObservedConfiguration observed_from(
        const runtime::ConfigurationResult& value) {
        return {
            value.revision,
            {
                static_cast<OutputRateCode>(
                    value.configuration.output_rate_code),
                value.configuration.range_g,
                value.configuration.high_pass_corner,
                static_cast<ConfigurationFilterProfile>(
                    value.configuration.filter_profile),
                value.configuration.fifo_watermark,
                value.configuration.offset_x,
                value.configuration.offset_y,
                value.configuration.offset_z,
            },
            value.sensor_odr_register,
            value.decimation_factor,
        };
    }

    ConfigurationHandlerResult encode_observed(
        uint8_t* output, size_t capacity) const {
        if (faulted_ || encode_get_config_success(
                observed_from(observed_), output, capacity) !=
                ConfigurationCodecStatus::Ok) {
            return error(ResponseStatus::InternalError, output, capacity);
        }
        return {kGetConfigResponseBytes};
    }

    static ConfigurationHandlerResult encode_result(
        const runtime::ConfigurationResult& value,
        uint8_t* output,
        size_t capacity) {
        const ConfigOperationResult wire{
            value.operation_id,
            static_cast<ConfigOperationCode>(value.code),
            observed_from(value),
            value.effective_sample_seq,
        };
        if (encode_config_operation_success(wire, output, capacity) !=
            ConfigurationCodecStatus::Ok) {
            return error(ResponseStatus::InternalError, output, capacity);
        }
        return {kConfigOperationResponseBytes};
    }

    static ConfigurationHandlerResult error(
        ResponseStatus status, uint8_t* output, size_t capacity) {
        if (output == nullptr || capacity == 0) return {};
        output[0] = static_cast<uint8_t>(status);
        return {1};
    }

    CacheEntry* find_cached(uint32_t operation_id) {
        for (auto& entry : cache_) {
            if (entry.valid && entry.request.operation_id == operation_id) {
                return &entry;
            }
        }
        return nullptr;
    }

    void cache(const runtime::ConfigurationCommand& request,
               const runtime::ConfigurationResult& result) {
        cache_[next_cache_] = {request, result, true};
        next_cache_ = (next_cache_ + 1u) % ResultCacheCapacity;
    }

    Mailbox& mailbox_;
    runtime::ConfigurationResult observed_{};
    runtime::ConfigurationCommand active_command_{};
    CacheEntry cache_[ResultCacheCapacity]{};
    size_t next_cache_ = 0;
    bool active_ = false;
    bool faulted_ = false;
};

}  // namespace ProtocolV3
