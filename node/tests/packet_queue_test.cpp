#include <cassert>
#include <cstddef>
#include <cstdint>

#include "transport/packet_queue.h"

namespace {

using Queue = PacketQueue<3, 8>;
using Packet = Queue::Packet;

Packet make_packet(uint32_t packet_seq,
                   uint64_t first_sample_seq,
                   uint64_t last_sample_seq) {
    Packet packet{};
    packet.packet_seq = packet_seq;
    packet.first_sample_seq = first_sample_seq;
    packet.last_sample_seq = last_sample_seq;
    packet.sample_count =
        static_cast<uint16_t>(last_sample_seq - first_sample_seq + 1);
    return packet;
}

void test_peek_and_find_preserve_fifo_order() {
    Queue queue{};
    queue.push(make_packet(10, 100, 103));
    queue.push(make_packet(11, 104, 107));

    assert(queue.count() == 2);
    assert(queue.capacity() == 3);
    assert(queue.overwrite_count() == 0);

    const Packet* head = nullptr;
    assert(queue.peek_head(head));
    assert(head != nullptr);
    assert(head->packet_seq == 10);
    assert(head->first_sample_seq == 100);

    const Packet* tail = nullptr;
    assert(queue.peek_tail(tail));
    assert(tail != nullptr);
    assert(tail->packet_seq == 11);
    assert(tail->last_sample_seq == 107);

    size_t relative_index = 99;
    assert(queue.find_packet_index_by_seq(105, relative_index));
    assert(relative_index == 1);

    assert(queue.find_packet_index_by_packet_seq(10, relative_index));
    assert(relative_index == 0);
}

void test_overwrite_discards_oldest_packet() {
    Queue queue{};
    queue.push(make_packet(1, 0, 1));
    queue.push(make_packet(2, 2, 3));
    queue.push(make_packet(3, 4, 5));
    queue.push(make_packet(4, 6, 7));

    assert(queue.count() == 3);
    assert(queue.overwrite_count() == 1);

    const Packet* head = nullptr;
    assert(queue.peek_head(head));
    assert(head != nullptr);
    assert(head->packet_seq == 2);

    const Packet* tail = nullptr;
    assert(queue.peek_tail(tail));
    assert(tail != nullptr);
    assert(tail->packet_seq == 4);
}

void test_trim_removes_only_fully_committed_packets() {
    Queue queue{};
    queue.push(make_packet(1, 0, 2));
    queue.push(make_packet(2, 3, 5));
    queue.push(make_packet(3, 6, 8));

    assert(queue.trim_up_to_sample_seq(4) == 1);
    assert(queue.count() == 2);

    const Packet* head = nullptr;
    assert(queue.peek_head(head));
    assert(head != nullptr);
    assert(head->packet_seq == 2);

    assert(queue.trim_up_to_sample_seq(5) == 1);
    assert(queue.count() == 1);

    assert(queue.trim_up_to_sample_seq(99) == 1);
    assert(queue.count() == 0);
    assert(!queue.peek_head(head));
    assert(head == nullptr);

    queue.push(make_packet(4, 9, 11));
    assert(queue.count() == 1);
    assert(queue.peek_head(head));
    assert(head != nullptr);
    assert(head->packet_seq == 4);
}

}  // namespace

int main() {
    test_peek_and_find_preserve_fifo_order();
    test_overwrite_discards_oldest_packet();
    test_trim_removes_only_fully_committed_packets();
    return 0;
}
