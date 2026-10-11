#pragma once

#include <cstddef>
#include <cstdint>

#include "protocol_v3/dispatcher.h"
#include "protocol_v3/framing.h"

namespace ProtocolV3 {

enum class RequestAcceptanceStatus : uint8_t {
    Queued = 0,
    Ignored,
    QueueFull,
    InvalidResponse,
    CompletionRejected,
};

// Bounded owner of decoded response frames. It keeps dispatch and completion
// settlement in one operation, so a prepared delivery token cannot be lost
// between building a response and attempting to queue it.
template <size_t Capacity>
class ResponseOutbox {
    static_assert(Capacity > 0, "response outbox capacity must be non-zero");

public:
    struct QueuedResponse {
        Frame frame{};
        TransmissionCompletion transmission{};
    };

    ResponseOutbox() = default;
    ResponseOutbox(const ResponseOutbox&) = delete;
    ResponseOutbox& operator=(const ResponseOutbox&) = delete;

    RequestAcceptanceStatus accept_request(
        ApplicationDispatcher& dispatcher, const Frame& request) {
        return accept_request_at(dispatcher, request, 0);
    }

    RequestAcceptanceStatus accept_request_at(
        ApplicationDispatcher& dispatcher,
        const Frame& request,
        uint64_t node_receive_us) {
        if (size_ == Capacity) return RequestAcceptanceStatus::QueueFull;

        Frame response{};
        const DispatchResult dispatched =
            dispatcher.dispatch_at(request, response, node_receive_us);
        if (!dispatched.response_produced) return RequestAcceptanceStatus::Ignored;

        if (validate(response) != Status::Ok) {
            if (dispatched.completion.required) {
                dispatcher.settle_response(dispatched.completion, false);
            }
            if (dispatched.transmission.required) {
                dispatcher.settle_transmission(dispatched.transmission, false);
            }
            return RequestAcceptanceStatus::InvalidResponse;
        }

        if (dispatched.completion.required &&
            dispatcher.settle_response(dispatched.completion, true) !=
                CompletionSettlementStatus::Ok) {
            return RequestAcceptanceStatus::CompletionRejected;
        }

        slots_[tail_] = {response, dispatched.transmission};
        tail_ = increment(tail_);
        ++size_;
        return RequestAcceptanceStatus::Queued;
    }

    const Frame* front() const {
        return size_ == 0 ? nullptr : &slots_[head_].frame;
    }

    Frame* front_mut() {
        return size_ == 0 ? nullptr : &slots_[head_].frame;
    }

    const TransmissionCompletion* front_transmission() const {
        return size_ == 0 ? nullptr : &slots_[head_].transmission;
    }

    bool pop() {
        if (size_ == 0) return false;
        slots_[head_] = {};
        head_ = increment(head_);
        --size_;
        return true;
    }

    void clear() {
        while (pop()) {}
    }

    bool empty() const { return size_ == 0; }
    bool full() const { return size_ == Capacity; }
    size_t size() const { return size_; }
    static constexpr size_t capacity() { return Capacity; }

private:
    static constexpr size_t increment(size_t index) {
        return index + 1 == Capacity ? 0 : index + 1;
    }

    QueuedResponse slots_[Capacity]{};
    size_t head_ = 0;
    size_t tail_ = 0;
    size_t size_ = 0;
};

}  // namespace ProtocolV3
