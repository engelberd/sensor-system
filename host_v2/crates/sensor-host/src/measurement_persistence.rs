use core::fmt;

use sensor_core::{ContinuityBreak, Gap, HardwareId, SequenceCursor};
use sensor_protocol::{MeasurementContractError, SampleChunk};

use crate::measurement_transfer::{MeasurementTransfer, MeasurementTransferError};
use crate::node_session::{DurableReceipt, SessionPhase};

/// Persistence boundary for the recorder's durable data product.
///
/// Returning `Ok(())` means the supplied record and its ordering metadata will
/// survive process and host restart according to the configured storage policy.
/// Buffering data only in userspace must not return success.
pub trait DurableMeasurementSink {
    type Error;

    fn persist_chunk(
        &mut self,
        hardware_id: HardwareId,
        chunk: &SampleChunk,
    ) -> Result<(), Self::Error>;

    fn persist_gap(&mut self, gap: Gap) -> Result<(), Self::Error>;

    fn persist_continuity_break(&mut self, observed: ContinuityBreak) -> Result<(), Self::Error>;
}

pub fn persist_pending<S>(
    transfer: &mut MeasurementTransfer,
    sink: &mut S,
) -> Result<DurableReceipt, PersistenceError<S::Error>>
where
    S: DurableMeasurementSink,
{
    let session = transfer.session();
    let receipt = match session.phase() {
        SessionPhase::PersistingChunk => {
            let chunk = session
                .pending_wire_chunk()
                .ok_or(PersistenceError::NonWireChunk)?;
            sink.persist_chunk(session.hardware_id(), chunk)
                .map_err(PersistenceError::Sink)?;
            DurableReceipt {
                hardware_id: session.hardware_id(),
                boot_epoch: session.boot_epoch(),
                end: chunk.header.end().map_err(PersistenceError::Contract)?,
            }
        }
        SessionPhase::PersistingGap => {
            let gap = session
                .pending_gap()
                .ok_or(PersistenceError::MissingPending)?;
            sink.persist_gap(gap).map_err(PersistenceError::Sink)?;
            let end = gap
                .first_missing
                .get()
                .checked_add(gap.missing_count)
                .ok_or(PersistenceError::CursorOverflow)?;
            DurableReceipt {
                hardware_id: gap.hardware_id,
                boot_epoch: gap.boot_epoch,
                end: SequenceCursor::new(end),
            }
        }
        SessionPhase::PersistingContinuityBreak => {
            let observed = session
                .pending_continuity_break()
                .ok_or(PersistenceError::MissingPending)?;
            sink.persist_continuity_break(observed)
                .map_err(PersistenceError::Sink)?;
            DurableReceipt {
                hardware_id: observed.hardware_id,
                boot_epoch: observed.boot_epoch,
                end: observed.end(),
            }
        }
        _ => return Err(PersistenceError::UnexpectedPhase),
    };
    transfer.confirm_durable(receipt)?;
    Ok(receipt)
}

#[derive(Debug)]
pub enum PersistenceError<E> {
    UnexpectedPhase,
    MissingPending,
    NonWireChunk,
    CursorOverflow,
    Contract(MeasurementContractError),
    Sink(E),
    Transfer(MeasurementTransferError),
}

impl<E> From<MeasurementTransferError> for PersistenceError<E> {
    fn from(value: MeasurementTransferError) -> Self {
        Self::Transfer(value)
    }
}

impl<E> fmt::Display for PersistenceError<E>
where
    E: fmt::Display,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedPhase => formatter.write_str("measurement has no record to persist"),
            Self::MissingPending => formatter.write_str("measurement pending record is missing"),
            Self::NonWireChunk => formatter.write_str("legacy chunk cannot enter recorder v2"),
            Self::CursorOverflow => formatter.write_str("durable cursor overflow"),
            Self::Contract(error) => write!(formatter, "measurement contract failed: {error}"),
            Self::Sink(error) => write!(formatter, "durable sink failed: {error}"),
            Self::Transfer(error) => write!(formatter, "delivery confirmation failed: {error}"),
        }
    }
}

