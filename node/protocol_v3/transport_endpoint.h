#pragma once

#include <cstddef>
#include <cstdint>

#include "protocol_v3/dispatcher.h"
#include "protocol_v3/framing.h"
#include "protocol_v3/response_outbox.h"

namespace ProtocolV3 {

struct TransportEndpointPolicy {
    size_t max_rx_bytes_per_update = 256;
    uint64_t tx_timeout_us = 100000;
    uint64_t response_turnaround_us = 0;
};

struct TransportEndpointStats {
    uint64_t rx_bytes = 0;
    uint64_t decoded_frames = 0;
    uint64_t framing_errors = 0;
    uint64_t ignored_frames = 0;
    uint64_t response_queue_full = 0;
    uint64_t invalid_responses = 0;
    uint64_t completion_rejections = 0;
    uint64_t tx_started = 0;
    uint64_t tx_completed = 0;
    uint64_t tx_start_failures = 0;
    uint64_t tx_timeouts = 0;
    uint64_t tx_encode_failures = 0;
    uint64_t unexpected_port_busy = 0;
    uint64_t transmission_completion_rejections = 0;
};

// Single-owner protocol v3 endpoint. PortT must provide poll/read_byte,
// start_tx, tx_busy, tx_completed, clear_tx_completed and abort_tx.
template <typename PortT, size_t ResponseQueueCapacity>
class TransportEndpoint {
public:
    TransportEndpoint(
        PortT& port, ApplicationDispatcher& dispatcher,
        TransportEndpointPolicy policy = {})
        : port_(port), dispatcher_(dispatcher), policy_(policy) {}

    TransportEndpoint(const TransportEndpoint&) = delete;
    TransportEndpoint& operator=(const TransportEndpoint&) = delete;

    bool valid_policy() const {
        return policy_.max_rx_bytes_per_update > 0 &&
               policy_.tx_timeout_us > 0;
    }

    void update(uint64_t now_us) {
        if (!valid_policy()) return;
        port_.poll();
        service_active_tx(now_us);
        if (tx_active_) return;

        if (try_start_tx(now_us)) return;
        process_rx(now_us);
        (void)try_start_tx(now_us);
    }

    bool tx_active() const { return tx_active_; }
    size_t queued_responses() const { return outbox_.size(); }
    const TransportEndpointStats& stats() const { return stats_; }

private:
    static void increment(uint64_t& counter) {
        if (counter != UINT64_MAX) ++counter;
    }

    void service_active_tx(uint64_t now_us) {
        if (!tx_active_) return;
        if (!port_.tx_busy() && port_.tx_completed()) {
            port_.clear_tx_completed();
            tx_active_ = false;
            increment(stats_.tx_completed);
            settle_active_transmission(true);
            return;
        }
        if (now_us - tx_started_at_us_ >= policy_.tx_timeout_us) {
            port_.abort_tx();
            tx_active_ = false;
            increment(stats_.tx_timeouts);
            settle_active_transmission(false);
        }
    }

    void process_rx(uint64_t now_us) {
        size_t processed = 0;
        uint8_t byte = 0;
        while (processed < policy_.max_rx_bytes_per_update &&
               port_.read_byte(byte)) {
            ++processed;
            increment(stats_.rx_bytes);
            Frame request{};
            const Status status = decoder_.push(byte, request);
            if (status == Status::NoFrame) continue;
            if (status != Status::Ok) {
                increment(stats_.framing_errors);
                continue;
            }

            increment(stats_.decoded_frames);
            const RequestAcceptanceStatus accepted =
                outbox_.accept_request_at(dispatcher_, request, now_us);
            if (accepted == RequestAcceptanceStatus::Ignored) {
                increment(stats_.ignored_frames);
            } else if (accepted == RequestAcceptanceStatus::QueueFull) {
                increment(stats_.response_queue_full);
            } else if (accepted == RequestAcceptanceStatus::InvalidResponse) {
                increment(stats_.invalid_responses);
            } else if (accepted == RequestAcceptanceStatus::CompletionRejected) {
                increment(stats_.completion_rejections);
            } else {
                tx_not_before_us_ =
                    now_us > UINT64_MAX - policy_.response_turnaround_us
                        ? UINT64_MAX
                        : now_us + policy_.response_turnaround_us;
            }
        }
    }

    bool try_start_tx(uint64_t now_us) {
        if (tx_active_ || outbox_.empty()) return false;
        if (now_us < tx_not_before_us_) return false;
        if (port_.tx_busy()) {
            increment(stats_.unexpected_port_busy);
            return false;
        }

        Frame* response = outbox_.front_mut();
        size_t wire_size = 0;
        if (response == nullptr ||
            !dispatcher_.prepare_transmission(*response, now_us) ||
            encode(*response, tx_wire_, sizeof(tx_wire_), wire_size) != Status::Ok) {
            const TransmissionCompletion* completion =
                outbox_.front_transmission();
            if (completion != nullptr && completion->required &&
                dispatcher_.settle_transmission(*completion, false) !=
                    TransmissionSettlementStatus::Ok) {
                increment(stats_.transmission_completion_rejections);
            }
            outbox_.pop();
            increment(stats_.tx_encode_failures);
            return false;
        }
        if (!port_.start_tx(tx_wire_, wire_size)) {
            increment(stats_.tx_start_failures);
            return false;
        }

        const TransmissionCompletion* completion =
            outbox_.front_transmission();
        active_transmission_ = completion == nullptr
            ? TransmissionCompletion{}
            : *completion;
        outbox_.pop();
        tx_active_ = true;
        tx_started_at_us_ = now_us;
        increment(stats_.tx_started);
        return true;
    }

    void settle_active_transmission(bool transmitted) {
        if (!active_transmission_.required) return;
        if (dispatcher_.settle_transmission(
                active_transmission_, transmitted) !=
            TransmissionSettlementStatus::Ok) {
            increment(stats_.transmission_completion_rejections);
        }
        active_transmission_ = {};
    }

    PortT& port_;
    ApplicationDispatcher& dispatcher_;
    TransportEndpointPolicy policy_{};
    StreamDecoder decoder_{};
    ResponseOutbox<ResponseQueueCapacity> outbox_{};
    uint8_t tx_wire_[kMaxWireFrameBytes]{};
    uint64_t tx_started_at_us_ = 0;
    uint64_t tx_not_before_us_ = 0;
    bool tx_active_ = false;
    TransmissionCompletion active_transmission_{};
    TransportEndpointStats stats_{};
};

}  // namespace ProtocolV3
