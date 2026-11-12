use core::fmt;

use crate::{BootEpoch, HardwareId, SampleSequence};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GapReason {
    UnknownDiscontinuity,
    SourceLoss,
    IngressOverflow,
    JournalOverflow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Gap {
    pub hardware_id: HardwareId,
    pub boot_epoch: BootEpoch,
    pub first_missing: SampleSequence,
    pub missing_count: u64,
    pub reason: GapReason,
}

impl Gap {
    pub fn new(
        hardware_id: HardwareId,
        boot_epoch: BootEpoch,
        first_missing: SampleSequence,
        missing_count: u64,
        reason: GapReason,
    ) -> Result<Self, GapError> {
        if missing_count == 0 {
            return Err(GapError::Empty);
        }
        // The exclusive cursor after the gap must also fit in u64. A missing
        // sample at sequence u64::MAX cannot be represented by this cursor model.
        if first_missing.checked_add(missing_count).is_none() {
            return Err(GapError::SequenceOverflow);
        }
        Ok(Self {
            hardware_id,
            boot_epoch,
            first_missing,
            missing_count,
            reason,
        })
    }

    pub fn last_missing(self) -> SampleSequence {
        self.first_missing
            .checked_add(self.missing_count - 1)
            .expect("Gap constructor validates the sequence range")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GapError {
    Empty,
    SequenceOverflow,
}

impl fmt::Display for GapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("gap must contain at least one missing sample"),
            Self::SequenceOverflow => formatter.write_str("gap sequence range overflows u64"),
        }
    }
}

impl std::error::Error for GapError {}

#[cfg(test)]
mod tests {
    use super::{Gap, GapError, GapReason};
    use crate::{BootEpoch, HardwareId, SampleSequence};

    #[test]
    fn preserves_exact_missing_range() {
        let gap = Gap::new(
            HardwareId::from_bytes([7, 0, 0, 0, 0, 0, 0, 0]),
            BootEpoch::new(2),
            SampleSequence::new(100),
            3,
            GapReason::SourceLoss,
        )
        .unwrap();

        assert_eq!(gap.last_missing(), SampleSequence::new(102));
    }

    #[test]
    fn rejects_empty_and_overflowing_ranges() {
        assert_eq!(
            Gap::new(
                HardwareId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0]),
                BootEpoch::new(1),
                SampleSequence::new(1),
                0,
                GapReason::UnknownDiscontinuity,
            ),
            Err(GapError::Empty)
        );
        assert_eq!(
            Gap::new(
                HardwareId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0]),
                BootEpoch::new(1),
                SampleSequence::new(u64::MAX),
                2,
                GapReason::UnknownDiscontinuity,
            ),
            Err(GapError::SequenceOverflow)
        );
    }
}
