use core::fmt;
use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};

use sensor_core::HardwareId;
use sensor_protocol::{
    ConfigOperationResult, ConfigValidationResult, MeasurementConfiguration, ObservedConfiguration,
};

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
            if matches!(
                mode,
                ChannelOperatingMode::Maintenance | ChannelOperatingMode::Faulted
            ) =>
        {
            Ok(())
        }
        OperationKind::ScanUnassigned { .. } | OperationKind::AssignAddress { .. }
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
    pub result: Option<OperationResult>,
    pub error: Option<OperationsError>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationResult {
    ConfigurationObserved(ObservedConfiguration),
    ConfigurationValidated(ConfigValidationResult),
    ConfigurationApplied(ConfigOperationResult),
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
            result: None,
            error: None,
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

    pub fn set_error(
        &mut self,
        operation_id: OperationId,
        phase: OperationPhase,
        error: OperationsError,
    ) -> Result<(), OperationsError> {
        if !matches!(phase, OperationPhase::Rejected | OperationPhase::Failed) {
            return Err(OperationsError::InvalidErrorPhase(phase));
        }
        self.set_phase(operation_id, phase)?;
        let record = self
            .records
            .iter_mut()
            .find(|record| record.request.operation_id == operation_id)
            .ok_or(OperationsError::UnknownOperation(operation_id))?;
        record.error = Some(error);
        Ok(())
    }

    pub fn set_result(
        &mut self,
        operation_id: OperationId,
        result: OperationResult,
    ) -> Result<(), OperationsError> {
        let record = self
            .records
            .iter_mut()
            .find(|record| record.request.operation_id == operation_id)
            .ok_or(OperationsError::UnknownOperation(operation_id))?;
        if record.phase != OperationPhase::Running {
            return Err(OperationsError::InvalidResultPhase(record.phase));
        }
        record.result = Some(result);
        Ok(())
    }

    pub fn get(&self, operation_id: OperationId) -> Option<&OperationRecord> {
        self.records
            .iter()
            .find(|record| record.request.operation_id == operation_id)
    }

    pub fn records(&self) -> Vec<OperationRecord> {
        self.records.iter().cloned().collect()
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelOperationsPolicy {
    pub mailbox_capacity: usize,
    pub ledger_capacity: usize,
}

impl ChannelOperationsPolicy {
    pub fn validate(self) -> Result<Self, OperationsError> {
        if self.mailbox_capacity == 0 || self.ledger_capacity == 0 {
            Err(OperationsError::ZeroCapacity)
        } else {
            Ok(self)
        }
    }
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelOperationsSnapshot {
    pub channel_id: u8,
    pub mode: ChannelOperatingMode,
    pub active_operation: Option<OperationId>,
    pub records: Vec<OperationRecord>,
}

#[derive(Clone)]
pub struct ChannelOperationsReader {
    snapshot: Arc<Mutex<ChannelOperationsSnapshot>>,
}

#[derive(Clone)]
pub struct ChannelOperationsHandle {
    channel_id: u8,
    commands: ChannelCommandPort,
    status: ChannelOperationsReader,
}

impl ChannelOperationsHandle {
    pub const fn channel_id(&self) -> u8 {
        self.channel_id
    }

    pub fn try_submit(&self, request: OperationRequest) -> Result<(), CommandSubmitError> {
        if request.target.channel_id() != self.channel_id {
            return Err(CommandSubmitError::Invalid(OperationsError::WrongChannel {
                expected: self.channel_id,
                received: request.target.channel_id(),
            }));
        }
        self.commands.try_submit(request)
    }

    pub fn snapshot(&self) -> Result<ChannelOperationsSnapshot, OperationsError> {
        self.status.snapshot()
    }
}

struct ChannelOperationsPublisher {
    snapshot: Arc<Mutex<ChannelOperationsSnapshot>>,
}

impl ChannelOperationsReader {
    pub fn snapshot(&self) -> Result<ChannelOperationsSnapshot, OperationsError> {
        self.snapshot
            .lock()
            .map(|snapshot| snapshot.clone())
            .map_err(|_| OperationsError::StatusUnavailable)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChannelCommandDecision {
    None,
    Existing {
        operation_id: OperationId,
        phase: OperationPhase,
    },
    Rejected {
        operation_id: OperationId,
        reason: OperationsError,
    },
    Started(OperationRequest),
}

/// Worker-owned admission boundary. It receives at most one request per poll,
/// owns the idempotency ledger, and publishes immutable snapshots for API
/// readers. It contains no wire or measurement behavior.
pub struct ChannelOperationsController {
    channel_id: u8,
    mode: ChannelOperatingMode,
    inbox: ChannelCommandInbox,
    ledger: OperationLedger,
    active_operation: Option<OperationId>,
    publisher: ChannelOperationsPublisher,
}

impl ChannelOperationsController {
    pub fn new(
        channel_id: u8,
        policy: ChannelOperationsPolicy,
    ) -> Result<(Self, ChannelOperationsHandle), OperationsError> {
        OperationTarget::Channel { channel_id }.validate()?;
        let policy = policy.validate()?;
        let (port, inbox) = channel_command_mailbox(policy.mailbox_capacity)?;
        let ledger = OperationLedger::new(policy.ledger_capacity)?;
        let snapshot = Arc::new(Mutex::new(ChannelOperationsSnapshot {
            channel_id,
            mode: ChannelOperatingMode::Recording,
            active_operation: None,
            records: Vec::new(),
        }));
        let status = ChannelOperationsReader {
            snapshot: Arc::clone(&snapshot),
        };
        let handle = ChannelOperationsHandle {
            channel_id,
            commands: port,
            status,
        };
        Ok((
            Self {
                channel_id,
                mode: ChannelOperatingMode::Recording,
                inbox,
                ledger,
                active_operation: None,
                publisher: ChannelOperationsPublisher { snapshot },
            },
            handle,
        ))
    }

    pub const fn mode(&self) -> ChannelOperatingMode {
        self.mode
    }

    pub const fn active_operation(&self) -> Option<OperationId> {
        self.active_operation
    }

    pub fn poll_command(&mut self) -> Result<ChannelCommandDecision, OperationsError> {
        let Some(request) = self.inbox.try_receive()? else {
            return Ok(ChannelCommandDecision::None);
        };
        if request.target.channel_id() != self.channel_id {
            return Err(OperationsError::WrongChannel {
                expected: self.channel_id,
                received: request.target.channel_id(),
            });
        }
        match self.ledger.register(request.clone())? {
            RegisterOutcome::Existing(phase) => {
                self.publish()?;
                return Ok(ChannelCommandDecision::Existing {
                    operation_id: request.operation_id,
                    phase,
                });
            }
            RegisterOutcome::Accepted => {}
        }
        if let Err(reason) = admit_operation(&request, self.mode) {
            self.ledger
                .set_error(request.operation_id, OperationPhase::Rejected, reason)?;
            self.publish()?;
            return Ok(ChannelCommandDecision::Rejected {
                operation_id: request.operation_id,
                reason,
            });
        }
        if self.active_operation.is_some() {
            self.ledger.set_error(
                request.operation_id,
                OperationPhase::Rejected,
                OperationsError::OperationInProgress,
            )?;
            self.publish()?;
            return Ok(ChannelCommandDecision::Rejected {
                operation_id: request.operation_id,
                reason: OperationsError::OperationInProgress,
            });
        }
        self.ledger
            .set_phase(request.operation_id, OperationPhase::Running)?;
        if request.kind == OperationKind::ReadStatus {
            self.ledger
                .set_phase(request.operation_id, OperationPhase::Succeeded)?;
            self.publish()?;
            return Ok(ChannelCommandDecision::Existing {
                operation_id: request.operation_id,
                phase: OperationPhase::Succeeded,
            });
        }
        self.active_operation = Some(request.operation_id);
        self.publish()?;
        Ok(ChannelCommandDecision::Started(request))
    }

    pub fn set_mode(&mut self, mode: ChannelOperatingMode) -> Result<(), OperationsError> {
        self.mode = mode;
        self.publish()
    }

    pub fn finish_active(&mut self, succeeded: bool) -> Result<OperationId, OperationsError> {
        let operation_id = self
            .active_operation
            .take()
            .ok_or(OperationsError::NoActiveOperation)?;
        self.ledger.set_phase(
            operation_id,
            if succeeded {
                OperationPhase::Succeeded
            } else {
                OperationPhase::Failed
            },
        )?;
        self.publish()?;
        Ok(operation_id)
    }

    pub fn complete_active(
        &mut self,
        result: OperationResult,
    ) -> Result<OperationId, OperationsError> {
        let operation_id = self
            .active_operation
            .take()
            .ok_or(OperationsError::NoActiveOperation)?;
        self.ledger.set_result(operation_id, result)?;
        self.ledger
            .set_phase(operation_id, OperationPhase::Succeeded)?;
        self.publish()?;
        Ok(operation_id)
    }

    pub fn fail_active(&mut self, error: OperationsError) -> Result<OperationId, OperationsError> {
        let operation_id = self
            .active_operation
            .take()
            .ok_or(OperationsError::NoActiveOperation)?;
        self.ledger
            .set_error(operation_id, OperationPhase::Failed, error)?;
        self.publish()?;
        Ok(operation_id)
    }

    pub fn fail_active_with_result(
        &mut self,
        result: OperationResult,
        error: OperationsError,
    ) -> Result<OperationId, OperationsError> {
        let operation_id = self
            .active_operation
            .take()
            .ok_or(OperationsError::NoActiveOperation)?;
        self.ledger.set_result(operation_id, result)?;
        self.ledger
            .set_error(operation_id, OperationPhase::Failed, error)?;
        self.publish()?;
        Ok(operation_id)
    }

    fn publish(&self) -> Result<(), OperationsError> {
        let mut snapshot = self
            .publisher
            .snapshot
            .lock()
            .map_err(|_| OperationsError::StatusUnavailable)?;
        *snapshot = ChannelOperationsSnapshot {
            channel_id: self.channel_id,
            mode: self.mode,
            active_operation: self.active_operation,
            records: self.ledger.records(),
        };
        Ok(())
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
    InvalidErrorPhase(OperationPhase),
    InvalidResultPhase(OperationPhase),
    CommandPathDisconnected,
    WrongChannel {
        expected: u8,
        received: u8,
    },
    OperationInProgress,
    NoActiveOperation,
    StatusUnavailable,
    ExecutorUnavailable,
    NodeNotConfigured,
    ConfigurationDeadline,
    ConfigurationFailed,
    ConfigurationAdmissionRejected,
    ConfigurationRejected,
    MaintenanceBarrierDeadline,
    MaintenanceDrainDeadline,
    MaintenanceConvergenceFailed,
    StreamReconciliationRequired,
    ResumeVerificationFailed,
    ResumeAdmissionRejected,
    ResumeActivationFailed,
    NodeOffline,
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
            Self::InvalidErrorPhase(phase) => {
                write!(formatter, "operation error cannot be attached to {phase:?}")
            }
            Self::InvalidResultPhase(phase) => {
                write!(
                    formatter,
                    "operation result cannot be attached to {phase:?}"
                )
            }
            Self::CommandPathDisconnected => {
                formatter.write_str("channel command path disconnected")
            }
            Self::WrongChannel { expected, received } => write!(
                formatter,
                "operation for channel {received} reached channel {expected}"
            ),
            Self::OperationInProgress => {
                formatter.write_str("another channel operation is already running")
            }
            Self::NoActiveOperation => formatter.write_str("channel has no active operation"),
            Self::StatusUnavailable => {
                formatter.write_str("channel operation status is unavailable")
            }
            Self::ExecutorUnavailable => formatter.write_str("operation executor is not connected"),
            Self::NodeNotConfigured => {
                formatter.write_str("operation target is not configured on this channel")
            }
            Self::ConfigurationDeadline => {
                formatter.write_str("configuration operation deadline expired")
            }
            Self::ConfigurationFailed => formatter.write_str("configuration operation failed"),
            Self::ConfigurationAdmissionRejected => {
                formatter.write_str("configuration would exceed the channel admission budget")
            }
            Self::ConfigurationRejected => {
                formatter.write_str("node rejected the configuration operation")
            }
            Self::MaintenanceBarrierDeadline => {
                formatter.write_str("maintenance quiesce barrier deadline expired")
            }
            Self::MaintenanceDrainDeadline => {
                formatter.write_str("maintenance final drain deadline expired")
            }
            Self::MaintenanceConvergenceFailed => {
                formatter.write_str("channel could not converge to maintenance")
            }
            Self::StreamReconciliationRequired => {
                formatter.write_str("node restarted and stream reconciliation is required")
            }
            Self::ResumeVerificationFailed => {
                formatter.write_str("recording resume verification failed")
            }
            Self::ResumeAdmissionRejected => {
                formatter.write_str("recording resume admission was rejected")
            }
            Self::ResumeActivationFailed => {
                formatter.write_str("recording resume activation failed")
            }
            Self::NodeOffline => formatter.write_str("node is offline"),
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
        assert_eq!(
            admit_operation(&request, ChannelOperatingMode::Faulted),
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

    fn channel_request(operation_id: u64, kind: OperationKind) -> OperationRequest {
        OperationRequest {
            operation_id: id(operation_id),
            target: OperationTarget::Channel { channel_id: 2 },
            kind,
        }
    }

    #[test]
    fn controller_publishes_idempotent_operation_lifecycle() {
        let (mut controller, handle) = ChannelOperationsController::new(
            2,
            ChannelOperationsPolicy {
                mailbox_capacity: 2,
                ledger_capacity: 4,
            },
        )
        .unwrap();
        let request = channel_request(10, OperationKind::EnterMaintenance);
        handle.try_submit(request.clone()).unwrap();
        assert_eq!(
            controller.poll_command().unwrap(),
            ChannelCommandDecision::Started(request.clone())
        );
        let running = handle.snapshot().unwrap();
        assert_eq!(running.mode, ChannelOperatingMode::Recording);
        assert_eq!(running.active_operation, Some(id(10)));
        assert_eq!(running.records[0].phase, OperationPhase::Running);

        handle.try_submit(request).unwrap();
        assert_eq!(
            controller.poll_command().unwrap(),
            ChannelCommandDecision::Existing {
                operation_id: id(10),
                phase: OperationPhase::Running,
            }
        );
        controller
            .set_mode(ChannelOperatingMode::EnteringMaintenance)
            .unwrap();
        controller.finish_active(true).unwrap();
        let complete = handle.snapshot().unwrap();
        assert_eq!(complete.mode, ChannelOperatingMode::EnteringMaintenance);
        assert_eq!(complete.active_operation, None);
        assert_eq!(complete.records[0].phase, OperationPhase::Succeeded);
    }

    #[test]
    fn controller_publishes_typed_configuration_result() {
        let (mut controller, handle) = ChannelOperationsController::new(
            2,
            ChannelOperationsPolicy {
                mailbox_capacity: 1,
                ledger_capacity: 2,
            },
        )
        .unwrap();
        controller
            .set_mode(ChannelOperatingMode::Maintenance)
            .unwrap();
        let request = get_configuration(15);
        handle.try_submit(request.clone()).unwrap();
        assert_eq!(
            controller.poll_command().unwrap(),
            ChannelCommandDecision::Started(request)
        );
        let observed = ObservedConfiguration {
            revision: 3,
            desired: configuration(),
            sensor_odr: sensor_protocol::Adxl355Odr::Hz250,
            decimation_factor: 1,
        };
        controller
            .complete_active(OperationResult::ConfigurationObserved(observed))
            .unwrap();
        let snapshot = handle.snapshot().unwrap();
        assert_eq!(snapshot.records[0].phase, OperationPhase::Succeeded);
        assert_eq!(
            snapshot.records[0].result,
            Some(OperationResult::ConfigurationObserved(observed))
        );
    }

    #[test]
    fn controller_rejects_conflicting_work_without_replacing_active_operation() {
        let (mut controller, handle) = ChannelOperationsController::new(
            2,
            ChannelOperationsPolicy {
                mailbox_capacity: 2,
                ledger_capacity: 4,
            },
        )
        .unwrap();
        handle
            .try_submit(channel_request(11, OperationKind::EnterMaintenance))
            .unwrap();
        controller.poll_command().unwrap();
        handle
            .try_submit(channel_request(12, OperationKind::EnterMaintenance))
            .unwrap();
        assert_eq!(
            controller.poll_command().unwrap(),
            ChannelCommandDecision::Rejected {
                operation_id: id(12),
                reason: OperationsError::OperationInProgress,
            }
        );
        let snapshot = handle.snapshot().unwrap();
        assert_eq!(snapshot.active_operation, Some(id(11)));
        assert_eq!(snapshot.records[0].phase, OperationPhase::Running);
        assert_eq!(snapshot.records[1].phase, OperationPhase::Rejected);
        assert_eq!(
            snapshot.records[1].error,
            Some(OperationsError::OperationInProgress)
        );
    }

    #[test]
    fn executor_failure_is_terminal_and_preserves_its_reason() {
        let (mut controller, handle) = ChannelOperationsController::new(
            2,
            ChannelOperationsPolicy {
                mailbox_capacity: 1,
                ledger_capacity: 2,
            },
        )
        .unwrap();
        handle
            .try_submit(channel_request(14, OperationKind::EnterMaintenance))
            .unwrap();
        controller.poll_command().unwrap();
        controller
            .fail_active(OperationsError::ExecutorUnavailable)
            .unwrap();
        let snapshot = handle.snapshot().unwrap();
        assert_eq!(snapshot.active_operation, None);
        assert_eq!(snapshot.records[0].phase, OperationPhase::Failed);
        assert_eq!(
            snapshot.records[0].error,
            Some(OperationsError::ExecutorUnavailable)
        );
    }

    #[test]
    fn channel_handle_rejects_misrouted_work_before_the_worker_inbox() {
        let (mut controller, handle) = ChannelOperationsController::new(
            2,
            ChannelOperationsPolicy {
                mailbox_capacity: 1,
                ledger_capacity: 2,
            },
        )
        .unwrap();
        let mut request = channel_request(13, OperationKind::ReadStatus);
        request.target = OperationTarget::Channel { channel_id: 3 };
        assert_eq!(
            handle.try_submit(request),
            Err(CommandSubmitError::Invalid(OperationsError::WrongChannel {
                expected: 2,
                received: 3,
            }))
        );
        assert_eq!(
            controller.poll_command().unwrap(),
            ChannelCommandDecision::None
        );
    }
}
