#pragma once

#include <cstddef>
#include <cstdint>

#include "protocol_v3/lifecycle.h"
#include "runtime/deferred_system_action.h"
#include "runtime/system_operation_runtime.h"

namespace ProtocolV3 {

struct LifecycleHandlerResult {
    size_t payload_size = 0;
    bool post_transmit_required = false;
    uint64_t post_transmit_token = 0;
};

class ILifecycleServiceHandler {
public:
    virtual ~ILifecycleServiceHandler() = default;
    virtual LifecycleHandlerResult handle(
        uint8_t message, const uint8_t* request_payload,
        size_t request_payload_size, uint8_t* response_payload,
        size_t response_capacity) = 0;
    virtual bool settle_transmission(uint64_t token, bool transmitted) = 0;
};

struct LifecycleServiceState {
    bool operation_active = false;
    bool faulted = false;
    uint32_t active_operation_id = 0;
    size_t cached_results = 0;
    uint32_t accepted_operations = 0;
    uint32_t completed_operations = 0;
    uint32_t in_progress_results = 0;
    uint32_t busy_responses = 0;
    uint32_t conflict_responses = 0;
    uint32_t unexpected_results = 0;
    uint32_t update_actions_armed = 0;
    uint32_t restart_actions_armed = 0;
};

template <typename SensorMailbox,
          typename SystemMailbox,
          size_t ResultCacheCapacity>
class LifecycleServiceHandler final : public ILifecycleServiceHandler {
    static_assert(ResultCacheCapacity > 0,
                  "lifecycle result cache must be non-zero");

public:
    LifecycleServiceHandler(SensorMailbox& sensor_mailbox,
                            SystemMailbox& system_mailbox,
                            runtime::DeferredSystemAction& system_action)
        : sensor_mailbox_(sensor_mailbox),
          system_mailbox_(system_mailbox),
          system_action_(system_action) {}

    bool poll_result() {
        runtime::SensorRuntimeResult result{};
        if (!has_active_) {
            if (!sensor_mailbox_.try_receive_result(result) &&
                !system_mailbox_.try_receive_result(result)) {
                return false;
            }
            increment(unexpected_results_);
            faulted_ = true;
            return true;
        }
        const bool received = active_.system_operation
            ? system_mailbox_.try_receive_result(result)
            : sensor_mailbox_.try_receive_result(result);
        if (!received) return false;
        if (result.operation_id != active_.operation_id) {
            increment(unexpected_results_);
            faulted_ = true;
            return true;
        }

        last_result_ = result;
        last_request_ = active_;
        has_last_result_ = true;
        has_active_ = false;
        if (result.code == runtime::SensorRuntimeResultCode::InProgress) {
            increment(in_progress_results_);
            return true;
        }

        runtime::DeferredSystemActionKind action =
            runtime::DeferredSystemActionKind::None;
        if (result.code == runtime::SensorRuntimeResultCode::Ok) {
            if (active_.message == kPrepareFirmwareUpdateMessageId &&
                (result.state == runtime::SensorRuntimeState::Maintenance ||
                 result.state == runtime::SensorRuntimeState::Faulted)) {
                action = runtime::DeferredSystemActionKind::EnterBootloader;
            } else if (active_.message == kRestartNodeMessageId &&
                       (result.state == runtime::SensorRuntimeState::Idle ||
                        result.state == runtime::SensorRuntimeState::Maintenance ||
                        result.state == runtime::SensorRuntimeState::Faulted)) {
                action = runtime::DeferredSystemActionKind::RestartNode;
            }
        }
        cache_result(last_request_, result, action);
        increment(completed_operations_);
        return true;
    }

