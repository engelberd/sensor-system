use core::fmt;

use sensor_core::{BootEpoch, HardwareId, SequenceCursor};
use sensor_protocol::{MeasurementContractError, StreamState};

use crate::discovery::DiscoveredNode;
use crate::node_session::{NodeSession, SessionError};

/// Immutable facts established by the fail-closed discovery handshake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpectedStreamState {
    pub hardware_id: HardwareId,
    pub boot_epoch: BootEpoch,
    pub journal_capacity_samples: u32,
}

impl From<&DiscoveredNode> for ExpectedStreamState {
    fn from(node: &DiscoveredNode) -> Self {
        Self {
            hardware_id: node.identity().hardware_id,
            boot_epoch: node.identity().boot_epoch,
            journal_capacity_samples: node.capabilities().journal_capacity_samples,
        }
    }
}

/// Durable Capture evidence recovered from disk before any node data is freed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureRecoveryEvidence {
    pub hardware_id: HardwareId,
    pub boot_epoch: BootEpoch,
    pub durable_cursor: SequenceCursor,
}

/// Boundary selected from an authoritative pre-Start node read. It is not
/// durable Capture evidence until storage has created and recovered a stream
/// whose first cursor is `cursor`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NewCaptureBoundary {
    pub hardware_id: HardwareId,
    pub boot_epoch: BootEpoch,
    pub cursor: SequenceCursor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryContinuity {
    Contiguous,
    /// The node has already overwritten this range. The normal first pull will
    /// turn it into durable `JournalOverflow` gap evidence before progressing.
    JournalGapRequired {
        first_missing: SequenceCursor,
        missing_count: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconciledStream {
    session: NodeSession,
    continuity: RecoveryContinuity,
}

impl ReconciledStream {
    pub fn into_session(self) -> NodeSession {
        self.session
    }

    pub const fn session(&self) -> &NodeSession {
        &self.session
    }

    pub const fn continuity(&self) -> RecoveryContinuity {
        self.continuity
    }
}

/// Reconciles the node's authoritative journal state with data proven durable
/// by Capture. This function never advances a host cursor and never forgives a
/// node release beyond durable storage.
pub fn reconcile_existing_capture(
    expected: ExpectedStreamState,
    capture: CaptureRecoveryEvidence,
    node: StreamState,
) -> Result<ReconciledStream, StreamRecoveryError> {
    let node = node
        .validate()
        .map_err(StreamRecoveryError::InvalidNodeState)?;
    validate_capture_identity(expected, capture)?;
    validate_node_identity(expected, node)?;

    if node.released > capture.durable_cursor {
        return Err(StreamRecoveryError::ReleasedBeyondDurable {
            released: node.released,
            durable: capture.durable_cursor,
        });
    }
    if capture.durable_cursor > node.produced {
        return Err(StreamRecoveryError::CaptureAheadOfNode {
            durable: capture.durable_cursor,
            produced: node.produced,
        });
    }

    let continuity = if capture.durable_cursor < node.retained_from {
        RecoveryContinuity::JournalGapRequired {
            first_missing: capture.durable_cursor,
            missing_count: node.retained_from.get() - capture.durable_cursor.get(),
        }
    } else {
        RecoveryContinuity::Contiguous
    };
    let session = NodeSession::recovered(
        expected.hardware_id,
        expected.boot_epoch,
        capture.durable_cursor,
        node.released,
    )
    .map_err(StreamRecoveryError::Session)?;
    Ok(ReconciledStream {
        session,
        continuity,
    })
}

/// Defines a new recording's explicit pre-Start boundary. Samples before this
/// cursor are outside the recording rather than an inferred loss. The caller
/// must create and recover the Capture WAL at this cursor before issuing Start.
pub fn new_capture_boundary(
    expected: ExpectedStreamState,
    node: StreamState,
) -> Result<NewCaptureBoundary, StreamRecoveryError> {
    let node = node
        .validate()
        .map_err(StreamRecoveryError::InvalidNodeState)?;
    validate_node_identity(expected, node)?;
    Ok(NewCaptureBoundary {
        hardware_id: expected.hardware_id,
        boot_epoch: expected.boot_epoch,
        cursor: node.produced,
    })
}

fn validate_capture_identity(
    expected: ExpectedStreamState,
    capture: CaptureRecoveryEvidence,
) -> Result<(), StreamRecoveryError> {
    if capture.hardware_id != expected.hardware_id {
        return Err(StreamRecoveryError::CaptureHardwareMismatch);
    }
    if capture.boot_epoch != expected.boot_epoch {
        return Err(StreamRecoveryError::CaptureBootEpochMismatch {
            expected: expected.boot_epoch,
            actual: capture.boot_epoch,
        });
    }
    Ok(())
}

fn validate_node_identity(
    expected: ExpectedStreamState,
    node: StreamState,
) -> Result<(), StreamRecoveryError> {
    if node.boot_epoch != expected.boot_epoch {
        return Err(StreamRecoveryError::NodeBootEpochMismatch {
            expected: expected.boot_epoch,
            actual: node.boot_epoch,
        });
    }
    if node.capacity_samples != expected.journal_capacity_samples {
        return Err(StreamRecoveryError::JournalCapacityMismatch {
            expected: expected.journal_capacity_samples,
            actual: node.capacity_samples,
        });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamRecoveryError {
    InvalidNodeState(MeasurementContractError),
    CaptureHardwareMismatch,
    CaptureBootEpochMismatch {
        expected: BootEpoch,
        actual: BootEpoch,
    },
    NodeBootEpochMismatch {
        expected: BootEpoch,
        actual: BootEpoch,
    },
    JournalCapacityMismatch {
        expected: u32,
        actual: u32,
    },
    ReleasedBeyondDurable {
        released: SequenceCursor,
        durable: SequenceCursor,
    },
    CaptureAheadOfNode {
        durable: SequenceCursor,
        produced: SequenceCursor,
    },
    Session(SessionError),
}

impl fmt::Display for StreamRecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "stream recovery error: {self:?}")
    }
}

impl std::error::Error for StreamRecoveryError {}

#[cfg(test)]
mod tests {
    use super::*;

    const HARDWARE_ID: HardwareId = HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);

    fn expected() -> ExpectedStreamState {
        ExpectedStreamState {
            hardware_id: HARDWARE_ID,
            boot_epoch: BootEpoch::new(9),
            journal_capacity_samples: 2048,
        }
    }

    fn capture(durable: u64) -> CaptureRecoveryEvidence {
        CaptureRecoveryEvidence {
            hardware_id: HARDWARE_ID,
            boot_epoch: BootEpoch::new(9),
            durable_cursor: SequenceCursor::new(durable),
        }
    }

    fn state(retained: u64, produced: u64, released: u64) -> StreamState {
        StreamState {
            boot_epoch: BootEpoch::new(9),
            retained_from: SequenceCursor::new(retained),
            produced: SequenceCursor::new(produced),
            released: SequenceCursor::new(released),
            capacity_samples: 2048,
            overwritten_samples: retained,
        }
    }

    #[test]
    fn resumes_only_from_the_capture_durable_cursor() {
        let reconciled =
            reconcile_existing_capture(expected(), capture(100), state(80, 140, 90)).unwrap();
        assert_eq!(reconciled.continuity(), RecoveryContinuity::Contiguous);
        assert_eq!(
            reconciled.session().cursors().received(),
            SequenceCursor::new(100)
        );
        assert_eq!(
            reconciled.session().cursors().released(),
            SequenceCursor::new(90)
        );
    }

    #[test]
    fn reports_the_exact_overwritten_prefix_without_advancing_the_session() {
        let reconciled =
            reconcile_existing_capture(expected(), capture(100), state(125, 140, 90)).unwrap();
        assert_eq!(
            reconciled.continuity(),
            RecoveryContinuity::JournalGapRequired {
                first_missing: SequenceCursor::new(100),
                missing_count: 25,
            }
        );
        assert_eq!(
            reconciled.session().cursors().received(),
            SequenceCursor::new(100)
        );
    }

    #[test]
    fn refuses_data_released_beyond_durable_capture() {
        assert_eq!(
            reconcile_existing_capture(expected(), capture(100), state(80, 140, 101)),
            Err(StreamRecoveryError::ReleasedBeyondDurable {
                released: SequenceCursor::new(101),
                durable: SequenceCursor::new(100),
            })
        );
    }

    #[test]
    fn refuses_capture_ahead_of_the_nodes_stream() {
        assert_eq!(
            reconcile_existing_capture(expected(), capture(141), state(80, 140, 90)),
            Err(StreamRecoveryError::CaptureAheadOfNode {
                durable: SequenceCursor::new(141),
                produced: SequenceCursor::new(140),
            })
        );
    }

    #[test]
    fn refuses_epoch_or_capacity_drift() {
        let mut changed_epoch = state(80, 140, 90);
        changed_epoch.boot_epoch = BootEpoch::new(10);
        assert!(matches!(
            reconcile_existing_capture(expected(), capture(100), changed_epoch),
            Err(StreamRecoveryError::NodeBootEpochMismatch { .. })
        ));

        let mut changed_capacity = state(80, 140, 90);
        changed_capacity.capacity_samples = 4096;
        assert_eq!(
            reconcile_existing_capture(expected(), capture(100), changed_capacity),
            Err(StreamRecoveryError::JournalCapacityMismatch {
                expected: 2048,
                actual: 4096,
            })
        );
    }

    #[test]
    fn new_recording_boundary_is_the_real_pre_start_produced_cursor() {
        assert_eq!(
            new_capture_boundary(expected(), state(80, 140, 90)).unwrap(),
            NewCaptureBoundary {
                hardware_id: HARDWARE_ID,
                boot_epoch: BootEpoch::new(9),
                cursor: SequenceCursor::new(140),
            }
        );
    }
}