impl<E> std::error::Error for PersistenceError<E> where E: std::error::Error + 'static {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_session::NodeSession;
    use sensor_core::{BootEpoch, SampleAxes, SampleAxis};
    use sensor_protocol::{
        encode_pull_response, PullResponse, SampleChunkHeader, TimingEvidence, MAX_PAYLOAD_BYTES,
        TIMING_VALID,
    };

    const HARDWARE_ID: HardwareId = HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);

    #[derive(Debug, Eq, PartialEq)]
    struct SinkFailure;

    impl fmt::Display for SinkFailure {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("injected failure")
        }
    }

    impl std::error::Error for SinkFailure {}

    struct Sink {
        fail: bool,
        persisted_revision: Option<u32>,
    }

    impl DurableMeasurementSink for Sink {
        type Error = SinkFailure;

        fn persist_chunk(
            &mut self,
            hardware_id: HardwareId,
            chunk: &SampleChunk,
        ) -> Result<(), Self::Error> {
            assert_eq!(hardware_id, HARDWARE_ID);
            if self.fail {
                return Err(SinkFailure);
            }
            self.persisted_revision = Some(chunk.config_revision);
            Ok(())
        }

        fn persist_gap(&mut self, _gap: Gap) -> Result<(), Self::Error> {
            unreachable!()
        }

        fn persist_continuity_break(
            &mut self,
            _observed: ContinuityBreak,
        ) -> Result<(), Self::Error> {
            unreachable!()
        }
    }

    fn pending_transfer() -> MeasurementTransfer {
        let session = NodeSession::recovered(
            HARDWARE_ID,
            BootEpoch::new(9),
            SequenceCursor::ORIGIN,
            SequenceCursor::ORIGIN,
        )
        .unwrap();
        let mut transfer = MeasurementTransfer::new(7, 16, session).unwrap();
        transfer
            .begin_attempt(crate::transaction::DurationMicros::new(100).unwrap())
            .unwrap();
        let axis = SampleAxis::new(17).unwrap();
        let chunk = SampleChunk {
            header: SampleChunkHeader {
                boot_epoch: BootEpoch::new(9),
                retained_from: SequenceCursor::ORIGIN,
                produced: SequenceCursor::new(1),
                first: SequenceCursor::ORIGIN,
                sample_count: 1,
            },
            config_revision: 44,
            timing: TimingEvidence {
                segment_id: 3,
                first_device_time_us: 1234,
                last_device_time_us: 1234,
                max_fit_residual_us: 7,
                quality_flags: TIMING_VALID,
            },
            samples: vec![SampleAxes::new(axis, axis, axis)],
        };
        let mut payload = [0; MAX_PAYLOAD_BYTES];
        let len = encode_pull_response(&PullResponse::SampleChunk(chunk), &mut payload).unwrap();
        transfer.accept_response(&payload[..len]).unwrap();
        transfer
    }

    #[test]
    fn sink_failure_keeps_the_chunk_pending_and_does_not_release_it() {
        let mut transfer = pending_transfer();
        let before = transfer.session().cursors();
        let mut sink = Sink {
            fail: true,
            persisted_revision: None,
        };

        assert!(matches!(
            persist_pending(&mut transfer, &mut sink),
            Err(PersistenceError::Sink(SinkFailure))
        ));
        assert_eq!(transfer.session().phase(), SessionPhase::PersistingChunk);
        assert_eq!(transfer.session().cursors(), before);
    }

    #[test]
    fn successful_durable_write_advances_commit_before_next_release() {
        let mut transfer = pending_transfer();
        let mut sink = Sink {
            fail: false,
            persisted_revision: None,
        };

        let receipt = persist_pending(&mut transfer, &mut sink).unwrap();
        assert_eq!(receipt.end, SequenceCursor::new(1));
        assert_eq!(sink.persisted_revision, Some(44));
        let next = transfer
            .begin_attempt(crate::transaction::DurationMicros::new(100).unwrap())
            .unwrap();
        assert_eq!(
            u64::from_le_bytes(next.payload()[16..24].try_into().unwrap()),
            1
        );
    }
}
