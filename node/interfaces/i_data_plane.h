#pragma once

#include <cstddef>
#include <cstdint>

#include "transport/status_codes.h"

enum class BurstEndReason : uint8_t {
    Completed = 0,
    NoData = 1,
    Busy = 2,
    Aborted = 3
};

struct DataPlaneState {
    uint64_t oldest_packet_first_seq = 0;
    uint64_t newest_packet_last_seq = 0;
    uint64_t committed_sample_seq = 0;
    uint32_t queued_packets = 0;
    uint32_t packet_capacity = 0;
    uint32_t packet_overwrite_count = 0;
};

class IDataPlane {
public:
    virtual ~IDataPlane() = default;

    virtual bool burst_active() const = 0;
    virtual uint8_t burst_destination() const = 0;

    virtual StatusCode start_burst(uint64_t start_seq,
                                   uint16_t max_packets,
                                   uint8_t destination) = 0;
    virtual StatusCode commit_read_up_to(uint64_t last_sample_seq) = 0;
    virtual void finish_burst(BurstEndReason reason) = 0;
    virtual DataPlaneState state() const = 0;

    virtual bool try_build_current_packet_payload(uint8_t* out_payload,
                                                  size_t out_capacity,
                                                  size_t& out_size) = 0;

    virtual void on_packet_transmitted() = 0;
};