    LifecycleHandlerResult handle(
        uint8_t message, const uint8_t* request_payload,
        size_t request_payload_size, uint8_t* response_payload,
        size_t response_capacity) override {
        (void)poll_result();
        if (faulted_) {
            return error(ResponseStatus::InternalError,
                         response_payload, response_capacity);
        }

        RequestKey request{};
        runtime::SensorRuntimeCommand sensor_command{};
        if (message == kPrepareFirmwareUpdateMessageId ||
            message == kConfirmTrialBootMessageId ||
            message == kRestartNodeMessageId) {
            uint32_t operation_id = 0;
            LifecycleCodecStatus decoded = LifecycleCodecStatus::UnknownMessage;
            if (message == kPrepareFirmwareUpdateMessageId) {
                decoded = decode_prepare_firmware_update_request(
                    request_payload, request_payload_size, operation_id);
            } else if (message == kConfirmTrialBootMessageId) {
                decoded = decode_confirm_trial_boot_request(
                    request_payload, request_payload_size, operation_id);
            } else {
                decoded = decode_restart_node_request(
                    request_payload, request_payload_size, operation_id);
            }
            if (decoded != LifecycleCodecStatus::Ok) {
                return error(ResponseStatus::MalformedRequest,
                             response_payload, response_capacity);
            }
            request = {operation_id, message,
                       runtime::ObservabilityProfile::Production, true};
        } else {
            const LifecycleCodecStatus decoded = decode_lifecycle_request(
                message, request_payload, request_payload_size, sensor_command);
            if (decoded == LifecycleCodecStatus::UnknownMessage) {
                return error(ResponseStatus::UnsupportedMessage,
                             response_payload, response_capacity);
            }
            if (decoded != LifecycleCodecStatus::Ok) {
                return error(ResponseStatus::MalformedRequest,
                             response_payload, response_capacity);
            }
            request = {sensor_command.operation_id, message,
                       sensor_command.requested_profile, false};
        }

        CacheEntry* cached = find_cached(request.operation_id);
        if (cached != nullptr) {
            if (!same_request(cached->request, request)) {
                increment(conflict_responses_);
                return error(ResponseStatus::Conflict,
                             response_payload, response_capacity);
            }
            return encode_cached(*cached, response_payload, response_capacity);
        }

        if (has_active_) {
            if (!same_request(active_, request)) {
                if (active_.operation_id == request.operation_id) {
                    increment(conflict_responses_);
                    return error(ResponseStatus::Conflict,
                                 response_payload, response_capacity);
                }
                increment(busy_responses_);
                return error(ResponseStatus::Busy,
                             response_payload, response_capacity);
            }
            return encode(in_progress(request), response_payload,
                          response_capacity);
        }

        if (has_last_result_ &&
            last_request_.operation_id == request.operation_id &&
            !same_request(last_request_, request)) {
            increment(conflict_responses_);
            return error(ResponseStatus::Conflict,
                         response_payload, response_capacity);
        }

        const bool accepted = request.system_operation
            ? system_mailbox_.try_send_command({
                  request.operation_id,
                  request.message == kPrepareFirmwareUpdateMessageId
                      ? runtime::SystemOperation::PrepareFirmwareUpdate
                      : request.message == kConfirmTrialBootMessageId
                          ? runtime::SystemOperation::ConfirmTrialBoot
                          : runtime::SystemOperation::RestartNode})
            : sensor_mailbox_.try_send_command(sensor_command);
        if (!accepted) {
            increment(busy_responses_);
            return error(ResponseStatus::Busy,
                         response_payload, response_capacity);
        }
        active_ = request;
        has_active_ = true;
        increment(accepted_operations_);
        return encode(in_progress(request), response_payload, response_capacity);
    }

    bool settle_transmission(uint64_t token, bool transmitted) override {
        CacheEntry* cached = find_token(token);
        if (cached == nullptr) return false;
        if (!transmitted) return true;
        const auto status = system_action_.request(cached->post_transmit_action);
        if (status != runtime::DeferredSystemActionRequestStatus::Accepted &&
            status != runtime::DeferredSystemActionRequestStatus::AlreadyRequested) {
            return false;
        }
        cached->post_transmit_pending = false;
        if (cached->post_transmit_action ==
            runtime::DeferredSystemActionKind::EnterBootloader) {
            increment(update_actions_armed_);
        } else {
            increment(restart_actions_armed_);
        }
        return true;
    }

    LifecycleServiceState state() const {
        return {
            has_active_, faulted_, has_active_ ? active_.operation_id : 0,
            cached_count_, accepted_operations_, completed_operations_,
            in_progress_results_, busy_responses_, conflict_responses_,
            unexpected_results_, update_actions_armed_, restart_actions_armed_,
        };
    }

private:
    struct RequestKey {
        uint32_t operation_id = 0;
        uint8_t message = 0;
        runtime::ObservabilityProfile profile =
            runtime::ObservabilityProfile::Production;
        bool system_operation = false;
    };

    struct CacheEntry {
        RequestKey request{};
        runtime::SensorRuntimeResult result{};
        uint64_t post_transmit_token = 0;
        runtime::DeferredSystemActionKind post_transmit_action =
            runtime::DeferredSystemActionKind::None;
        bool post_transmit_pending = false;
        bool valid = false;
    };

