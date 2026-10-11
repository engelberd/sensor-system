#include <cassert>
#include <cstddef>
#include <cstdint>
#include <vector>

#include "measurement/measurement_journal.h"

namespace {
using namespace measurement;

SampleRecord sample(int32_t value, uint32_t config = 1, uint32_t segment = 1) {
    return {static_cast<uint64_t>(value + 100000), value, -value, value + 1,
            config, segment, 1, 0};
}

int32_t value_at(const MeasurementReadView& view, size_t index) {
    return index < view.first.size
        ? view.first.data[index].x
        : view.second.data[index - view.first.size].x;
}

void test_sample_gap_sample_is_one_exact_stream() {
    MeasurementJournal<8, 4> journal;
    assert(journal.append_sample(0, sample(10)).status == StreamAppendStatus::Ok);
    assert(journal.append_sample(1, sample(11)).status == StreamAppendStatus::Ok);
    assert(journal.append_gap({2, 3, GapReason::SourceLoss}).status ==
           StreamAppendStatus::Ok);
    assert(journal.append_sample(5, sample(15)).status == StreamAppendStatus::Ok);

    MeasurementReadView view = journal.prepare_read(0, 8);
    assert(view.status == StreamReadStatus::Samples);
    assert(view.first_sequence == 0 && view.end_sequence == 2);
    assert(view.sample_count() == 2 && value_at(view, 1) == 11);
    assert(journal.mark_delivered(2) == StreamDeliveryStatus::Ok);

    view = journal.prepare_read(2, 8);
    assert(view.status == StreamReadStatus::Gap);
    assert(view.gap.first_missing == 2 && view.gap.missing_count == 3);
    assert(view.end_sequence == 5);
    assert(journal.mark_delivered(5) == StreamDeliveryStatus::Ok);

    view = journal.prepare_read(5, 8);
    assert(view.status == StreamReadStatus::Samples);
    assert(view.sample_count() == 1 && value_at(view, 0) == 15);
    assert(journal.mark_delivered(6) == StreamDeliveryStatus::Ok);

    const MeasurementJournalState state = journal.state();
    assert(state.produced == 6 && state.stored_samples == 3);
    assert(state.stored_gaps == 1 && state.delivered == 6);
}

void test_recovery_inside_gap_returns_only_the_remaining_range() {
    MeasurementJournal<4, 2> journal;
    journal.append_gap({0, 5, GapReason::IngressOverflow});
    const MeasurementReadView view = journal.prepare_read(2, 4);
    assert(view.status == StreamReadStatus::Gap);
    assert(view.gap.first_missing == 2 && view.gap.missing_count == 3);
    assert(journal.mark_delivered(5) == StreamDeliveryStatus::Ok);
    assert(journal.release_before(5) == StreamReleaseStatus::Ok);
    const MeasurementJournalState state = journal.state();
    assert(state.retained_from == 5 && state.released == 5);
    assert(state.stored_gaps == 0);
}

void test_sample_break_sample_preserves_unknown_extent_honestly() {
    MeasurementJournal<8, 4> journal;
    assert(journal.append_sample(0, sample(10)).status == StreamAppendStatus::Ok);
    assert(journal.append_continuity_break(
               {1, ContinuityBreakReason::SensorOverflow}).status ==
           StreamAppendStatus::Ok);
    assert(journal.append_sample(2, sample(12, 1, 2)).status ==
           StreamAppendStatus::Ok);

    MeasurementReadView view = journal.prepare_read(0, 8);
    assert(view.status == StreamReadStatus::Samples && view.end_sequence == 1);
    assert(journal.mark_delivered(1) == StreamDeliveryStatus::Ok);
    view = journal.prepare_read(1, 8);
    assert(view.status == StreamReadStatus::ContinuityBreak);
    assert(view.continuity_break.at_sequence == 1);
    assert(view.continuity_break.reason ==
           ContinuityBreakReason::SensorOverflow);
    assert(view.end_sequence == 2);
    assert(journal.mark_delivered(2) == StreamDeliveryStatus::Ok);
    view = journal.prepare_read(2, 8);
    assert(view.status == StreamReadStatus::Samples && view.end_sequence == 3);

    const MeasurementJournalState state = journal.state();
    assert(state.produced == 3 && state.stored_samples == 2);
    assert(state.stored_continuity_breaks == 1);
}

void test_release_removes_break_metadata_and_not_a_phantom_sample() {
    MeasurementJournal<2, 2> journal;
    journal.append_sample(0, sample(0));
    journal.append_continuity_break(
        {1, ContinuityBreakReason::SensorReadUncertain});
    journal.append_sample(2, sample(2, 1, 2));

    MeasurementReadView view = journal.prepare_read(0, 2);
    assert(journal.mark_delivered(view.end_sequence) == StreamDeliveryStatus::Ok);
    view = journal.prepare_read(1, 2);
    assert(journal.mark_delivered(view.end_sequence) == StreamDeliveryStatus::Ok);
    assert(journal.release_before(2) == StreamReleaseStatus::Ok);

    const MeasurementJournalState state = journal.state();
    assert(state.retained_from == 2 && state.stored_samples == 1);
    assert(state.stored_continuity_breaks == 0);
    assert(journal.prepare_read(2, 2).status == StreamReadStatus::Samples);
}

void test_release_removes_samples_and_gap_metadata_as_one_prefix() {
    MeasurementJournal<8, 4> journal;
    journal.append_sample(0, sample(0));
    journal.append_gap({1, 2, GapReason::SourceLoss});
    journal.append_sample(3, sample(3));

    MeasurementReadView view = journal.prepare_read(0, 8);
    assert(journal.mark_delivered(view.end_sequence) == StreamDeliveryStatus::Ok);
    view = journal.prepare_read(1, 8);
    assert(journal.mark_delivered(view.end_sequence) == StreamDeliveryStatus::Ok);
    view = journal.prepare_read(3, 8);
    assert(journal.mark_delivered(view.end_sequence) == StreamDeliveryStatus::Ok);

    assert(journal.release_before(3) == StreamReleaseStatus::Ok);
    MeasurementJournalState state = journal.state();
    assert(state.retained_from == 3 && state.released == 3);
    assert(state.stored_samples == 1 && state.stored_gaps == 0);
    assert(journal.prepare_read(2, 1).status ==
           StreamReadStatus::RangeUnavailable);
}

void test_sample_capacity_collapse_advances_over_preceding_gaps() {
    MeasurementJournal<2, 4> journal;
    journal.append_gap({0, 2, GapReason::SourceLoss});
    journal.append_sample(2, sample(2));
    journal.append_sample(3, sample(3));
    const StreamAppendResult appended = journal.append_sample(4, sample(4));
    assert(appended.status == StreamAppendStatus::Ok);
    assert(appended.retention_collapsed);

    const MeasurementJournalState state = journal.state();
    assert(state.retained_from == 3 && state.produced == 5);
    assert(state.discarded_samples == 1);
    assert(state.discarded_sequences == 3);
    assert(state.stored_samples == 2 && state.stored_gaps == 0);
    assert(journal.prepare_read(0, 2).status ==
           StreamReadStatus::RangeUnavailable);
}

void test_full_gap_ledger_collapses_an_old_prefix_fail_closed() {
    MeasurementJournal<8, 2> journal;
    journal.append_sample(0, sample(0));
    journal.append_gap({1, 1, GapReason::SourceLoss});
    journal.append_sample(2, sample(2));
    journal.append_gap({3, 1, GapReason::IngressOverflow});
    journal.append_sample(4, sample(4));
    const StreamAppendResult result =
        journal.append_gap({5, 1, GapReason::SourceLoss});
    assert(result.status == StreamAppendStatus::Ok);
    assert(result.retention_collapsed);

    const MeasurementJournalState state = journal.state();
    assert(state.retained_from == 2);
    assert(state.stored_samples == 2);
    assert(state.stored_gaps == 2);
    assert(state.discarded_samples == 1);
    assert(state.discarded_sequences == 2);
    assert(journal.prepare_read(1, 4).status ==
           StreamReadStatus::RangeUnavailable);
    assert(journal.prepare_read(2, 4).status == StreamReadStatus::Samples);
}

void test_rejects_non_contiguous_events_without_mutation() {
    MeasurementJournal<4, 2> journal;
    assert(journal.append_sample(1, sample(1)).status ==
           StreamAppendStatus::NonContiguous);
    assert(journal.append_gap({1, 1, GapReason::SourceLoss}).status ==
           StreamAppendStatus::NonContiguous);
    SampleRecord invalid = sample(0);
    invalid.config_revision = 0;
    assert(journal.append_sample(0, invalid).status ==
           StreamAppendStatus::InvalidSample);
    assert(journal.state().produced == 0);
}

struct TruthEvent {
    bool gap = false;
    GapReason reason = GapReason::UnknownDiscontinuity;
    int32_t value = 0;
    uint32_t config = 1;
    uint32_t segment = 1;
};

void verify_retained_stream(
    MeasurementJournal<37, 5>& journal,
    const std::vector<TruthEvent>& truth) {
    const MeasurementJournalState initial = journal.state();
    uint64_t cursor = initial.retained_from;
    while (cursor < initial.produced) {
        const MeasurementReadView view = journal.prepare_read(cursor, 7);
        if (truth[static_cast<size_t>(cursor)].gap) {
            assert(view.status == StreamReadStatus::Gap);
            const GapReason reason = truth[static_cast<size_t>(cursor)].reason;
            uint64_t expected_end = cursor;
            while (expected_end < initial.produced &&
                   truth[static_cast<size_t>(expected_end)].gap &&
                   truth[static_cast<size_t>(expected_end)].reason == reason) {
                ++expected_end;
            }
            assert(view.gap.first_missing == cursor);
            assert(view.gap.end() == expected_end);
        } else {
            assert(view.status == StreamReadStatus::Samples);
            assert(view.sample_count() != 0 && view.sample_count() <= 7);
            for (size_t index = 0; index < view.sample_count(); ++index) {
                const TruthEvent& expected =
                    truth[static_cast<size_t>(cursor) + index];
                assert(!expected.gap);
                assert(expected.config ==
                       truth[static_cast<size_t>(cursor)].config);
                assert(expected.segment ==
                       truth[static_cast<size_t>(cursor)].segment);
                assert(value_at(view, index) == expected.value);
            }
        }
        assert(view.end_sequence > cursor);
        assert(journal.mark_delivered(view.end_sequence) ==
               StreamDeliveryStatus::Ok);
        cursor = view.end_sequence;
    }
}

void test_deterministic_mixed_stream_matches_reference() {
    MeasurementJournal<37, 5> journal;
    std::vector<TruthEvent> truth;
    truth.reserve(50000);
    uint32_t random = 0x6d2b79f5u;
    auto next_random = [&random]() {
        random = random * 1664525u + 1013904223u;
        return random;
    };

    uint64_t cursor = 0;
    while (cursor < 50000) {
        const uint32_t choice = next_random();
        if ((choice % 11u) == 0u) {
            const uint64_t count = 1u + ((choice >> 8u) % 4u);
            const uint64_t bounded = count < 50000 - cursor
                ? count
                : 50000 - cursor;
            const GapReason reason = (choice & 1u) == 0u
                ? GapReason::SourceLoss
                : GapReason::IngressOverflow;
            assert(journal.append_gap({cursor, bounded, reason}).status ==
                   StreamAppendStatus::Ok);
            for (uint64_t index = 0; index < bounded; ++index) {
                truth.push_back({true, reason, 0, 0, 0});
            }
            cursor += bounded;
        } else {
            const int32_t value = static_cast<int32_t>(cursor % 10000u);
            const uint32_t config = static_cast<uint32_t>(cursor / 17u) + 1u;
            const uint32_t segment = static_cast<uint32_t>(cursor / 29u) + 1u;
            assert(journal.append_sample(
                       cursor, sample(value, config, segment)).status ==
                   StreamAppendStatus::Ok);
            truth.push_back({false, GapReason::UnknownDiscontinuity,
                             value, config, segment});
            ++cursor;
        }

        if ((cursor % 503u) < 4u) verify_retained_stream(journal, truth);
    }
    verify_retained_stream(journal, truth);
}

}  // namespace

int main() {
    test_sample_gap_sample_is_one_exact_stream();
    test_recovery_inside_gap_returns_only_the_remaining_range();
    test_sample_break_sample_preserves_unknown_extent_honestly();
    test_release_removes_break_metadata_and_not_a_phantom_sample();
    test_release_removes_samples_and_gap_metadata_as_one_prefix();
    test_sample_capacity_collapse_advances_over_preceding_gaps();
    test_full_gap_ledger_collapses_an_old_prefix_fail_closed();
    test_rejects_non_contiguous_events_without_mutation();
    test_deterministic_mixed_stream_matches_reference();
    return 0;
}
