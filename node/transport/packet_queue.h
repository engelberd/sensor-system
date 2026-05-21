#pragma once

#include <array>
#include <cstddef>
#include <cstdint>

template <size_t MaxPayloadSize>
struct QueuedPacket {
    uint32_t packet_seq = 0;
    uint64_t first_sample_seq = 0;
    uint64_t last_sample_seq = 0;
    uint16_t sample_count = 0;
    uint16_t payload_size = 0;
    uint8_t payload[MaxPayloadSize]{};
};

template <size_t Capacity, size_t MaxPayloadSize>
class PacketQueue {
public:
    using Packet = QueuedPacket<MaxPayloadSize>;

    bool push(const Packet& packet) {
        buffer_[write_index_] = packet;
        write_index_ = (write_index_ + 1) % Capacity;

        if (count_ < Capacity) {
            ++count_;
        } else {
            head_index_ = (head_index_ + 1) % Capacity;
            ++overwrite_count_;
        }

        return true;
    }

    bool find_packet_index_by_seq(uint64_t start_seq, size_t& relative_index) const {
        if (count_ == 0) {
            return false;
        }

        for (size_t i = 0; i < count_; ++i) {
            const Packet& p = at_relative(i);
            if (p.last_sample_seq >= start_seq) {
                relative_index = i;
                return true;
            }
        }

        return false;
    }

    bool find_packet_index_by_packet_seq(uint32_t packet_seq,
                                         size_t& relative_index) const {
        if (count_ == 0) {
            return false;
        }

        for (size_t i = 0; i < count_; ++i) {
            const Packet& p = at_relative(i);
            if (p.packet_seq == packet_seq) {
                relative_index = i;
                return true;
            }
        }

        return false;
    }

    bool peek_relative(size_t relative_index, const Packet*& out_packet) const {
        if (relative_index >= count_) {
            out_packet = nullptr;
            return false;
        }

        out_packet = &at_relative(relative_index);
        return true;
    }

    size_t count() const {
        return count_;
    }

    size_t capacity() const {
        return Capacity;
    }

    uint32_t overwrite_count() const {
        return overwrite_count_;
    }

    bool peek_head(const Packet*& out_packet) const {
        return peek_relative(0, out_packet);
    }

    bool peek_tail(const Packet*& out_packet) const {
        if (count_ == 0) {
            out_packet = nullptr;
            return false;
        }

        out_packet = &at_relative(count_ - 1);
        return true;
    }

    size_t trim_up_to_sample_seq(uint64_t committed_sample_seq) {
        size_t removed = 0;

        while (count_ > 0) {
            const Packet& head = buffer_[head_index_];
            if (head.last_sample_seq > committed_sample_seq) {
                break;
            }

            head_index_ = (head_index_ + 1) % Capacity;
            --count_;
            ++removed;
        }

        if (count_ == 0) {
            head_index_ = 0;
            write_index_ = 0;
        }

        return removed;
    }

    void clear() {
        head_index_ = 0;
        write_index_ = 0;
        count_ = 0;
        overwrite_count_ = 0;
    }

private:
    const Packet& at_relative(size_t relative_index) const {
        const size_t idx = (head_index_ + relative_index) % Capacity;
        return buffer_[idx];
    }

private:
    std::array<Packet, Capacity> buffer_{};
    size_t head_index_ = 0;
    size_t write_index_ = 0;
    size_t count_ = 0;
    uint32_t overwrite_count_ = 0;
};