    static void increment(uint32_t& value) {
        if (value != UINT32_MAX) ++value;
    }

    static bool same_request(const RequestKey& left, const RequestKey& right) {
        return left.operation_id == right.operation_id &&
               left.message == right.message &&
               left.system_operation == right.system_operation &&
               (left.message != kSetObservabilityMessageId ||
                left.profile == right.profile);
    }

    runtime::SensorRuntimeResult in_progress(const RequestKey& request) const {
        if (has_last_result_ && same_request(last_request_, request)) {
            runtime::SensorRuntimeResult result = last_result_;
            result.code = runtime::SensorRuntimeResultCode::InProgress;
            return result;
        }
        return {
            request.operation_id,
            runtime::SensorRuntimeResultCode::InProgress,
            has_last_result_ ? last_result_.state
                             : runtime::SensorRuntimeState::Idle,
            has_last_result_ ? last_result_.observability
                             : runtime::ObservabilityProfile::Production,
            has_last_result_ ? last_result_.sensor_status : SensorStatus::Ok,
        };
    }

    static LifecycleHandlerResult error(
        ResponseStatus status, uint8_t* output, size_t capacity) {
        if (output == nullptr || capacity == 0) return {};
        output[0] = static_cast<uint8_t>(status);
        return {1, false, 0};
    }

    static LifecycleHandlerResult encode(
        const runtime::SensorRuntimeResult& result,
        uint8_t* output, size_t capacity) {
        if (encode_lifecycle_response(result, output, capacity) !=
            LifecycleCodecStatus::Ok) {
            return error(ResponseStatus::InternalError, output, capacity);
        }
        return {kLifecycleResponseBytes, false, 0};
    }

    static LifecycleHandlerResult encode_cached(
        const CacheEntry& cached, uint8_t* output, size_t capacity) {
        LifecycleHandlerResult encoded = encode(cached.result, output, capacity);
        if (encoded.payload_size == kLifecycleResponseBytes &&
            cached.post_transmit_pending) {
            encoded.post_transmit_required = true;
            encoded.post_transmit_token = cached.post_transmit_token;
        }
        return encoded;
    }

    CacheEntry* find_cached(uint32_t operation_id) {
        for (size_t index = 0; index < ResultCacheCapacity; ++index) {
            if (cache_[index].valid &&
                cache_[index].request.operation_id == operation_id) {
                return &cache_[index];
            }
        }
        return nullptr;
    }

    CacheEntry* find_token(uint64_t token) {
        for (size_t index = 0; index < ResultCacheCapacity; ++index) {
            if (cache_[index].valid && cache_[index].post_transmit_pending &&
                cache_[index].post_transmit_token == token) {
                return &cache_[index];
            }
        }
        return nullptr;
    }

    void cache_result(const RequestKey& request,
                      const runtime::SensorRuntimeResult& result,
                      runtime::DeferredSystemActionKind action) {
        uint64_t token = 0;
        if (action != runtime::DeferredSystemActionKind::None) {
            ++next_post_transmit_generation_;
            if (next_post_transmit_generation_ == 0) {
                ++next_post_transmit_generation_;
            }
            token = (static_cast<uint64_t>(next_post_transmit_generation_) << 32u) |
                    request.operation_id;
        }
        cache_[cache_next_] = {
            request,
            result,
            token,
            action,
            action != runtime::DeferredSystemActionKind::None,
            true};
        cache_next_ = (cache_next_ + 1u) % ResultCacheCapacity;
        if (cached_count_ < ResultCacheCapacity) ++cached_count_;
    }

    SensorMailbox& sensor_mailbox_;
    SystemMailbox& system_mailbox_;
    runtime::DeferredSystemAction& system_action_;
    CacheEntry cache_[ResultCacheCapacity]{};
    RequestKey active_{};
    RequestKey last_request_{};
    runtime::SensorRuntimeResult last_result_{};
    size_t cache_next_ = 0;
    size_t cached_count_ = 0;
    uint32_t next_post_transmit_generation_ = 0;
    uint32_t accepted_operations_ = 0;
    uint32_t completed_operations_ = 0;
    uint32_t in_progress_results_ = 0;
    uint32_t busy_responses_ = 0;
    uint32_t conflict_responses_ = 0;
    uint32_t unexpected_results_ = 0;
    uint32_t update_actions_armed_ = 0;
    uint32_t restart_actions_armed_ = 0;
    bool has_active_ = false;
    bool has_last_result_ = false;
    bool faulted_ = false;
};

}  // namespace ProtocolV3
