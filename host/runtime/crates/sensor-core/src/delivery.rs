use core::fmt;

use crate::SampleSequence;

/// Exclusive position in one sample stream.
///
/// A cursor value `N` describes the contiguous prefix `[0, N)`. The first
/// sample not covered by that cursor has sequence `N`.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SequenceCursor(u64);

impl SequenceCursor {
    pub const ORIGIN: Self = Self(0);

    pub const fn new(next_sequence: u64) -> Self {
        Self(next_sequence)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub const fn next_sample(self) -> SampleSequence {
        SampleSequence::new(self.0)
    }

    pub const fn checked_advance(self, count: u64) -> Option<Self> {
        match self.0.checked_add(count) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

/// Three monotonic boundaries owned by one host session.
///
/// `received` advances only over contiguous samples or an explicit confirmed
/// gap. `durable` advances only after crash-safe storage of those samples or
/// the gap evidence. `released` advances only after the node accepts permission
/// to discard the same prefix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeliveryCursors {
    received: SequenceCursor,
    durable: SequenceCursor,
    released: SequenceCursor,
}

impl DeliveryCursors {
    pub const fn empty() -> Self {
        Self {
            received: SequenceCursor::ORIGIN,
            durable: SequenceCursor::ORIGIN,
            released: SequenceCursor::ORIGIN,
        }
    }

    pub fn new(
        received: SequenceCursor,
        durable: SequenceCursor,
        released: SequenceCursor,
    ) -> Result<Self, CursorError> {
        if durable > received {
            return Err(CursorError::DurableAheadOfReceived);
        }
        if released > durable {
            return Err(CursorError::ReleasedAheadOfDurable);
        }
        Ok(Self {
            received,
            durable,
            released,
        })
    }

    pub const fn received(self) -> SequenceCursor {
        self.received
    }

    pub const fn durable(self) -> SequenceCursor {
        self.durable
    }

    pub const fn released(self) -> SequenceCursor {
        self.released
    }

    pub fn accept_contiguous(
        &mut self,
        first: SampleSequence,
        count: u16,
    ) -> Result<SequenceCursor, CursorError> {
        if count == 0 {
            return Err(CursorError::EmptyChunk);
        }
        if first != self.received.next_sample() {
            return Err(CursorError::NonContiguous {
                expected: self.received.next_sample(),
                received: first,
            });
        }
        self.received = self
            .received
            .checked_advance(u64::from(count))
            .ok_or(CursorError::SequenceOverflow)?;
        Ok(self.received)
    }

    pub fn account_gap(
        &mut self,
        first: SampleSequence,
        count: u64,
    ) -> Result<SequenceCursor, CursorError> {
        if count == 0 {
            return Err(CursorError::EmptyGap);
        }
        if first != self.received.next_sample() {
            return Err(CursorError::NonContiguous {
                expected: self.received.next_sample(),
                received: first,
            });
        }
        self.received = self
            .received
            .checked_advance(count)
            .ok_or(CursorError::SequenceOverflow)?;
        Ok(self.received)
    }

    pub fn account_continuity_break(
        &mut self,
        at: SequenceCursor,
    ) -> Result<SequenceCursor, CursorError> {
        if at != self.received {
            return Err(CursorError::NonContiguous {
                expected: self.received.next_sample(),
                received: at.next_sample(),
            });
        }
        self.received = self
            .received
            .checked_advance(1)
            .ok_or(CursorError::SequenceOverflow)?;
        Ok(self.received)
    }

    pub fn mark_durable(&mut self, cursor: SequenceCursor) -> Result<(), CursorError> {
        if cursor < self.durable {
            return Err(CursorError::DurableRegression);
        }
        if cursor > self.received {
            return Err(CursorError::DurableAheadOfReceived);
        }
        self.durable = cursor;
        Ok(())
    }

    pub fn mark_released(&mut self, cursor: SequenceCursor) -> Result<(), CursorError> {
        if cursor < self.released {
            return Err(CursorError::ReleasedRegression);
        }
        if cursor > self.durable {
            return Err(CursorError::ReleasedAheadOfDurable);
        }
        self.released = cursor;
        Ok(())
    }
}

impl Default for DeliveryCursors {
    fn default() -> Self {
        Self::empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorError {
    EmptyChunk,
    EmptyGap,
    NonContiguous {
        expected: SampleSequence,
        received: SampleSequence,
    },
    SequenceOverflow,
    DurableRegression,
    DurableAheadOfReceived,
    ReleasedRegression,
    ReleasedAheadOfDurable,
}

impl fmt::Display for CursorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyChunk => formatter.write_str("a received chunk must contain samples"),
            Self::EmptyGap => formatter.write_str("an accounted gap must contain samples"),
            Self::NonContiguous { expected, received } => write!(
                formatter,
                "non-contiguous chunk: expected first sequence {}, received {}",
                expected.get(),
                received.get()
            ),
            Self::SequenceOverflow => formatter.write_str("sample cursor overflows u64"),
            Self::DurableRegression => formatter.write_str("durable cursor cannot move backward"),
            Self::DurableAheadOfReceived => {
                formatter.write_str("durable cursor cannot pass received cursor")
            }
            Self::ReleasedRegression => formatter.write_str("released cursor cannot move backward"),
            Self::ReleasedAheadOfDurable => {
                formatter.write_str("released cursor cannot pass durable cursor")
            }
        }
    }
}

impl std::error::Error for CursorError {}

#[cfg(test)]
mod tests {
    use super::{CursorError, DeliveryCursors, SequenceCursor};
    use crate::SampleSequence;

