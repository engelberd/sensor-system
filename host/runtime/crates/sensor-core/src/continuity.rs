use core::fmt;

use crate::{BootEpoch, HardwareId, SequenceCursor};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContinuityBreakReason {
    Unknown,
    SensorOverflow,
    SensorReadUncertain,
    InvalidTiming,
    ProcessingReset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContinuityBreak {
    pub hardware_id: HardwareId,
    pub boot_epoch: BootEpoch,
    pub at: SequenceCursor,
    pub reason: ContinuityBreakReason,
}

impl ContinuityBreak {
    pub fn new(
        hardware_id: HardwareId,
        boot_epoch: BootEpoch,
        at: SequenceCursor,
        reason: ContinuityBreakReason,
    ) -> Result<Self, ContinuityBreakError> {
        if at.checked_advance(1).is_none() {
            return Err(ContinuityBreakError::SequenceOverflow);
        }
        Ok(Self {
            hardware_id,
            boot_epoch,
            at,
            reason,
        })
    }

    pub fn end(self) -> SequenceCursor {
        self.at
            .checked_advance(1)
            .expect("ContinuityBreak constructor validates the cursor")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContinuityBreakError {
    SequenceOverflow,
}

impl fmt::Display for ContinuityBreakError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("continuity break cursor overflows u64")
    }
}

impl std::error::Error for ContinuityBreakError {}

#[cfg(test)]
mod tests {
    use super::{ContinuityBreak, ContinuityBreakError, ContinuityBreakReason};
    use crate::{BootEpoch, HardwareId, SequenceCursor};

    #[test]
    fn break_occupies_one_cursor_position_without_claiming_a_sample_count() {
        let value = ContinuityBreak::new(
            HardwareId::from_bytes([7, 0, 0, 0, 0, 0, 0, 0]),
            BootEpoch::new(2),
            SequenceCursor::new(100),
            ContinuityBreakReason::SensorOverflow,
        )
        .unwrap();
        assert_eq!(value.end(), SequenceCursor::new(101));
    }

    #[test]
    fn rejects_the_last_unadvanceable_cursor() {
        assert_eq!(
            ContinuityBreak::new(
                HardwareId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0]),
                BootEpoch::new(1),
                SequenceCursor::new(u64::MAX),
                ContinuityBreakReason::Unknown,
            ),
            Err(ContinuityBreakError::SequenceOverflow)
        );
    }
}
