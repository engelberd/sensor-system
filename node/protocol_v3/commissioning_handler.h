#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>

#include "protocol_v3/commissioning.h"
#include "runtime/commissioning_runtime.h"
#include "runtime/deferred_system_action.h"

namespace ProtocolV3 {

struct CommissioningHandlerResult {
    size_t payload_size = 0;
    bool response_produced = false;
    bool post_transmit_required = false;
    uint64_t post_transmit_token = 0;
};

class ICommissioningServiceHandler {
public:
    virtual ~ICommissioningServiceHandler() = default;
    virtual CommissioningHandlerResult handle(
        uint8_t message,
        const uint8_t* request_payload,
        size_t request_payload_size,
        uint8_t* response_payload,
        size_t response_capacity) = 0;
    virtual bool settle_transmission(uint64_t token, bool transmitted) = 0;
};

template <typename Mailbox, size_t ResultCacheCapacity>
class CommissioningServiceHandler final : public ICommissioningServiceHandler {
    static_assert(ResultCacheCapacity > 0,
                  "commissioning result cache must be non-zero");

public:
    CommissioningServiceHandler(
        Mailbox& mailbox,
        runtime::DeferredSystemAction& system_action,
        const CommissioningIdentity& identity)
        : mailbox_(mailbox), system_action_(system_action), identity_(identity) {}

    CommissioningHandlerResult handle(
        uint8_t message,
        const uint8_t* request_payload,
        size_t request_payload_size,
        uint8_t* response_payload,
        size_t response_capacity) override {
        poll_result();
        if (faulted_) {
            return error(ResponseStatus::InternalError,
                         response_payload, response_capacity);
        }
        if (message == kProbeSlotMessageId) {
            ProbeSlotRequest request{};
            if (decode_probe_slot_request(
                    request_payload, request_payload_size, request) !=
                CommissioningCodecStatus::Ok) {
                return error(ResponseStatus::MalformedRequest,
                             response_payload, response_capacity);
            }
            if (!matches_commissioning_slot(identity_.hardware_id, request)) {
                return {};
            }
            CommissioningIdentity response = identity_;
            response.round_nonce = request.round_nonce;
            if (encode_probe_slot_success(
                    response, response_payload, response_capacity) !=
                CommissioningCodecStatus::Ok) {
                return error(ResponseStatus::InternalError,
                             response_payload, response_capacity);
            }
            return {kProbeSlotResponseBytes, true, false, 0};
        }
        if (message != kAssignAddressMessageId) {
            return error(ResponseStatus::UnsupportedMessage,
                         response_payload, response_capacity);
        }

        AssignAddressRequest request{};
        if (decode_assign_address_request(
                request_payload, request_payload_size, request) !=
            CommissioningCodecStatus::Ok) {
            return error(ResponseStatus::MalformedRequest,
                         response_payload, response_capacity);
        }
        if (std::memcmp(request.hardware_id, identity_.hardware_id,
                        kHardwareIdBytes) != 0) {
            return {};
        }

        CacheEntry* cached = find_cached(request.operation_id);
        if (cached != nullptr) {
            if (!same_request(cached->request, request)) {
                return error(ResponseStatus::Conflict,
                             response_payload, response_capacity);
            }
            if (cached->result.code !=
                runtime::CommissioningResultCode::Assigned) {
                return error(ResponseStatus::InternalError,
                             response_payload, response_capacity);
            }
            const AssignAddressResponse response{
                cached->result.operation_id, {}, cached->result.node_address};
            AssignAddressResponse mutable_response = response;
            std::memcpy(mutable_response.hardware_id, identity_.hardware_id,
                        kHardwareIdBytes);
            if (encode_assign_address_success(
                    mutable_response, response_payload, response_capacity) !=
                CommissioningCodecStatus::Ok) {
                return error(ResponseStatus::InternalError,
                             response_payload, response_capacity);
            }
            return {kAssignAddressResponseBytes, true, true, cached->token};
        }
        if (active_) {
            if (!same_request(active_request_, request)) {
                return error(
                    active_request_.operation_id == request.operation_id
                        ? ResponseStatus::Conflict
                        : ResponseStatus::Busy,
                    response_payload, response_capacity);
            }
            return error(ResponseStatus::Busy,
                         response_payload, response_capacity);
        }
        if (!mailbox_.try_send_command({
                request.operation_id, request.round_nonce,
                request.node_address})) {
            return error(ResponseStatus::Busy,
                         response_payload, response_capacity);
        }
        active_request_ = request;
        active_ = true;
        return error(ResponseStatus::Busy,
                     response_payload, response_capacity);
    }

    bool poll_result() {
        runtime::CommissioningResult result{};
        if (!mailbox_.try_receive_result(result)) return false;
        if (!active_ || result.operation_id != active_request_.operation_id) {
            faulted_ = true;
            return true;
        }
        CacheEntry& entry = cache_[next_cache_];
        entry = {active_request_, result, next_token_++, true, false};
        if (next_token_ == 0) next_token_ = 1;
        next_cache_ = (next_cache_ + 1u) % ResultCacheCapacity;
        active_ = false;
        return true;
    }

    bool settle_transmission(uint64_t token, bool transmitted) override {
        CacheEntry* entry = find_token(token);
        if (entry == nullptr) return false;
        if (!transmitted || entry->restart_armed) return true;
        const auto status = system_action_.request(
            runtime::DeferredSystemActionKind::RestartNode);
        if (status != runtime::DeferredSystemActionRequestStatus::Accepted &&
            status != runtime::DeferredSystemActionRequestStatus::AlreadyRequested) {
            return false;
        }
        entry->restart_armed = true;
        return true;
    }

private:
    struct CacheEntry {
        AssignAddressRequest request{};
        runtime::CommissioningResult result{};
        uint64_t token = 0;
        bool valid = false;
        bool restart_armed = false;
    };

    static bool same_request(
        const AssignAddressRequest& left,
        const AssignAddressRequest& right) {
        return left.operation_id == right.operation_id &&
               left.round_nonce == right.round_nonce &&
               left.node_address == right.node_address &&
               std::memcmp(left.hardware_id, right.hardware_id,
                           kHardwareIdBytes) == 0;
    }

    static CommissioningHandlerResult error(
        ResponseStatus status, uint8_t* output, size_t capacity) {
        if (output == nullptr || capacity == 0) return {};
        output[0] = static_cast<uint8_t>(status);
        return {1, true, false, 0};
    }

    CacheEntry* find_cached(uint32_t operation_id) {
        for (auto& entry : cache_) {
            if (entry.valid && entry.request.operation_id == operation_id) {
                return &entry;
            }
        }
        return nullptr;
    }

    CacheEntry* find_token(uint64_t token) {
        for (auto& entry : cache_) {
            if (entry.valid && entry.token == token) return &entry;
        }
        return nullptr;
    }

    Mailbox& mailbox_;
    runtime::DeferredSystemAction& system_action_;
    CommissioningIdentity identity_{};
    AssignAddressRequest active_request_{};
    CacheEntry cache_[ResultCacheCapacity]{};
    size_t next_cache_ = 0;
    uint64_t next_token_ = 1;
    bool active_ = false;
    bool faulted_ = false;
};

}  // namespace ProtocolV3
