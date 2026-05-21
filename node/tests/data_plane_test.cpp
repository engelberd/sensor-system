#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstring>

#include "transport/command_payloads.h"
#include "transport/data_plane.h"

namespace {

using TestDataPlane = DataPlane<4, 2>;

StoredSample make_sample(uint64_t sample_seq, int32_t value) {
    StoredSample sample{};
    sample.sample_seq = sample_seq;
    sample.x = value;
    sample.y = -value;
    sample.z = value + 100;
    return sample;
}

void feed_samples(TestDataPlane& data_plane,
                  uint64_t first_seq,
                  size_t count) {
    StoredSample samples[8]{};
    assert(count <= 8);

    for (size_t i = 0; i < count; ++i) {
        samples[i] = make_sample(first_seq + i, static_cast<int32_t>(i + 1));
    }

    data_plane.on_samples(samples, count);
}

BurstDataPayloadHeader decode_header(const uint8_t* payload) {
    BurstDataPayloadHeader header{};
    std::memcpy(&header, payload, sizeof(header));
    return header;
}

void assert_i24_be(const uint8_t* bytes, int32_t expected) {
    const uint32_t encoded =
        (static_cast<uint32_t>(bytes[0]) << 16) |
        (static_cast<uint32_t>(bytes[1]) << 8) |
        static_cast<uint32_t>(bytes[2]);
    assert(encoded == (static_cast<uint32_t>(expected) & 0x00FFFFFFu));
}

void test_packets_are_created_after_staging_is_full() {
    TestDataPlane data_plane{};

    feed_samples(data_plane, 0, 1);
    DataPlaneState state = data_plane.state();
    assert(state.queued_packets == 0);

    feed_samples(data_plane, 1, 1);
    state = data_plane.state();
    assert(state.queued_packets == 1);
    assert(state.packet_capacity == 4);
    assert(state.oldest_packet_first_seq == 0);
    assert(state.newest_packet_last_seq == 1);
}

void test_burst_payload_contains_header_and_raw_xyz24_samples() {
    TestDataPlane data_plane{};
    StoredSample samples[2]{};
    samples[0] = make_sample(10, 0x010203);
    samples[1] = make_sample(11, -2);
    data_plane.on_samples(samples, 2);

    assert(data_plane.start_burst(10, 1, 7) == StatusCode::Ok);
    assert(data_plane.burst_active());
    assert(data_plane.burst_destination() == 7);

    uint8_t payload[TestDataPlane::MAX_PACKET_PAYLOAD]{};
    size_t payload_size = 0;
    assert(data_plane.try_build_current_packet_payload(
        payload,
        sizeof(payload),
        payload_size
    ));
    assert(payload_size == sizeof(BurstDataPayloadHeader) + 2 * 9);

    const BurstDataPayloadHeader header = decode_header(payload);
    assert(header.command == static_cast<uint8_t>(CommandType::GrantBurstRead));
    assert(header.status == static_cast<uint8_t>(StatusCode::Ok));
    assert(header.packet_seq == 0);
    assert(header.first_sample_seq == 10);
    assert(header.sample_count == 2);
    assert(header.sample_encoding == static_cast<uint8_t>(SampleEncoding::RawXYZ24));

    const uint8_t* sample_bytes = payload + sizeof(BurstDataPayloadHeader);
    assert_i24_be(sample_bytes + 0, 0x010203);
    assert_i24_be(sample_bytes + 3, -0x010203);
    assert_i24_be(sample_bytes + 6, 0x010267);
    assert_i24_be(sample_bytes + 9, -2);
    assert_i24_be(sample_bytes + 12, 2);
    assert_i24_be(sample_bytes + 15, 98);

    assert(!data_plane.try_build_current_packet_payload(
        payload,
        sizeof(payload),
        payload_size
    ));

    data_plane.on_packet_transmitted();
    assert(!data_plane.burst_active());
}

void test_commit_trims_packets_and_restarts_reads_after_commit() {
    TestDataPlane data_plane{};
    feed_samples(data_plane, 0, 4);

    assert(data_plane.commit_read_up_to(1) == StatusCode::Ok);

    DataPlaneState state = data_plane.state();
    assert(state.committed_sample_seq == 1);
    assert(state.queued_packets == 1);
    assert(state.oldest_packet_first_seq == 2);
    assert(state.newest_packet_last_seq == 3);

    assert(data_plane.start_burst(0, 1, 3) == StatusCode::Ok);

    uint8_t payload[TestDataPlane::MAX_PACKET_PAYLOAD]{};
    size_t payload_size = 0;
    assert(data_plane.try_build_current_packet_payload(
        payload,
        sizeof(payload),
        payload_size
    ));

    const BurstDataPayloadHeader header = decode_header(payload);
    assert(header.first_sample_seq == 2);
    assert(header.sample_count == 2);
}

void test_commit_finishes_active_burst_when_next_packet_is_trimmed() {
    TestDataPlane data_plane{};
    feed_samples(data_plane, 0, 4);

    assert(data_plane.start_burst(0, 2, 5) == StatusCode::Ok);
    assert(data_plane.burst_active());

    assert(data_plane.commit_read_up_to(3) == StatusCode::Ok);
    assert(!data_plane.burst_active());

    DataPlaneState state = data_plane.state();
    assert(state.committed_sample_seq == 3);
    assert(state.queued_packets == 0);
}

}  // namespace

int main() {
    test_packets_are_created_after_staging_is_full();
    test_burst_payload_contains_header_and_raw_xyz24_samples();
    test_commit_trims_packets_and_restarts_reads_after_commit();
    test_commit_finishes_active_burst_when_next_packet_is_trimmed();
    return 0;
}
