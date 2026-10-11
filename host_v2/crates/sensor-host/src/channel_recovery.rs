use core::fmt;

use sensor_core::{BootEpoch, HardwareId};
use sensor_protocol::RuntimeResult;

use crate::capture_profile::DiscoveredCaptureStream;
use crate::capture_runtime::RecoveredCaptureStream;
use crate::capture_wal::CaptureWalHeader;
use crate::channel_bootstrap::NodeBootstrapEvidence;
use crate::discovery::DiscoveryTargetId;
use crate::node_session::NodeSession;
use crate::stream_recovery::{
    new_capture_boundary, reconcile_existing_capture, CaptureRecoveryEvidence, ExpectedStreamState,
    RecoveryContinuity, StreamRecoveryError,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureStartupKind {
    Existing,
    New,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedCaptureStream {
    pub stream: DiscoveredCaptureStream,
    pub kind: CaptureStartupKind,
}

/// Builds only the immutable Capture origin. It deliberately cannot provide a
/// session cursor; that cursor is available only after `CaptureRuntime` has
/// replayed the WAL and returned `RecoveredCaptureStream`.
pub fn prepare_capture_stream(
    node: NodeBootstrapEvidence,
    origin: Option<CaptureWalHeader>,
) -> Result<PreparedCaptureStream, ChannelRecoveryError> {
    let expected = expected(node);
    match origin {
        Some(header) => {
            if header.hardware_id != expected.hardware_id {
                return Err(ChannelRecoveryError::OriginHardwareMismatch(node.id));
            }
            if header.boot_epoch != expected.boot_epoch {
                return Err(ChannelRecoveryError::OriginBootEpochMismatch {
                    id: node.id,
                    expected: expected.boot_epoch,
                    actual: header.boot_epoch,
                });
            }
            Ok(PreparedCaptureStream {
                stream: DiscoveredCaptureStream {
                    hardware_id: header.hardware_id,
                    boot_epoch: header.boot_epoch,
                    recovered_cursor: header.first_cursor,
                    first_window_ordinal: header.first_window_ordinal,
                },
                kind: CaptureStartupKind::Existing,
            })
        }
        None => {
            let boundary = new_capture_boundary(expected, node.stream)
                .map_err(ChannelRecoveryError::Stream)?;
            Ok(PreparedCaptureStream {
                stream: DiscoveredCaptureStream {
                    hardware_id: boundary.hardware_id,
                    boot_epoch: boundary.boot_epoch,
                    recovered_cursor: boundary.cursor,
                    first_window_ordinal: 0,
                },
                kind: CaptureStartupKind::New,
            })
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveredNode {
    id: DiscoveryTargetId,
    runtime: RuntimeResult,
    continuity: RecoveryContinuity,
    session: NodeSession,
}

impl RecoveredNode {
    pub const fn id(&self) -> DiscoveryTargetId {
        self.id
    }

    pub const fn runtime(&self) -> RuntimeResult {
        self.runtime
    }

    pub const fn continuity(&self) -> RecoveryContinuity {
        self.continuity
    }

    pub const fn session(&self) -> &NodeSession {
        &self.session
    }

    pub fn into_session(self) -> NodeSession {
        self.session
    }

    pub(crate) fn from_parts(
        id: DiscoveryTargetId,
        runtime: RuntimeResult,
        continuity: RecoveryContinuity,
        session: NodeSession,
    ) -> Self {
        Self {
            id,
            runtime,
            continuity,
            session,
        }
    }

    pub(crate) fn confirm_runtime(&mut self, runtime: RuntimeResult) {
        self.runtime = runtime;
    }
}

/// Joins bootstrapped node evidence with the authoritative durable snapshot
/// produced by Capture replay. Matching is by immutable `(hardware, epoch)`;
/// address ordering cannot attach one sensor's cursor to another sensor.
pub fn reconcile_channel_streams(
    nodes: &[NodeBootstrapEvidence],
    recovered: &[RecoveredCaptureStream],
) -> Result<Vec<RecoveredNode>, ChannelRecoveryError> {
    if nodes.len() != recovered.len() {
        return Err(ChannelRecoveryError::StreamCountMismatch {
            nodes: nodes.len(),
            recovered: recovered.len(),
        });
    }
    for (index, stream) in recovered.iter().enumerate() {
        if recovered[..index].iter().any(|candidate| {
            candidate.hardware_id == stream.hardware_id && candidate.boot_epoch == stream.boot_epoch
        }) {
            return Err(ChannelRecoveryError::DuplicateRecoveredStream {
                hardware_id: stream.hardware_id,
                boot_epoch: stream.boot_epoch,
            });
        }
    }

    let mut result = Vec::with_capacity(nodes.len());
    for node in nodes {
        let expected = expected(*node);
        let stream = recovered
            .iter()
            .find(|stream| {
                stream.hardware_id == expected.hardware_id
                    && stream.boot_epoch == expected.boot_epoch
            })
            .ok_or(ChannelRecoveryError::MissingRecoveredStream(node.id))?;
        let reconciled = reconcile_existing_capture(
            expected,
            CaptureRecoveryEvidence {
                hardware_id: stream.hardware_id,
                boot_epoch: stream.boot_epoch,
                durable_cursor: stream.durable,
            },
            node.stream,
        )
        .map_err(ChannelRecoveryError::Stream)?;
        result.push(RecoveredNode::from_parts(
            node.id,
            node.runtime,
            reconciled.continuity(),
            reconciled.into_session(),
        ));
    }
    Ok(result)
}

fn expected(node: NodeBootstrapEvidence) -> ExpectedStreamState {
    ExpectedStreamState {
        hardware_id: node.identity.hardware_id,
        boot_epoch: node.identity.boot_epoch,
        journal_capacity_samples: node.capabilities.journal_capacity_samples,
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum ChannelRecoveryError {
    OriginHardwareMismatch(DiscoveryTargetId),
    OriginBootEpochMismatch {
        id: DiscoveryTargetId,
        expected: BootEpoch,
        actual: BootEpoch,
    },
    StreamCountMismatch {
        nodes: usize,
        recovered: usize,
    },
    DuplicateRecoveredStream {
        hardware_id: HardwareId,
        boot_epoch: BootEpoch,
    },
    MissingRecoveredStream(DiscoveryTargetId),
    Stream(StreamRecoveryError),
}

impl fmt::Display for ChannelRecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "channel recovery error: {self:?}")
    }
}

impl std::error::Error for ChannelRecoveryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_core::SequenceCursor;
    use sensor_protocol::{
        FirmwareVersion, NodeCapabilities, NodeIdentity, ObservabilityProfile, RuntimeResultCode,
        RuntimeState, SensorStatusCode, StreamState, CAPABILITIES_SCHEMA_VERSION,
        LOCAL_PROTOCOL_VERSION, MAX_PAYLOAD_BYTES,
    };

    use crate::capture_window::CaptureWindowPolicy;

    fn hardware(address: u8) -> HardwareId {
        HardwareId::from_bytes([address, 1, 2, 3, 4, 5, 6, 7])
    }

    fn node(address: u8, retained: u64, produced: u64, released: u64) -> NodeBootstrapEvidence {
        let epoch = BootEpoch::new(100 + u64::from(address));
        NodeBootstrapEvidence {
            id: DiscoveryTargetId {
                channel_id: 1,
                node_address: address,
            },
            identity: NodeIdentity {
                hardware_id: hardware(address),
                boot_epoch: epoch,
                node_address: address,
                board_revision: 2,
                firmware: FirmwareVersion {
                    major: 0,
                    minor: 5,
                    patch: 6,
                },
                protocol: LOCAL_PROTOCOL_VERSION,
            },
            capabilities: NodeCapabilities {
                schema_version: CAPABILITIES_SCHEMA_VERSION,
                service_flags: 0x0f,
                feature_flags: 0,
                max_payload_bytes: MAX_PAYLOAD_BYTES as u16,
                journal_capacity_samples: 2048,
                output_rate_mask: 1,
                max_sample_chunk_samples: 48,
            },
            runtime: RuntimeResult {
                operation_id: 7,
                code: RuntimeResultCode::Ok,
                state: RuntimeState::Recording,
                observability: ObservabilityProfile::Production,
                sensor_status: SensorStatusCode::Ok,
            },
            stream: StreamState {
                boot_epoch: epoch,
                retained_from: SequenceCursor::new(retained),
                produced: SequenceCursor::new(produced),
                released: SequenceCursor::new(released),
                capacity_samples: 2048,
                overwritten_samples: retained,
            },
        }
    }

    fn header(address: u8, first: u64, ordinal: u64) -> CaptureWalHeader {
        CaptureWalHeader {
            hardware_id: hardware(address),
            boot_epoch: BootEpoch::new(100 + u64::from(address)),
            first_cursor: SequenceCursor::new(first),
            first_window_ordinal: ordinal,
            window_policy: CaptureWindowPolicy::new(600_000_000).unwrap(),
        }
    }

    #[test]
    fn new_stream_uses_observed_produced_as_origin_but_not_as_a_session() {
        let prepared = prepare_capture_stream(node(1, 80, 140, 90), None).unwrap();
        assert_eq!(prepared.kind, CaptureStartupKind::New);
        assert_eq!(prepared.stream.recovered_cursor, SequenceCursor::new(140));
        assert_eq!(prepared.stream.first_window_ordinal, 0);
    }

    #[test]
    fn existing_stream_uses_only_its_immutable_header_origin() {
        let prepared =
            prepare_capture_stream(node(1, 80, 140, 90), Some(header(1, 20, 7))).unwrap();
        assert_eq!(prepared.kind, CaptureStartupKind::Existing);
        assert_eq!(prepared.stream.recovered_cursor, SequenceCursor::new(20));
        assert_eq!(prepared.stream.first_window_ordinal, 7);
    }

    #[test]
    fn durable_snapshot_is_matched_by_identity_and_preserves_exact_gap() {
        let nodes = [node(1, 125, 140, 90), node(2, 50, 120, 70)];
        let recovered = [
            RecoveredCaptureStream {
                hardware_id: hardware(2),
                boot_epoch: BootEpoch::new(102),
                durable: SequenceCursor::new(100),
            },
            RecoveredCaptureStream {
                hardware_id: hardware(1),
                boot_epoch: BootEpoch::new(101),
                durable: SequenceCursor::new(100),
            },
        ];
        let sessions = reconcile_channel_streams(&nodes, &recovered).unwrap();
        assert_eq!(sessions[0].id().node_address, 1);
        assert_eq!(
            sessions[0].continuity(),
            RecoveryContinuity::JournalGapRequired {
                first_missing: SequenceCursor::new(100),
                missing_count: 25,
            }
        );
        assert_eq!(
            sessions[1].session().cursors().received(),
            SequenceCursor::new(100)
        );
    }

    #[test]
    fn released_ahead_of_the_replayed_wal_fails_closed() {
        let error = reconcile_channel_streams(
            &[node(1, 80, 140, 101)],
            &[RecoveredCaptureStream {
                hardware_id: hardware(1),
                boot_epoch: BootEpoch::new(101),
                durable: SequenceCursor::new(100),
            }],
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ChannelRecoveryError::Stream(StreamRecoveryError::ReleasedBeyondDurable { .. })
        ));
    }
}
