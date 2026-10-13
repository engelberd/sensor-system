use core::fmt;
use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};

use sensor_core::HardwareId;
use sensor_protocol::MeasurementConfiguration;

use crate::installation_topology::MAX_HOST_CHANNELS;

/// Stable identity supplied by an Operations API client. A retry must preserve
/// both this value and the complete request.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OperationId(u64);

impl OperationId {
    pub fn new(value: u64) -> Result<Self, OperationsError> {
        if value == 0 {
            Err(OperationsError::ZeroOperationId)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationTarget {
    Channel { channel_id: u8 },
    Node { channel_id: u8, node_address: u8 },
}

impl OperationTarget {
    pub const fn channel_id(self) -> u8 {
        match self {
            Self::Channel { channel_id } | Self::Node { channel_id, .. } => channel_id,
        }
    }

    pub fn validate(self) -> Result<Self, OperationsError> {
        let channel_id = self.channel_id();
        if channel_id == 0 || usize::from(channel_id) > MAX_HOST_CHANNELS {
            return Err(OperationsError::InvalidChannelId(channel_id));
        }
        if let Self::Node { node_address, .. } = self {
            if !(1..=253).contains(&node_address) {
                return Err(OperationsError::InvalidNodeAddress(node_address));
            }
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelOperatingMode {
    Recording,
    EnteringMaintenance,
    Maintenance,
    ResumingRecording,
    Faulted,
    Offline,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OperationKind {
    ReadStatus,
    EnterMaintenance,
    ResumeRecording,
    GetConfiguration,
    ValidateConfiguration {
        configuration: MeasurementConfiguration,
    },
    ApplyConfiguration {
        expected_revision: u32,
        configuration: MeasurementConfiguration,
    },
    ResetConfiguration {
        expected_revision: u32,
    },
    ScanUnassigned {
        round_nonce: u64,
        slot_count: u16,
    },
    AssignAddress {
        round_nonce: u64,
        hardware_id: HardwareId,
        node_address: u8,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationRequest {
    pub operation_id: OperationId,
    pub target: OperationTarget,
    pub kind: OperationKind,
}

impl OperationRequest {
    pub fn validate(&self) -> Result<(), OperationsError> {
        self.target.validate()?;
        match &self.kind {
            OperationKind::ValidateConfiguration { configuration } => {
                configuration
                    .validate()
                    .map_err(|_| OperationsError::InvalidConfiguration)?;
            }
            OperationKind::ApplyConfiguration {
                expected_revision,
                configuration,
            } => {
                configuration
                    .validate()
                    .map_err(|_| OperationsError::InvalidConfiguration)?;
                if *expected_revision == 0 {
                    return Err(OperationsError::ZeroExpectedRevision);
                }
            }
            OperationKind::ResetConfiguration { expected_revision } => {
                if *expected_revision == 0 {
                    return Err(OperationsError::ZeroExpectedRevision);
                }
            }
            OperationKind::ScanUnassigned {
                round_nonce,
                slot_count,
            } => {
                if *round_nonce == 0 {
                    return Err(OperationsError::ZeroRoundNonce);
                }
                if !(1..=256).contains(slot_count) || !slot_count.is_power_of_two() {
                    return Err(OperationsError::InvalidSlotCount(*slot_count));
                }
            }
            OperationKind::AssignAddress {
                round_nonce,
                node_address,
                ..
            } => {
                if *round_nonce == 0 {
                    return Err(OperationsError::ZeroRoundNonce);
                }
                if !(1..=253).contains(node_address) {
                    return Err(OperationsError::InvalidNodeAddress(*node_address));
                }
            }
            OperationKind::ReadStatus
            | OperationKind::EnterMaintenance
            | OperationKind::ResumeRecording
            | OperationKind::GetConfiguration => {}
        }
        validate_scope(self.target, &self.kind)
    }
}

/// Admission is intentionally independent from wire and IPC. Every adapter
/// (hostctl, dashboard, future remote gateway) receives the same answer.
pub fn admit_operation(
    request: &OperationRequest,
    mode: ChannelOperatingMode,
) -> Result<(), OperationsError> {
    request.validate()?;
    match request.kind {
        OperationKind::ReadStatus => Ok(()),
        OperationKind::EnterMaintenance if mode == ChannelOperatingMode::Recording => Ok(()),
        OperationKind::ResumeRecording if mode == ChannelOperatingMode::Maintenance => Ok(()),
        OperationKind::GetConfiguration
        | OperationKind::ValidateConfiguration { .. }
        | OperationKind::ApplyConfiguration { .. }
        | OperationKind::ResetConfiguration { .. }
        | OperationKind::ScanUnassigned { .. }
        | OperationKind::AssignAddress { .. }
            if mode == ChannelOperatingMode::Maintenance =>
        {
            Ok(())
        }
        _ => Err(OperationsError::WrongMode { mode }),
    }
}

fn validate_scope(target: OperationTarget, kind: &OperationKind) -> Result<(), OperationsError> {
    let valid = matches!(
        (target, kind),
        (_, OperationKind::ReadStatus)
            | (
                OperationTarget::Channel { .. },
                OperationKind::EnterMaintenance
            )
            | (
                OperationTarget::Channel { .. },
                OperationKind::ResumeRecording
            )
            | (
                OperationTarget::Node { .. },
                OperationKind::GetConfiguration
            )
            | (
                OperationTarget::Node { .. },
                OperationKind::ValidateConfiguration { .. }
            )
            | (
                OperationTarget::Node { .. },
                OperationKind::ApplyConfiguration { .. }
            )
            | (
                OperationTarget::Node { .. },
                OperationKind::ResetConfiguration { .. }
            )
            | (
                OperationTarget::Channel { .. },
                OperationKind::ScanUnassigned { .. }
            )
            | (
                OperationTarget::Channel { .. },
                OperationKind::AssignAddress { .. }
            )
    );
    if valid {
        Ok(())
    } else {
        Err(OperationsError::WrongTargetScope)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationPhase {
    Accepted,
    Running,
    Succeeded,
    Rejected,
    Failed,
}

impl OperationPhase {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Rejected | Self::Failed)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationRecord {
    pub request: OperationRequest,
    pub phase: OperationPhase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegisterOutcome {
    Accepted,
    Existing(OperationPhase),
}

/// Bounded in-memory idempotency ledger. Persistence/audit is a separate port;
/// this type only prevents duplicate execution inside a live host process.
pub struct OperationLedger {
    capacity: usize,
    records: VecDeque<OperationRecord>,
}

impl OperationLedger {
    pub fn new(capacity: usize) -> Result<Self, OperationsError> {
        if capacity == 0 {
            return Err(OperationsError::ZeroCapacity);
        }
        Ok(Self {
            capacity,
            records: VecDeque::with_capacity(capacity),
        })
    }

    pub fn register(
        &mut self,
        request: OperationRequest,
    ) -> Result<RegisterOutcome, OperationsError> {
        request.validate()?;
        if let Some(record) = self
            .records
            .iter()
            .find(|record| record.request.operation_id == request.operation_id)
        {
            return if record.request == request {
                Ok(RegisterOutcome::Existing(record.phase))
            } else {
                Err(OperationsError::OperationIdConflict(request.operation_id))
            };
        }
        if self.records.len() == self.capacity {
            let Some(index) = self
                .records
                .iter()
                .position(|record| record.phase.is_terminal())
            else {
                return Err(OperationsError::LedgerFull);
            };
            self.records.remove(index);
        }
        self.records.push_back(OperationRecord {
            request,
            phase: OperationPhase::Accepted,
        });
        Ok(RegisterOutcome::Accepted)
    }

    pub fn set_phase(
        &mut self,
        operation_id: OperationId,
        next: OperationPhase,
    ) -> Result<(), OperationsError> {
        let record = self
            .records
            .iter_mut()
            .find(|record| record.request.operation_id == operation_id)
            .ok_or(OperationsError::UnknownOperation(operation_id))?;
        if !valid_phase_transition(record.phase, next) {
            return Err(OperationsError::InvalidPhaseTransition {
                from: record.phase,
                to: next,
            });
        }
        record.phase = next;
        Ok(())
    }

    pub fn get(&self, operation_id: OperationId) -> Option<&OperationRecord> {
        self.records
            .iter()
            .find(|record| record.request.operation_id == operation_id)
    }
}

fn valid_phase_transition(from: OperationPhase, to: OperationPhase) -> bool {
    matches!(
        (from, to),
        (OperationPhase::Accepted, OperationPhase::Running)
            | (OperationPhase::Accepted, OperationPhase::Rejected)
            | (OperationPhase::Accepted, OperationPhase::Failed)
            | (OperationPhase::Running, OperationPhase::Succeeded)
            | (OperationPhase::Running, OperationPhase::Failed)
    )
}

#[derive(Clone)]
pub struct ChannelCommandPort {
    sender: SyncSender<OperationRequest>,
}

pub struct ChannelCommandInbox {
    receiver: Receiver<OperationRequest>,
}

pub fn channel_command_mailbox(
    capacity: usize,
) -> Result<(ChannelCommandPort, ChannelCommandInbox), OperationsError> {
    if capacity == 0 {
        return Err(OperationsError::ZeroCapacity);
    }
    let (sender, receiver) = mpsc::sync_channel(capacity);
    Ok((
        ChannelCommandPort { sender },
        ChannelCommandInbox { receiver },
    ))
}

impl ChannelCommandPort {
    pub fn try_submit(&self, request: OperationRequest) -> Result<(), CommandSubmitError> {
        request.validate().map_err(CommandSubmitError::Invalid)?;
        self.sender.try_send(request).map_err(|error| match error {
            TrySendError::Full(_) => CommandSubmitError::QueueFull,
            TrySendError::Disconnected(_) => CommandSubmitError::Disconnected,
        })
    }
}

impl ChannelCommandInbox {
    /// A worker calls this at most once per bounded loop iteration.
    pub fn try_receive(&self) -> Result<Option<OperationRequest>, OperationsError> {
        match self.receiver.try_recv() {
            Ok(request) => Ok(Some(request)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(OperationsError::CommandPathDisconnected),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandSubmitError {
    Invalid(OperationsError),
    QueueFull,
    Disconnected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationsError {
    ZeroOperationId,
    InvalidChannelId(u8),
    InvalidNodeAddress(u8),
    InvalidConfiguration,
    ZeroExpectedRevision,
    ZeroRoundNonce,
    InvalidSlotCount(u16),
    WrongTargetScope,
    WrongMode {
        mode: ChannelOperatingMode,
    },
    ZeroCapacity,
    OperationIdConflict(OperationId),
    LedgerFull,
    UnknownOperation(OperationId),
    InvalidPhaseTransition {
        from: OperationPhase,
        to: OperationPhase,
    },
    CommandPathDisconnected,
}

impl fmt::Display for OperationsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroOperationId => formatter.write_str("operation ID must be non-zero"),
            Self::InvalidChannelId(value) => write!(formatter, "invalid channel ID {value}"),
            Self::InvalidNodeAddress(value) => write!(formatter, "invalid node address {value}"),
            Self::InvalidConfiguration => formatter.write_str("invalid measurement configuration"),
            Self::ZeroExpectedRevision => formatter.write_str("expected revision must be non-zero"),
            Self::ZeroRoundNonce => formatter.write_str("commissioning nonce must be non-zero"),
            Self::InvalidSlotCount(value) => write!(formatter, "invalid slot count {value}"),
            Self::WrongTargetScope => formatter.write_str("operation has the wrong target scope"),
            Self::WrongMode { mode } => write!(formatter, "operation is not allowed in {mode:?}"),
            Self::ZeroCapacity => formatter.write_str("operation capacity must be non-zero"),
            Self::OperationIdConflict(value) => {
                write!(formatter, "operation ID {} was reused", value.get())
            }
            Self::LedgerFull => {
                formatter.write_str("operation ledger has no terminal entry to evict")
            }
            Self::UnknownOperation(value) => write!(formatter, "unknown operation {}", value.get()),
            Self::InvalidPhaseTransition { from, to } => {
                write!(formatter, "invalid operation transition {from:?} -> {to:?}")
            }
            Self::CommandPathDisconnected => {
                formatter.write_str("channel command path disconnected")
            }
        }
    }
}

impl std::error::Error for OperationsError {}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_protocol::{DecimationFilterProfile, OutputRateCode};

    fn id(value: u64) -> OperationId {
        OperationId::new(value).unwrap()
    }

    fn configuration() -> MeasurementConfiguration {
        MeasurementConfiguration {
            output_rate: OutputRateCode::Hz250,
            range_g: 2,
            high_pass_corner: 0,
            filter_profile: DecimationFilterProfile::Balanced,
            fifo_watermark: 30,
            offset_x: 0,
            offset_y: 0,
            offset_z: 0,
        }
    }

    fn get_configuration(operation_id: u64) -> OperationRequest {
        OperationRequest {
            operation_id: id(operation_id),
            target: OperationTarget::Node {
                channel_id: 2,
                node_address: 7,
            },
            kind: OperationKind::GetConfiguration,
        }
    }

    #[test]
    fn configuration_requires_node_target_and_maintenance() {
        let request = get_configuration(1);
        assert_eq!(
            admit_operation(&request, ChannelOperatingMode::Recording),
            Err(OperationsError::WrongMode {
                mode: ChannelOperatingMode::Recording
            })
        );
        assert_eq!(
            admit_operation(&request, ChannelOperatingMode::Maintenance),
            Ok(())
        );

        let mut wrong_scope = request;
        wrong_scope.target = OperationTarget::Channel { channel_id: 2 };
        assert_eq!(
            wrong_scope.validate(),
            Err(OperationsError::WrongTargetScope)
        );
    }

    #[test]
    fn mutating_configuration_requires_revision() {
        let request = OperationRequest {
            operation_id: id(2),
            target: OperationTarget::Node {
                channel_id: 1,
                node_address: 1,
            },
            kind: OperationKind::ApplyConfiguration {
                expected_revision: 0,
                configuration: configuration(),
            },
        };
        assert_eq!(
            request.validate(),
            Err(OperationsError::ZeroExpectedRevision)
        );
    }

    #[test]
    fn commissioning_is_channel_scoped_and_slot_count_is_bounded_power_of_two() {
        let request = OperationRequest {
            operation_id: id(3),
            target: OperationTarget::Channel { channel_id: 1 },
            kind: OperationKind::ScanUnassigned {
                round_nonce: 9,
                slot_count: 64,
            },
        };
        assert_eq!(
            admit_operation(&request, ChannelOperatingMode::Maintenance),
            Ok(())
        );
        let mut invalid = request;
        invalid.kind = OperationKind::ScanUnassigned {
            round_nonce: 9,
            slot_count: 3,
        };
        assert_eq!(
            invalid.validate(),
            Err(OperationsError::InvalidSlotCount(3))
        );
    }

    #[test]
    fn ledger_is_idempotent_and_rejects_operation_id_reuse() {
        let mut ledger = OperationLedger::new(2).unwrap();
        let request = get_configuration(4);
        assert_eq!(
            ledger.register(request.clone()),
            Ok(RegisterOutcome::Accepted)
        );
        assert_eq!(
            ledger.register(request),
            Ok(RegisterOutcome::Existing(OperationPhase::Accepted))
        );

        let conflict = OperationRequest {
            operation_id: id(4),
            target: OperationTarget::Channel { channel_id: 2 },
            kind: OperationKind::ReadStatus,
        };
        assert_eq!(
            ledger.register(conflict),
            Err(OperationsError::OperationIdConflict(id(4)))
        );
    }

    #[test]
    fn ledger_never_evicts_an_active_operation() {
        let mut ledger = OperationLedger::new(1).unwrap();
        ledger.register(get_configuration(5)).unwrap();
        assert_eq!(
            ledger.register(get_configuration(6)),
            Err(OperationsError::LedgerFull)
        );
        ledger.set_phase(id(5), OperationPhase::Running).unwrap();
        ledger.set_phase(id(5), OperationPhase::Succeeded).unwrap();
        assert_eq!(
            ledger.register(get_configuration(6)),
            Ok(RegisterOutcome::Accepted)
        );
        assert!(ledger.get(id(5)).is_none());
    }

    #[test]
    fn command_mailbox_is_bounded_and_preserves_full_request() {
        let (port, inbox) = channel_command_mailbox(1).unwrap();
        let first = get_configuration(7);
        port.try_submit(first.clone()).unwrap();
        assert_eq!(
            port.try_submit(get_configuration(8)),
            Err(CommandSubmitError::QueueFull)
        );
        assert_eq!(inbox.try_receive().unwrap(), Some(first));
        assert_eq!(inbox.try_receive().unwrap(), None);
    }
}
