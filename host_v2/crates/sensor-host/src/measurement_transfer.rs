use core::fmt;

use sensor_core::{BootEpoch, ContinuityBreak, Gap, SequenceCursor};
use sensor_protocol::{
    decode_pull_response, encode_pull_request, MeasurementWireError, PullResponse, ResponseStatus,
    ServiceId, StreamState, PULL_REQUEST_BYTES, PULL_SAMPLES_MESSAGE_ID,
};

use crate::node_session::{DurableReceipt, NodeSession, PendingPersistence, SessionError};
use crate::transaction::{DurationMicros, RequestSpec, TimeoutRecovery};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistenceKind {
    SampleChunk { sample_count: u16 },
    Gap(Gap),
    ContinuityBreak(ContinuityBreak),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryReason {
    StreamChanged(BootEpoch),
    InvalidRelease(StreamState),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeasurementOutcome {
    NotAccepted,
    NoData(StreamState),
    PersistenceRequired(PersistenceKind),
    RecoveryRequired(RecoveryReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeasurementAttempt {
    pub request: RequestSpec,
    payload: [u8; PULL_REQUEST_BYTES],
}

impl MeasurementAttempt {
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

/// Owns the delivery state for one node while it moves through the channel
/// scheduler, response validation and durable persistence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeasurementTransfer {
    node_address: u8,
    max_samples: u16,
    session: NodeSession,
    observed_produced: Option<SequenceCursor>,
    persistence_kind: Option<PersistenceKind>,
}

impl MeasurementTransfer {
    pub fn new(
        node_address: u8,
        max_samples: u16,
        session: NodeSession,
    ) -> Result<Self, MeasurementTransferError> {
        if max_samples == 0 {
            return Err(MeasurementTransferError::ZeroSampleLimit);
        }
        Ok(Self {
            node_address,
            max_samples,
            session,
            observed_produced: None,
            persistence_kind: None,
        })
    }

    pub const fn node_address(&self) -> u8 {
        self.node_address
    }

    pub const fn max_samples(&self) -> u16 {
        self.max_samples
    }

    pub const fn session(&self) -> &NodeSession {
        &self.session
    }

    pub const fn observed_produced(&self) -> Option<SequenceCursor> {
        self.observed_produced
    }

    pub const fn persistence_kind(&self) -> Option<PersistenceKind> {
        self.persistence_kind
    }

    pub fn take_pending_persistence(
        &mut self,
    ) -> Result<PendingPersistence, MeasurementTransferError> {
        self.session
            .take_pending_persistence()
            .map_err(MeasurementTransferError::Session)
    }

    pub fn into_session(self) -> NodeSession {
        self.session
    }

    pub fn begin_attempt(
        &mut self,
        timeout: DurationMicros,
    ) -> Result<MeasurementAttempt, MeasurementTransferError> {
        let pull = self.session.begin_pull(self.max_samples)?;
        Ok(MeasurementAttempt {
            request: RequestSpec {
                node_address: self.node_address,
                service: ServiceId::Measurement as u8,
                request_message: PULL_SAMPLES_MESSAGE_ID,
                response_message: PULL_SAMPLES_MESSAGE_ID,
                timeout,
                timeout_recovery: TimeoutRecovery::RetrySafe,
            },
            payload: encode_pull_request(pull),
        })
    }

    pub fn mark_attempt_uncertain(&mut self) -> Result<(), MeasurementTransferError> {
        self.session.abandon_pull_for_retry()?;
        Ok(())
    }

    pub fn accept_response(
        &mut self,
        payload: &[u8],
    ) -> Result<MeasurementOutcome, MeasurementTransferError> {
        let response = match decode_pull_response(payload) {
            Ok(response) => response,
            Err(MeasurementWireError::RemoteError(ResponseStatus::Busy)) => {
                self.session.abandon_pull_for_retry()?;
                return Ok(MeasurementOutcome::NotAccepted);
            }
            Err(error) => {
                self.session.fail_pull_for_recovery()?;
                return Err(error.into());
            }
        };

        match response {
            PullResponse::SampleChunk(chunk) => {
                let produced = chunk.header.produced;
                let sample_count = chunk.header.sample_count;
                self.session.accept_wire_chunk(chunk)?;
                self.observed_produced = Some(produced);
                self.persistence_kind = Some(PersistenceKind::SampleChunk { sample_count });
                Ok(MeasurementOutcome::PersistenceRequired(
                    PersistenceKind::SampleChunk { sample_count },
                ))
            }
            PullResponse::NoData(state) => {
                self.session.accept_no_data(state)?;
                self.observed_produced = Some(state.produced);
                Ok(MeasurementOutcome::NoData(state))
            }
            PullResponse::RangeUnavailable(state) => {
                let gap = self.session.accept_range_unavailable(state)?;
                self.observed_produced = Some(state.produced);
                self.persistence_kind = Some(PersistenceKind::Gap(gap));
                Ok(MeasurementOutcome::PersistenceRequired(
                    PersistenceKind::Gap(gap),
                ))
            }
            PullResponse::Gap(evidence) => {
                let gap = self.session.accept_gap(evidence)?;
                self.observed_produced = Some(evidence.produced);
                self.persistence_kind = Some(PersistenceKind::Gap(gap));
                Ok(MeasurementOutcome::PersistenceRequired(
                    PersistenceKind::Gap(gap),
                ))
            }
            PullResponse::ContinuityBreak(evidence) => {
                let observed = self.session.accept_continuity_break(evidence)?;
                self.observed_produced = Some(evidence.produced);
                self.persistence_kind = Some(PersistenceKind::ContinuityBreak(observed));
                Ok(MeasurementOutcome::PersistenceRequired(
                    PersistenceKind::ContinuityBreak(observed),
                ))
            }
            PullResponse::StreamChanged(epoch) => {
                self.session.stream_changed()?;
                Ok(MeasurementOutcome::RecoveryRequired(
                    RecoveryReason::StreamChanged(epoch),
                ))
            }
            PullResponse::InvalidRelease(state) => {
                self.session.invalid_release(state)?;
                self.observed_produced = Some(state.produced);
                Ok(MeasurementOutcome::RecoveryRequired(
                    RecoveryReason::InvalidRelease(state),
                ))
            }
        }
    }

    pub fn confirm_durable(
        &mut self,
        receipt: DurableReceipt,
    ) -> Result<(), MeasurementTransferError> {
        self.session.confirm_durable(receipt)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeasurementTransferError {
    ZeroSampleLimit,
    Session(SessionError),
    Wire(MeasurementWireError),
}

impl From<SessionError> for MeasurementTransferError {
    fn from(value: SessionError) -> Self {
        Self::Session(value)
    }
}

impl From<MeasurementWireError> for MeasurementTransferError {
    fn from(value: MeasurementWireError) -> Self {
        Self::Wire(value)
    }
}

impl fmt::Display for MeasurementTransferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroSampleLimit => formatter.write_str("measurement sample limit is zero"),
            Self::Session(error) => write!(formatter, "measurement session failed: {error}"),
            Self::Wire(error) => write!(formatter, "measurement response failed: {error}"),
        }
    }
}

impl std::error::Error for MeasurementTransferError {}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_core::{HardwareId, SampleAxes, SampleAxis, SequenceCursor};
    use sensor_protocol::{
        encode_pull_response, PullResponse, SampleChunk, SampleChunkHeader, TimingEvidence,
        MAX_PAYLOAD_BYTES, TIMING_VALID,
    };

    const HARDWARE_ID: HardwareId = HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);

    fn transfer() -> MeasurementTransfer {
        MeasurementTransfer::new(
            7,
            16,
            NodeSession::recovered(
                HARDWARE_ID,
                BootEpoch::new(9),
                SequenceCursor::ORIGIN,
                SequenceCursor::ORIGIN,
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn timeout_retries_the_exact_delivery_cursors() {
        let mut transfer = transfer();
        let first = transfer
            .begin_attempt(DurationMicros::new(100).unwrap())
            .unwrap();
        transfer.mark_attempt_uncertain().unwrap();
        let retry = transfer
            .begin_attempt(DurationMicros::new(100).unwrap())
            .unwrap();
        assert_eq!(first.payload(), retry.payload());
    }

    #[test]
    fn preserves_wire_timing_and_configuration_until_durable_commit() {
        let mut transfer = transfer();
        transfer
            .begin_attempt(DurationMicros::new(100).unwrap())
            .unwrap();
        let sample = SampleAxis::new(17).unwrap();
        let chunk = SampleChunk {
            header: SampleChunkHeader {
                boot_epoch: BootEpoch::new(9),
                retained_from: SequenceCursor::ORIGIN,
                produced: SequenceCursor::new(1),
                first: SequenceCursor::ORIGIN,
                sample_count: 1,
            },
            config_revision: 5,
            timing: TimingEvidence {
                segment_id: 3,
                first_device_time_us: 1234,
                last_device_time_us: 1234,
                max_fit_residual_us: 7,
                quality_flags: TIMING_VALID,
            },
            samples: vec![SampleAxes::new(sample, sample, sample)],
        };
        let mut payload = [0; MAX_PAYLOAD_BYTES];
        let len =
            encode_pull_response(&PullResponse::SampleChunk(chunk.clone()), &mut payload).unwrap();

        assert_eq!(
            transfer.accept_response(&payload[..len]).unwrap(),
            MeasurementOutcome::PersistenceRequired(PersistenceKind::SampleChunk {
                sample_count: 1
            })
        );
        assert_eq!(transfer.session().pending_wire_chunk(), Some(&chunk));
        assert_eq!(transfer.observed_produced(), Some(SequenceCursor::new(1)));
        transfer
            .confirm_durable(DurableReceipt {
                hardware_id: HARDWARE_ID,
                boot_epoch: BootEpoch::new(9),
                end: SequenceCursor::new(1),
            })
            .unwrap();
        assert!(transfer.session().pending_wire_chunk().is_none());
    }
}
