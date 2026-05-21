#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>

#include "pico/critical_section.h"
#include "interfaces/i_data_plane.h"
#include "interfaces/i_sample_sink.h"
#include "storage/stored_sample.h"
#include "transport/command_payloads.h"
#include "transport/command_types.h"
#include "transport/packet_queue.h"

template <size_t PacketQueueCapacity, size_t SamplesPerPacket = 32>
class DataPlane : public ISampleSink, public IDataPlane {
public:
    static constexpr size_t RAW_SAMPLE_SIZE = 9;
    static constexpr size_t MAX_PACKET_PAYLOAD =
        sizeof(BurstDataPayloadHeader) + SamplesPerPacket * RAW_SAMPLE_SIZE;

    using QueueT = PacketQueue<PacketQueueCapacity, MAX_PACKET_PAYLOAD>;
    using PacketT = typename QueueT::Packet;

    DataPlane() {
        critical_section_init(&cs_);
    }

    void on_samples(const StoredSample* samples, size_t count) override {
        if (samples == nullptr || count == 0) {
            return;
        }

        critical_section_enter_blocking(&cs_);

        for (size_t i = 0; i < count; ++i) {
            staging_[staging_count_++] = samples[i];

            if (staging_count_ == SamplesPerPacket) {
                flush_staging_packet_locked();
            }
        }

        critical_section_exit(&cs_);
    }

    bool burst_active() const override {
        critical_section_enter_blocking(const_cast<critical_section_t*>(&cs_));
        const bool active = burst_active_;
        critical_section_exit(&cs_);
        return active;
    }

    uint8_t burst_destination() const override {
        critical_section_enter_blocking(const_cast<critical_section_t*>(&cs_));
        const uint8_t destination = burst_destination_;
        critical_section_exit(&cs_);
        return destination;
    }

    DataPlaneState state() const override {
        critical_section_enter_blocking(const_cast<critical_section_t*>(&cs_));

        DataPlaneState result{};
        result.committed_sample_seq = committed_sample_seq_;
        result.queued_packets = static_cast<uint32_t>(queue_.count());
        result.packet_capacity = static_cast<uint32_t>(queue_.capacity());
        result.packet_overwrite_count = queue_.overwrite_count();

        const PacketT* head = nullptr;
        if (queue_.peek_head(head) && head != nullptr) {
            result.oldest_packet_first_seq = head->first_sample_seq;
        }

        const PacketT* tail = nullptr;
        if (queue_.peek_tail(tail) && tail != nullptr) {
            result.newest_packet_last_seq = tail->last_sample_seq;
        }

        critical_section_exit(&cs_);
        return result;
    }

    StatusCode start_burst(uint64_t start_seq,
                           uint16_t max_packets,
                           uint8_t destination) override {
        critical_section_enter_blocking(&cs_);

        StatusCode status = StatusCode::Ok;

        if (max_packets == 0) {
            status = StatusCode::InvalidParam;
        } else if (burst_active_) {
            status = StatusCode::Busy;
        } else {
            const uint64_t effective_start_seq =
                (start_seq <= committed_sample_seq_)
                    ? (committed_sample_seq_ + 1)
                    : start_seq;
            size_t start_index = 0;
            if (!queue_.find_packet_index_by_seq(effective_start_seq, start_index)) {
                status = StatusCode::NoData;
            } else {
                const PacketT* pkt = nullptr;
                if (!queue_.peek_relative(start_index, pkt) || pkt == nullptr) {
                    status = StatusCode::NoData;
                } else {
                    burst_active_ = true;
                    burst_next_packet_seq_ = pkt->packet_seq;
                    burst_packets_remaining_ = max_packets;
                    burst_packet_inflight_ = false;
                    burst_destination_ = destination;
                }
            }
        }
        // printf("[DP] start_burst start_seq=%llu max_packets=%u\n",
        // (unsigned long long)start_seq,
        // (unsigned)max_packets);

        critical_section_exit(&cs_);
        return status;
    }

    StatusCode commit_read_up_to(uint64_t last_sample_seq) override {
        critical_section_enter_blocking(&cs_);

        if (last_sample_seq <= committed_sample_seq_) {
            critical_section_exit(&cs_);
            return StatusCode::Ok;
        }

        committed_sample_seq_ = last_sample_seq;
        queue_.trim_up_to_sample_seq(committed_sample_seq_);

        if (burst_active_) {
            size_t packet_index = 0;
            if (!queue_.find_packet_index_by_packet_seq(burst_next_packet_seq_, packet_index)) {
                finish_burst_locked();
            }
        }

        critical_section_exit(&cs_);
        return StatusCode::Ok;
    }

