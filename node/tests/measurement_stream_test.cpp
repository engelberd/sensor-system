#include <cassert>
#include <cstdint>

#include "measurement/source_sequencer.h"
#include "measurement/stream_gap.h"

namespace {
using namespace measurement;

void test_source_sequence_is_claimed_before_transport() {
    SourceSequencer sequence;
    const SequenceClaim first = sequence.claim_sample();
    assert(first.status == SequenceClaimStatus::Ok && first.first == 0);

    GapRecord lost{};
    const SequenceClaim gap =
        sequence.claim_gap(3, GapReason::IngressOverflow, lost);
    assert(gap.status == SequenceClaimStatus::Ok);
    assert(lost.first_missing == 1 && lost.missing_count == 3);
    assert(lost.end() == 4);

    const SequenceClaim next = sequence.claim_sample();
    assert(next.status == SequenceClaimStatus::Ok && next.first == 4);
    assert(sequence.cursor() == 5);
}

void test_sequence_rejects_invalid_and_unrepresentable_claims() {
    SourceSequencer sequence(UINT64_MAX - 1);
    assert(sequence.claim(0).status == SequenceClaimStatus::InvalidCount);
    assert(sequence.claim_sample().status == SequenceClaimStatus::Ok);
    assert(sequence.cursor() == UINT64_MAX);
    assert(sequence.claim_sample().status == SequenceClaimStatus::Exhausted);
    assert(sequence.cursor() == UINT64_MAX);
}

void test_ledger_coalesces_only_adjacent_equal_reasons() {
    GapLedger<4> ledger;
    assert(ledger.append({2, 2, GapReason::SourceLoss}) == GapAppendStatus::Ok);
    assert(ledger.append({4, 3, GapReason::SourceLoss}) ==
           GapAppendStatus::Coalesced);
    assert(ledger.size() == 1);
    assert(ledger.oldest()->first_missing == 2);
    assert(ledger.oldest()->missing_count == 5);

    assert(ledger.append({7, 1, GapReason::IngressOverflow}) ==
           GapAppendStatus::Ok);
    assert(ledger.append({10, 1, GapReason::IngressOverflow}) ==
           GapAppendStatus::Ok);
    assert(ledger.size() == 3);
}

void test_lookup_returns_an_exact_remaining_gap() {
    GapLedger<4> ledger;
    ledger.append({5, 4, GapReason::SourceLoss});
    ledger.append({12, 2, GapReason::IngressOverflow});

    assert(!ledger.containing(4).found);
    const GapLookup whole = ledger.containing(5);
    assert(whole.found && whole.gap.first_missing == 5);
    assert(whole.gap.missing_count == 4);

    const GapLookup remainder = ledger.containing(7);
    assert(remainder.found && remainder.gap.first_missing == 7);
    assert(remainder.gap.missing_count == 2);
    assert(remainder.gap.reason == GapReason::SourceLoss);

    const GapLookup next = ledger.next_from(9);
    assert(next.found && next.gap.first_missing == 12);
}

void test_discard_trims_and_wraps_without_losing_order() {
    GapLedger<3> ledger;
    ledger.append({2, 4, GapReason::SourceLoss});
    ledger.append({8, 1, GapReason::IngressOverflow});
    ledger.discard_before(4);
    assert(ledger.oldest()->first_missing == 4);
    assert(ledger.oldest()->missing_count == 2);

    ledger.discard_before(8);
    assert(ledger.size() == 1 && ledger.oldest()->first_missing == 8);
    ledger.discard_before(9);
    assert(ledger.empty());

    ledger.append({10, 1, GapReason::SourceLoss});
    ledger.append({12, 1, GapReason::IngressOverflow});
    ledger.append({14, 1, GapReason::SourceLoss});
    assert(ledger.size() == 3);
    ledger.discard_before(13);
    assert(ledger.size() == 1 && ledger.oldest()->first_missing == 14);
    assert(ledger.append({16, 1, GapReason::IngressOverflow}) ==
           GapAppendStatus::Ok);
}

void test_ledger_fails_closed_without_mutation() {
    GapLedger<2> ledger;
    assert(ledger.append({3, 0, GapReason::SourceLoss}) ==
           GapAppendStatus::InvalidGap);
    assert(ledger.append({3, 2, GapReason::SourceLoss}) == GapAppendStatus::Ok);
    assert(ledger.append({4, 1, GapReason::IngressOverflow}) ==
           GapAppendStatus::OutOfOrder);
    assert(ledger.append({7, 1, GapReason::IngressOverflow}) ==
           GapAppendStatus::Ok);
    assert(ledger.append({9, 1, GapReason::SourceLoss}) ==
           GapAppendStatus::Full);
    assert(ledger.size() == 2);
    assert(ledger.oldest()->first_missing == 3);
}

}  // namespace

int main() {
    test_source_sequence_is_claimed_before_transport();
    test_sequence_rejects_invalid_and_unrepresentable_claims();
    test_ledger_coalesces_only_adjacent_equal_reasons();
    test_lookup_returns_an_exact_remaining_gap();
    test_discard_trims_and_wraps_without_losing_order();
    test_ledger_fails_closed_without_mutation();
    return 0;
}
