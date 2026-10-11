#include <cassert>
#include <cstddef>
#include <cstdint>
#include <type_traits>

#include "measurement/delivery_session.h"

namespace {
using namespace measurement;

constexpr uint64_t kBootEpoch = 0x1122334455667788ull;

SampleRecord sample(
    int32_t value, uint64_t time_us, uint32_t config = 1,
    uint32_t segment = 1, uint16_t timing_flags = kTimingValidFlag) {
    return {time_us, value, -value, value + 1, config, segment, timing_flags, 0};
}

template <size_t Capacity, size_t GapCapacity>
void append_one(
    MeasurementJournal<Capacity, GapCapacity>& journal,
    const SampleRecord& record) {
    assert(journal.append_sample(journal.state().produced, record).status ==
           StreamAppendStatus::Ok);
}

void append_linear_samples(MeasurementJournal<8, 8>& journal, int32_t count) {
    for (int32_t value = 0; value < count; ++value) {
        append_one(journal, sample(value, 1000u + 4000u * value));
    }
}

void test_prepare_complete_retry_and_chunk_limit() {
    MeasurementJournal<8, 8> journal;
    append_linear_samples(journal, 6);
    DeliverySession<8, 4> session(kBootEpoch, journal);

    const PullCommand command{kBootEpoch, 0, 0, 48};
    const PullPlan first = session.prepare_pull(command);
    assert(first.decision == PullDecision::SampleChunk);
    assert(first.chunk.first_sequence == 0 && first.chunk.end_sequence == 4);
    assert(first.chunk.samples.sample_count() == 4);
    assert(first.chunk.timing.first_device_time_us == 1000);
    assert(first.chunk.timing.last_device_time_us == 13000);
    assert(first.chunk.timing.max_fit_residual_us == 0);
    assert(session.has_prepared_chunk());

    assert(session.prepare_pull(command).decision == PullDecision::Busy);
    assert(session.complete(first.chunk.token + 1) == CompletionStatus::TokenMismatch);
    assert(session.complete(first.chunk.token) == CompletionStatus::Ok);
    assert(journal.state().delivered == 4);

    const PullPlan retry = session.prepare_pull(command);
    assert(retry.decision == PullDecision::SampleChunk);
    assert(retry.chunk.first_sequence == 0 && retry.chunk.end_sequence == 4);
    assert(session.abort(retry.chunk.token) == CompletionStatus::Ok);
    assert(!session.has_prepared_chunk());
    assert(journal.state().delivered == 4);
}

void test_release_is_bounded_and_piggybacked() {
    MeasurementJournal<8, 8> journal;
    append_linear_samples(journal, 6);
    DeliverySession<8, 4> session(kBootEpoch, journal);

    PullPlan plan = session.prepare_pull({kBootEpoch, 0, 0, 4});
    assert(session.complete(plan.chunk.token) == CompletionStatus::Ok);

    plan = session.prepare_pull({kBootEpoch, 4, 4, 4});
    assert(plan.decision == PullDecision::SampleChunk);
    assert(plan.state.retained_from == 4 && plan.state.released == 4);
    assert(plan.chunk.first_sequence == 4 && plan.chunk.end_sequence == 6);
    assert(session.abort(plan.chunk.token) == CompletionStatus::Ok);

    plan = session.prepare_pull({kBootEpoch, 5, 5, 1});
    assert(plan.decision == PullDecision::InvalidRelease);
    assert(journal.state().retained_from == 4);

    const ReleaseResult released = session.release({kBootEpoch, 4});
    assert(released.decision == ReleaseDecision::Applied);
    assert(released.state.released == 4);
}

void test_fail_closed_requests_do_not_mutate_state() {
    MeasurementJournal<8, 8> journal;
    append_linear_samples(journal, 2);
    DeliverySession<8, 4> session(kBootEpoch, journal);
    const MeasurementJournalState before = journal.state();

    PullPlan plan = session.prepare_pull({kBootEpoch + 1, 0, 0, 1});
    assert(plan.decision == PullDecision::StreamChanged);
    assert(plan.actual_boot_epoch == kBootEpoch);
    plan = session.prepare_pull({kBootEpoch, 0, 1, 1});
    assert(plan.decision == PullDecision::InvalidRelease);
    plan = session.prepare_pull({kBootEpoch, 0, 0, 0});
    assert(plan.decision == PullDecision::InvalidRequest);
    plan = session.prepare_pull({kBootEpoch, 3, 0, 1});
    assert(plan.decision == PullDecision::CursorAhead);

    const MeasurementJournalState after = journal.state();
    assert(after.retained_from == before.retained_from);
    assert(after.produced == before.produced);
    assert(after.released == before.released);
}

void test_overflow_produces_range_unavailable_and_gap_recovery() {
    MeasurementJournal<3, 8> journal;
    for (int32_t value = 0; value < 5; ++value) {
        append_one(journal, sample(value, 1000u + 4000u * value));
    }
    DeliverySession<3, 3> session(kBootEpoch, journal);

    PullPlan plan = session.prepare_pull({kBootEpoch, 0, 0, 3});
    assert(plan.decision == PullDecision::RangeUnavailable);
    assert(plan.state.retained_from == 2 && plan.state.produced == 5);

    plan = session.prepare_pull({kBootEpoch, 2, 2, 3});
    assert(plan.decision == PullDecision::SampleChunk);
    assert(plan.state.released == 2);
    assert(plan.chunk.first_sequence == 2 && plan.chunk.end_sequence == 5);
    assert(session.complete(plan.chunk.token) == CompletionStatus::Ok);
}

void test_exact_gap_uses_the_same_completion_token_as_samples() {
    MeasurementJournal<8, 8> journal;
    append_one(journal, sample(0, 1000));
    assert(journal.append_gap({1, 2, GapReason::IngressOverflow}).status ==
           StreamAppendStatus::Ok);
    append_one(journal, sample(3, 13000));
    DeliverySession<8, 4> session(kBootEpoch, journal);

    PullPlan plan = session.prepare_pull({kBootEpoch, 0, 0, 4});
    assert(plan.decision == PullDecision::SampleChunk);
    assert(plan.chunk.end_sequence == 1);
    assert(session.complete(plan.chunk.token) == CompletionStatus::Ok);

    plan = session.prepare_pull({kBootEpoch, 1, 0, 4});
    assert(plan.decision == PullDecision::Gap);
    assert(plan.gap.gap.first_missing == 1);
    assert(plan.gap.gap.missing_count == 2);
    assert(journal.state().delivered == 1);
    assert(session.complete(plan.gap.token + 1) == CompletionStatus::TokenMismatch);
    assert(session.complete(plan.gap.token) == CompletionStatus::Ok);
    assert(journal.state().delivered == 3);

    plan = session.prepare_pull({kBootEpoch, 1, 0, 4});
    assert(plan.decision == PullDecision::Gap);
    assert(session.abort(plan.gap.token) == CompletionStatus::Ok);
    assert(journal.state().delivered == 3);
}

void test_continuity_break_uses_delivery_token_without_fake_gap_count() {
    MeasurementJournal<8, 8> journal;
    append_one(journal, sample(0, 1000));
    assert(journal.append_continuity_break(
               {1, ContinuityBreakReason::InvalidTiming}).status ==
           StreamAppendStatus::Ok);
    append_one(journal, sample(2, 9000, 1, 2));
    DeliverySession<8, 4> session(kBootEpoch, journal);

    PullPlan plan = session.prepare_pull({kBootEpoch, 0, 0, 4});
    assert(plan.decision == PullDecision::SampleChunk);
    assert(session.complete(plan.chunk.token) == CompletionStatus::Ok);

    plan = session.prepare_pull({kBootEpoch, 1, 0, 4});
    assert(plan.decision == PullDecision::ContinuityBreak);
    assert(plan.continuity_break.value.at_sequence == 1);
    assert(plan.continuity_break.value.reason ==
           ContinuityBreakReason::InvalidTiming);
    assert(journal.state().delivered == 1);
    assert(session.complete(plan.continuity_break.token) == CompletionStatus::Ok);
    assert(journal.state().delivered == 2);
}

void test_mutation_invalidates_prepared_completion() {
    MeasurementJournal<8, 8> journal;
    append_linear_samples(journal, 2);
    DeliverySession<8, 4> session(kBootEpoch, journal);

    const PullPlan plan = session.prepare_pull({kBootEpoch, 0, 0, 2});
    assert(plan.decision == PullDecision::SampleChunk);
    append_one(journal, sample(2, 9000));
    assert(session.complete(plan.chunk.token) == CompletionStatus::JournalChanged);
    assert(!session.has_prepared_chunk());
    assert(journal.state().delivered == 0);
}

void test_timing_summary_aggregates_quality_and_residual() {
    MeasurementJournal<8, 8> journal;
    append_one(journal, sample(0, 1000, 1, 1, kTimingValidFlag));
    append_one(journal, sample(1, 5100, 1, 1, kTimingValidFlag | (1u << 1u)));
    append_one(journal, sample(2, 9000, 1, 1, 0));
    DeliverySession<8, 4> session(kBootEpoch, journal);

    const PullPlan plan = session.prepare_pull({kBootEpoch, 0, 0, 3});
    assert(plan.decision == PullDecision::SampleChunk);
    assert(plan.chunk.timing.first_device_time_us == 1000);
    assert(plan.chunk.timing.last_device_time_us == 9000);
    assert(plan.chunk.timing.max_fit_residual_us == 100);
    assert((plan.chunk.timing.quality_flags & kTimingValidFlag) == 0);
    assert((plan.chunk.timing.quality_flags & (1u << 1u)) != 0);

    assert(session.release({kBootEpoch, 0}).decision == ReleaseDecision::Busy);
    assert(session.abort(plan.chunk.token) == CompletionStatus::Ok);
}

void test_invalid_timing_fails_closed() {
    MeasurementJournal<8, 8> journal;
    append_one(journal, sample(0, 1000));
    append_one(journal, sample(1, 900));
    append_one(journal, sample(2, 2000));
    DeliverySession<8, 4> session(kBootEpoch, journal);
    assert(session.prepare_pull({kBootEpoch, 0, 0, 3}).decision ==
           PullDecision::InternalError);
    assert(!session.has_prepared_chunk());

    MeasurementJournal<8, 8> equal_timestamps;
    append_one(equal_timestamps, sample(0, 1000));
    append_one(equal_timestamps, sample(1, 1000));
    DeliverySession<8, 4> equal_session(kBootEpoch, equal_timestamps);
    assert(equal_session.prepare_pull({kBootEpoch, 0, 0, 2}).decision ==
           PullDecision::InternalError);

    MeasurementJournal<8, 8> valid_journal;
    DeliverySession<8, 4> invalid_epoch_session(0, valid_journal);
    assert(invalid_epoch_session.release({0, 0}).decision ==
           ReleaseDecision::InternalError);
}

void test_destructor_cancels_outstanding_preparation() {
    MeasurementJournal<8, 8> journal;
    append_linear_samples(journal, 2);
    uint64_t end = 0;
    {
        DeliverySession<8, 4> session(kBootEpoch, journal);
        const PullPlan plan = session.prepare_pull({kBootEpoch, 0, 0, 2});
        assert(plan.decision == PullDecision::SampleChunk);
        end = plan.chunk.end_sequence;
    }
    assert(journal.mark_delivered(end) ==
           StreamDeliveryStatus::BeyondPreparedRange);
}
}  // namespace

int main() {
    static_assert(!std::is_copy_constructible<DeliverySession<8, 4>>::value,
                  "delivery session must have one owner");
    test_prepare_complete_retry_and_chunk_limit();
    test_release_is_bounded_and_piggybacked();
    test_fail_closed_requests_do_not_mutate_state();
    test_overflow_produces_range_unavailable_and_gap_recovery();
    test_exact_gap_uses_the_same_completion_token_as_samples();
    test_continuity_break_uses_delivery_token_without_fake_gap_count();
    test_mutation_invalidates_prepared_completion();
    test_timing_summary_aggregates_quality_and_residual();
    test_invalid_timing_fails_closed();
    test_destructor_cancels_outstanding_preparation();
    return 0;
}