    void finish_burst(BurstEndReason reason) override {
        (void)reason;
        critical_section_enter_blocking(&cs_);
        finish_burst_locked();
        critical_section_exit(&cs_);
    }

    bool try_build_current_packet_payload(uint8_t* out_payload,
                                          size_t out_capacity,
                                          size_t& out_size) override {
        out_size = 0;

        critical_section_enter_blocking(&cs_);

        if (!burst_active_) {
            critical_section_exit(&cs_);
            return false;
        }

        if (burst_packets_remaining_ == 0) {
            finish_burst_locked();
            critical_section_exit(&cs_);
            return false;
        }

        if (burst_packet_inflight_) {
            critical_section_exit(&cs_);
            return false;
        }

        size_t packet_index = 0;
        if (!queue_.find_packet_index_by_packet_seq(burst_next_packet_seq_, packet_index)) {
            finish_burst_locked();
            critical_section_exit(&cs_);
            return false;
        }

        const PacketT* pkt = nullptr;
        if (!queue_.peek_relative(packet_index, pkt) || pkt == nullptr) {
            finish_burst_locked();
            critical_section_exit(&cs_);
            return false;
        }

        if (out_capacity < pkt->payload_size) {
            finish_burst_locked();
            critical_section_exit(&cs_);
            return false;
        }

        std::memcpy(out_payload, pkt->payload, pkt->payload_size);
        out_size = pkt->payload_size;
        burst_packet_inflight_ = true;

        critical_section_exit(&cs_);
        return true;
    }

    void on_packet_transmitted() override {
        critical_section_enter_blocking(&cs_);

        if (!burst_active_ || !burst_packet_inflight_) {
            critical_section_exit(&cs_);
            return;
        }

        burst_packet_inflight_ = false;
        ++burst_next_packet_seq_;

        if (burst_packets_remaining_ > 0) {
            --burst_packets_remaining_;
        }

        if (burst_packets_remaining_ == 0) {
            finish_burst_locked();
        }

        critical_section_exit(&cs_);
    }

private:
    void finish_burst_locked() {
        burst_active_ = false;
        burst_next_packet_seq_ = 0;
        burst_packets_remaining_ = 0;
        burst_packet_inflight_ = false;
        burst_destination_ = 0;
    }

    static void encode_i24_be(int32_t value, uint8_t* out) {
        const uint32_t v = static_cast<uint32_t>(value) & 0x00FFFFFFu;
        out[0] = static_cast<uint8_t>((v >> 16) & 0xFF);
        out[1] = static_cast<uint8_t>((v >> 8) & 0xFF);
        out[2] = static_cast<uint8_t>(v & 0xFF);
    }

    void flush_staging_packet_locked() {
        if (staging_count_ == 0) {
            return;
        }


        PacketT pkt{};
        pkt.packet_seq = next_packet_seq_++;
        pkt.first_sample_seq = staging_[0].sample_seq;
        pkt.last_sample_seq = staging_[staging_count_ - 1].sample_seq;
        pkt.sample_count = static_cast<uint16_t>(staging_count_);

        BurstDataPayloadHeader hdr{};
        hdr.command = static_cast<uint8_t>(CommandType::GrantBurstRead);
        hdr.status = static_cast<uint8_t>(StatusCode::Ok);
        hdr.packet_seq = pkt.packet_seq;
        hdr.first_sample_seq = pkt.first_sample_seq;
        hdr.sample_count = pkt.sample_count;
        hdr.sample_encoding = static_cast<uint8_t>(SampleEncoding::RawXYZ24);

        size_t offset = 0;
        std::memcpy(pkt.payload + offset, &hdr, sizeof(hdr));
        offset += sizeof(hdr);

        for (size_t i = 0; i < staging_count_; ++i) {
            encode_i24_be(staging_[i].x, pkt.payload + offset); offset += 3;
            encode_i24_be(staging_[i].y, pkt.payload + offset); offset += 3;
            encode_i24_be(staging_[i].z, pkt.payload + offset); offset += 3;
        }

        pkt.payload_size = static_cast<uint16_t>(offset);
        queue_.push(pkt);

        staging_count_ = 0;
    }

private:
    mutable critical_section_t cs_{};
    QueueT queue_{};

    StoredSample staging_[SamplesPerPacket]{};
    size_t staging_count_ = 0;
    uint32_t next_packet_seq_ = 0;
    uint64_t committed_sample_seq_ = 0;

    bool burst_active_ = false;
    uint32_t burst_next_packet_seq_ = 0;
    uint16_t burst_packets_remaining_ = 0;
    bool burst_packet_inflight_ = false;
    uint8_t burst_destination_ = 0;
};