    #[test]
    fn cursor_zero_naturally_describes_an_empty_prefix() {
        let cursors = DeliveryCursors::empty();
        assert_eq!(cursors.received().next_sample(), SampleSequence::new(0));
        assert_eq!(cursors.durable(), SequenceCursor::ORIGIN);
        assert_eq!(cursors.released(), SequenceCursor::ORIGIN);
    }

    #[test]
    fn advances_only_for_the_exact_next_chunk() {
        let mut cursors = DeliveryCursors::empty();
        assert_eq!(
            cursors.accept_contiguous(SampleSequence::new(0), 32),
            Ok(SequenceCursor::new(32))
        );

        assert_eq!(
            cursors.accept_contiguous(SampleSequence::new(64), 32),
            Err(CursorError::NonContiguous {
                expected: SampleSequence::new(32),
                received: SampleSequence::new(64),
            })
        );
        assert_eq!(cursors.received(), SequenceCursor::new(32));
    }

    #[test]
    fn cannot_release_samples_across_an_unpersisted_gap() {
        let mut cursors = DeliveryCursors::empty();
        cursors
            .accept_contiguous(SampleSequence::new(0), 32)
            .unwrap();
        cursors.mark_durable(SequenceCursor::new(32)).unwrap();

        assert_eq!(
            cursors.mark_released(SequenceCursor::new(96)),
            Err(CursorError::ReleasedAheadOfDurable)
        );
        assert_eq!(cursors.released(), SequenceCursor::ORIGIN);
    }

    #[test]
    fn advances_across_a_gap_only_when_it_starts_at_the_current_cursor() {
        let mut cursors = DeliveryCursors::empty();
        assert_eq!(
            cursors.account_gap(SampleSequence::new(0), 3),
            Ok(SequenceCursor::new(3))
        );
        assert_eq!(cursors.mark_durable(SequenceCursor::new(3)), Ok(()));
        assert_eq!(
            cursors.account_gap(SampleSequence::new(4), 1),
            Err(CursorError::NonContiguous {
                expected: SampleSequence::new(3),
                received: SampleSequence::new(4),
            })
        );
    }

    #[test]
    fn continuity_break_advances_one_logical_marker() {
        let mut cursors = DeliveryCursors::empty();
        assert_eq!(
            cursors.account_continuity_break(SequenceCursor::ORIGIN),
            Ok(SequenceCursor::new(1))
        );
        assert_eq!(
            cursors.account_continuity_break(SequenceCursor::new(2)),
            Err(CursorError::NonContiguous {
                expected: SampleSequence::new(1),
                received: SampleSequence::new(2),
            })
        );
    }

    #[test]
    fn rejects_invalid_initial_order_and_regressions() {
        assert_eq!(
            DeliveryCursors::new(
                SequenceCursor::new(10),
                SequenceCursor::new(11),
                SequenceCursor::new(0),
            ),
            Err(CursorError::DurableAheadOfReceived)
        );

        let mut cursors = DeliveryCursors::new(
            SequenceCursor::new(20),
            SequenceCursor::new(15),
            SequenceCursor::new(10),
        )
        .unwrap();
        assert_eq!(
            cursors.mark_durable(SequenceCursor::new(14)),
            Err(CursorError::DurableRegression)
        );
        assert_eq!(
            cursors.mark_released(SequenceCursor::new(9)),
            Err(CursorError::ReleasedRegression)
        );
    }
}
