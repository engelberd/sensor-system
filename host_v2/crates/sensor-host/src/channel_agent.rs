use core::fmt;
use std::collections::VecDeque;

use sensor_core::{BootEpoch, ClockObservation, ClockObservationError};
use sensor_protocol::{
    decode_stream_state_success, decode_telemetry_snapshot, decode_time_response,
    ConfigOperationRequest, Frame, MeasurementConfiguration, MeasurementWireError,
    NodeCapabilities, NodeIdentity, ObservabilityProfile, ResponseStatus, RuntimeCommand,
    RuntimeOperation, RuntimeResult, ServiceId, StreamState, TelemetrySnapshot, TelemetryWireError,
    TimeWireError, GET_STREAM_STATE_MESSAGE_ID, GET_TELEMETRY_SNAPSHOT_MESSAGE_ID,
    OBSERVE_CLOCK_MESSAGE_ID, TELEMETRY_SERVICE_ID, TIME_SERVICE_ID,
};

use crate::commissioning::{
    CommissioningError, CommissioningOperation, CommissioningOutcome, CommissioningPhase,
    CommissioningRequest,
};
use crate::configuration::{
    ConfigurationError, ConfigurationOperation, ConfigurationOutcome, ConfigurationPhase,
    ConfigurationRequest,
};
use crate::handshake::{
    capabilities_request, identity_request, HandshakeError, HandshakePhase, HandshakeRequirements,
    NodeHandshake,
};
use crate::lifecycle::{LifecycleError, LifecycleOperation, LifecycleOutcome, LifecyclePhase};
use crate::measurement_transfer::{
    MeasurementOutcome, MeasurementTransfer, MeasurementTransferError, PersistenceKind,
    RecoveryReason,
};
use crate::transaction::{
    DurationMicros, MonotonicMicros, ResponseDisposition, TimerEvent, TransactionCore,
    TransactionError,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelPolicy {
    pub transaction_timeout: DurationMicros,
    pub retry_delay: DurationMicros,
    /// Small link errors are retried inside one bus slot. Exhaustion yields the
    /// measurement to `ChannelRuntime`, so another node can use the channel.
    pub measurement_attempts_per_slot: u8,
    pub max_pending_work_items: usize,
}

impl ChannelPolicy {
    pub fn validate(self) -> Result<Self, ChannelAgentError> {
        if self.max_pending_work_items == 0 {
            Err(ChannelAgentError::ZeroQueueCapacity)
        } else if self.measurement_attempts_per_slot == 0 {
            Err(ChannelAgentError::ZeroMeasurementAttemptBudget)
        } else {
            Ok(self)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChannelEvent {
    Idle,
    TransmitReady,
    HandshakeProgress {
        node_address: u8,
        phase: HandshakePhase,
    },
    HandshakeComplete {
        node_address: u8,
        identity: NodeIdentity,
        capabilities: NodeCapabilities,
    },
    HandshakeTimedOut {
        node_address: u8,
        phase: HandshakePhase,
    },
    StreamState {
        node_address: u8,
        state: StreamState,
    },
    StreamStateTimedOut {
        node_address: u8,
    },
    StreamStateNotAccepted {
        node_address: u8,
    },
    MeasurementNotAccepted {
        node_address: u8,
    },
    MeasurementNoData {
        node_address: u8,
        state: sensor_protocol::StreamState,
    },
    MeasurementPersistenceRequired {
        node_address: u8,
        kind: PersistenceKind,
    },
    MeasurementRecoveryRequired {
        node_address: u8,
        reason: RecoveryReason,
    },
    MeasurementTimedOut {
        node_address: u8,
    },
    MeasurementAttemptsExhausted {
        node_address: u8,
    },
    TimeObservation {
        node_address: u8,
        observation: ClockObservation,
    },
    TimeSyncTimedOut {
        node_address: u8,
    },
    TimeSyncNotAccepted {
        node_address: u8,
    },
    TelemetrySnapshot {
        node_address: u8,
        snapshot: TelemetrySnapshot,
    },
    TelemetryTimedOut {
        node_address: u8,
    },
    TelemetryNotAccepted {
        node_address: u8,
    },
    MeasurementHandoffPending {
        node_address: u8,
    },
    LifecycleNotAccepted {
        node_address: u8,
        operation_id: u32,
    },
    LifecycleProgress {
        node_address: u8,
        operation_id: u32,
        state: sensor_protocol::RuntimeState,
    },
    LifecycleComplete {
        node_address: u8,
        result: RuntimeResult,
    },
    LifecycleTimedOut {
        node_address: u8,
        operation_id: u32,
    },
    ConfigurationNotAccepted {
        node_address: u8,
        operation_id: Option<u32>,
    },
    ConfigurationComplete {
        node_address: u8,
        outcome: ConfigurationOutcome,
    },
    ConfigurationTimedOut {
        node_address: u8,
        operation_id: Option<u32>,
    },
    CommissioningSlotEmpty {
        round_nonce: u64,
        slot_index: u16,
    },
    CommissioningNotAccepted {
        operation_id: u32,
    },
    CommissioningComplete {
        outcome: CommissioningOutcome,
    },
    CommissioningTimedOut {
        operation_id: u32,
    },
    WireAttemptFailed {
        node_address: u8,
    },
    QuarantineEnded,
    UnrelatedResponse,
    LateResponse,
    UnsolicitedResponse,
}

/// Stable identity of control work which an owning state machine may abandon
/// after its own bounded deadline. Sample transfer and time synchronization
/// deliberately have no key here: their settlement belongs to
/// `ChannelWorkCoordinator`, not to the operations control plane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlWorkKey {
    Handshake {
        node_address: u8,
    },
    StreamState {
        node_address: u8,
    },
    Lifecycle {
        node_address: u8,
        operation_id: u32,
    },
    Configuration {
        node_address: u8,
        operation_id: Option<u32>,
    },
    CommissioningProbe {
        round_nonce: u64,
        slot_index: u16,
    },
    CommissioningAssignment {
        operation_id: u32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbandonedControlWork {
    Queued(ControlWorkKey),
    Active(ControlWorkKey),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PendingWork {
    Handshake(NodeHandshake),
    StreamState { node_address: u8 },
    Lifecycle(LifecycleOperation),
    Configuration(ConfigurationOperation),
    Commissioning(CommissioningOperation),
    Measurement(Box<MeasurementTransfer>),
    TimeSync(TimeSyncAttempt),
    Telemetry { node_address: u8 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TimeSyncAttempt {
    node_address: u8,
    expected_boot_epoch: BootEpoch,
    host_send_us: Option<u64>,
}

impl PendingWork {
    fn node_address(&self) -> u8 {
        match self {
            Self::Handshake(handshake) => handshake.requirements().node_address,
            Self::StreamState { node_address } => *node_address,
            Self::Lifecycle(operation) => operation.node_address(),
            Self::Configuration(operation) => operation.node_address(),
            Self::Commissioning(_) => sensor_protocol::COMMISSIONING_ADDRESS,
            Self::Measurement(transfer) => transfer.node_address(),
            Self::TimeSync(attempt) => attempt.node_address,
            Self::Telemetry { node_address } => *node_address,
        }
    }

    fn matches_control_key(&self, key: ControlWorkKey) -> bool {
        match (self, key) {
            (Self::Handshake(handshake), ControlWorkKey::Handshake { node_address }) => {
                handshake.requirements().node_address == node_address
            }
            (
                Self::StreamState {
                    node_address: pending,
                },
                ControlWorkKey::StreamState { node_address },
            ) => *pending == node_address,
            (
                Self::Lifecycle(operation),
                ControlWorkKey::Lifecycle {
                    node_address,
                    operation_id,
                },
            ) => {
                operation.node_address() == node_address
                    && operation.command().operation_id == operation_id
            }
            (
                Self::Configuration(operation),
                ControlWorkKey::Configuration {
                    node_address,
                    operation_id,
                },
            ) => {
                operation.node_address() == node_address
                    && operation.request().operation_id() == operation_id
            }
            (
                Self::Commissioning(operation),
                ControlWorkKey::CommissioningProbe {
                    round_nonce,
                    slot_index,
                },
            ) => matches!(
                operation.request(),
                CommissioningRequest::Probe(value)
                    if value.round_nonce == round_nonce && value.slot_index == slot_index
            ),
            (
                Self::Commissioning(operation),
                ControlWorkKey::CommissioningAssignment { operation_id },
            ) => operation.request().operation_id() == Some(operation_id),
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActiveWork {
    work: PendingWork,
    retry_not_before: MonotonicMicros,
    attempts_started: u8,
}

pub struct ChannelAgent {
    transactions: TransactionCore,
    policy: ChannelPolicy,
    next_operation_id: u32,
    pending: VecDeque<PendingWork>,
    active: Option<ActiveWork>,
    outbound: Option<Frame>,
    completed_measurement: Option<MeasurementTransfer>,
}

impl ChannelAgent {
    pub fn new(
        transaction_seed: u32,
        operation_seed: u32,
        quarantine_duration: DurationMicros,
        policy: ChannelPolicy,
    ) -> Result<Self, ChannelAgentError> {
        Ok(Self {
            transactions: TransactionCore::new(transaction_seed, quarantine_duration),
            policy: policy.validate()?,
            next_operation_id: if operation_seed == 0 {
                1
            } else {
                operation_seed
            },
            pending: VecDeque::with_capacity(policy.max_pending_work_items),
            active: None,
            outbound: None,
            completed_measurement: None,
        })
    }

    pub fn submit_lifecycle(
        &mut self,
        node_address: u8,
        operation: RuntimeOperation,
        requested_profile: ObservabilityProfile,
    ) -> Result<u32, ChannelAgentError> {
        self.ensure_queue_capacity()?;
        let operation_id = self.allocate_operation_id()?;
        let lifecycle = LifecycleOperation::new(
            node_address,
            RuntimeCommand {
                operation_id,
                operation,
                requested_profile,
            },
        )?;
        self.pending.push_back(PendingWork::Lifecycle(lifecycle));
        Ok(operation_id)
    }

    pub fn submit_get_configuration(&mut self, node_address: u8) -> Result<(), ChannelAgentError> {
        self.submit_configuration(ConfigurationOperation::new(
            node_address,
            ConfigurationRequest::Get,
        )?)
    }

    pub fn submit_validate_configuration(
        &mut self,
        node_address: u8,
        configuration: MeasurementConfiguration,
    ) -> Result<(), ChannelAgentError> {
        self.submit_configuration(ConfigurationOperation::new(
            node_address,
            ConfigurationRequest::Validate(configuration),
        )?)
    }

    pub fn submit_apply_configuration(
        &mut self,
        node_address: u8,
        expected_revision: u32,
        configuration: MeasurementConfiguration,
    ) -> Result<u32, ChannelAgentError> {
        self.ensure_queue_capacity()?;
        let operation_id = self.allocate_operation_id()?;
        let operation = ConfigurationOperation::new(
            node_address,
            ConfigurationRequest::Apply(ConfigOperationRequest {
                operation_id,
                expected_revision,
                configuration,
            }),
        )?;
        self.pending
            .push_back(PendingWork::Configuration(operation));
        Ok(operation_id)
    }

    pub fn submit_reset_configuration(
        &mut self,
        node_address: u8,
        expected_revision: u32,
    ) -> Result<u32, ChannelAgentError> {
        self.ensure_queue_capacity()?;
        let operation_id = self.allocate_operation_id()?;
        let operation = ConfigurationOperation::new(
            node_address,
            ConfigurationRequest::Reset {
                operation_id,
                expected_revision,
            },
        )?;
        self.pending
            .push_back(PendingWork::Configuration(operation));
        Ok(operation_id)
    }

    fn submit_configuration(
        &mut self,
        operation: ConfigurationOperation,
    ) -> Result<(), ChannelAgentError> {
        self.ensure_queue_capacity()?;
        self.pending
            .push_back(PendingWork::Configuration(operation));
        Ok(())
    }

    pub fn submit_commissioning_probe(
        &mut self,
        request: sensor_protocol::ProbeSlotRequest,
    ) -> Result<(), ChannelAgentError> {
        self.ensure_queue_capacity()?;
        self.pending
            .push_back(PendingWork::Commissioning(CommissioningOperation::new(
                CommissioningRequest::Probe(request),
            )?));
        Ok(())
    }

    pub fn submit_commissioning_assignment(
        &mut self,
        round_nonce: u64,
        hardware_id: sensor_core::HardwareId,
        node_address: u8,
    ) -> Result<u32, ChannelAgentError> {
        self.ensure_queue_capacity()?;
        let operation_id = self.allocate_operation_id()?;
        self.pending
            .push_back(PendingWork::Commissioning(CommissioningOperation::new(
                CommissioningRequest::Assign(sensor_protocol::AssignAddressRequest {
                    operation_id,
                    round_nonce,
                    hardware_id,
                    node_address,
                }),
            )?));
        Ok(operation_id)
    }

    pub fn submit_handshake(
        &mut self,
        requirements: HandshakeRequirements,
    ) -> Result<(), ChannelAgentError> {
        self.ensure_queue_capacity()?;
        self.pending
            .push_back(PendingWork::Handshake(NodeHandshake::new(requirements)));
        Ok(())
    }

    /// Queues the authoritative measurement-journal cursor read on the same
    /// transaction owner as pulls, lifecycle commands, and time observations.
    pub fn submit_stream_state(&mut self, node_address: u8) -> Result<(), ChannelAgentError> {
        self.ensure_queue_capacity()?;
        if node_address == sensor_protocol::HOST_ADDRESS
            || node_address == sensor_protocol::BROADCAST_ADDRESS
        {
            return Err(ChannelAgentError::InvalidStreamStateNodeAddress(
                node_address,
            ));
        }
        self.pending
            .push_back(PendingWork::StreamState { node_address });
        Ok(())
    }

    pub fn submit_measurement(
        &mut self,
        transfer: MeasurementTransfer,
    ) -> Result<(), ChannelAgentError> {
        self.try_submit_measurement(transfer).map_err(|rejected| {
            let RejectedMeasurement { error, .. } = *rejected;
            error
        })
    }

    pub fn submit_time_sync(
        &mut self,
        node_address: u8,
        expected_boot_epoch: BootEpoch,
    ) -> Result<(), ChannelAgentError> {
        self.ensure_queue_capacity()?;
        if node_address == sensor_protocol::HOST_ADDRESS
            || node_address == sensor_protocol::BROADCAST_ADDRESS
        {
            return Err(ChannelAgentError::InvalidTimeNodeAddress(node_address));
        }
        if expected_boot_epoch.get() == 0 {
            return Err(ChannelAgentError::ZeroTimeBootEpoch);
        }
        self.pending
            .push_back(PendingWork::TimeSync(TimeSyncAttempt {
                node_address,
                expected_boot_epoch,
                host_send_us: None,
            }));
        Ok(())
    }

    pub fn submit_telemetry(&mut self, node_address: u8) -> Result<(), ChannelAgentError> {
        self.ensure_queue_capacity()?;
        if node_address == sensor_protocol::HOST_ADDRESS
            || node_address == sensor_protocol::BROADCAST_ADDRESS
        {
            return Err(ChannelAgentError::InvalidTelemetryNodeAddress(node_address));
        }
        self.pending
            .push_back(PendingWork::Telemetry { node_address });
        Ok(())
    }

    pub fn try_submit_measurement(
        &mut self,
        transfer: MeasurementTransfer,
    ) -> Result<(), Box<RejectedMeasurement>> {
        if let Err(error) = self.ensure_queue_capacity() {
            return Err(Box::new(RejectedMeasurement { error, transfer }));
        }
        self.pending
            .push_back(PendingWork::Measurement(Box::new(transfer)));
        Ok(())
    }

    pub fn poll(&mut self, now: MonotonicMicros) -> Result<ChannelEvent, ChannelAgentError> {
        if self.outbound.is_some() {
            return Ok(ChannelEvent::TransmitReady);
        }
        if let Some(transfer) = self.completed_measurement.as_ref() {
            return Ok(ChannelEvent::MeasurementHandoffPending {
                node_address: transfer.node_address(),
            });
        }

        match self.transactions.on_timer(now)? {
            TimerEvent::TimedOut { .. } => {
                let active = self
                    .active
                    .as_mut()
                    .ok_or(ChannelAgentError::InvariantViolation)?;
                let mut measurement_exhausted = false;
                let timeout_event = match &mut active.work {
                    PendingWork::Handshake(handshake) => ChannelEvent::HandshakeTimedOut {
                        node_address: handshake.requirements().node_address,
                        phase: handshake.phase(),
                    },
                    PendingWork::StreamState { node_address } => {
                        ChannelEvent::StreamStateTimedOut {
                            node_address: *node_address,
                        }
                    }
                    PendingWork::Lifecycle(operation) => {
                        operation.mark_timeout()?;
                        ChannelEvent::LifecycleTimedOut {
                            node_address: operation.node_address(),
                            operation_id: operation.command().operation_id,
                        }
                    }
                    PendingWork::Configuration(operation) => {
                        operation.mark_timeout()?;
                        ChannelEvent::ConfigurationTimedOut {
                            node_address: operation.node_address(),
                            operation_id: operation.request().operation_id(),
                        }
                    }
                    PendingWork::Commissioning(operation) => {
                        let request = operation.request();
                        operation.mark_timeout()?;
                        match request {
                            CommissioningRequest::Probe(probe) => {
                                self.active = None;
                                return Ok(ChannelEvent::CommissioningSlotEmpty {
                                    round_nonce: probe.round_nonce,
                                    slot_index: probe.slot_index,
                                });
                            }
                            CommissioningRequest::Assign(assign) => {
                                ChannelEvent::CommissioningTimedOut {
                                    operation_id: assign.operation_id,
                                }
                            }
                        }
                    }
                    PendingWork::Measurement(transfer) => {
                        transfer.mark_attempt_uncertain()?;
                        measurement_exhausted =
                            active.attempts_started >= self.policy.measurement_attempts_per_slot;
                        ChannelEvent::MeasurementTimedOut {
                            node_address: transfer.node_address(),
                        }
                    }
                    PendingWork::TimeSync(attempt) => ChannelEvent::TimeSyncTimedOut {
                        node_address: attempt.node_address,
                    },
                    PendingWork::Telemetry { node_address } => ChannelEvent::TelemetryTimedOut {
                        node_address: *node_address,
                    },
                };
                active.retry_not_before = now
                    .checked_add(self.policy.retry_delay)
                    .ok_or(ChannelAgentError::TimeOverflow)?;
                if measurement_exhausted {
                    let node_address = self.handoff_active_measurement()?;
                    return Ok(ChannelEvent::MeasurementAttemptsExhausted { node_address });
                }
                if matches!(
                    &active.work,
                    PendingWork::TimeSync(_) | PendingWork::Telemetry { .. }
                ) {
                    self.active = None;
                }
                return Ok(timeout_event);
            }
            TimerEvent::QuarantineEnded => {
                return Ok(ChannelEvent::QuarantineEnded);
            }
            TimerEvent::None => {}
        }

        if self.active.is_none() {
            let Some(operation) = self.pending.pop_front() else {
                return Ok(ChannelEvent::Idle);
            };
            self.active = Some(ActiveWork {
                work: operation,
                retry_not_before: now,
                attempts_started: 0,
            });
        }

        let active = self
            .active
            .as_mut()
            .ok_or(ChannelAgentError::InvariantViolation)?;
        if now < active.retry_not_before || !self.transactions.is_idle() {
            return Ok(ChannelEvent::Idle);
        }
        let frame = match &mut active.work {
            PendingWork::Handshake(handshake) => {
                let request = match handshake.phase() {
                    HandshakePhase::NeedIdentity => identity_request(
                        handshake.requirements().node_address,
                        self.policy.transaction_timeout,
                    ),
                    HandshakePhase::NeedCapabilities => capabilities_request(
                        handshake.requirements().node_address,
                        self.policy.transaction_timeout,
                    ),
                    HandshakePhase::Ready => return Err(ChannelAgentError::InvariantViolation),
                };
                self.transactions.prepare(request)?.request_frame(&[])?
            }
            PendingWork::StreamState { node_address } => self
                .transactions
                .prepare(crate::transaction::RequestSpec {
                    node_address: *node_address,
                    service: ServiceId::Measurement as u8,
                    request_message: GET_STREAM_STATE_MESSAGE_ID,
                    response_message: GET_STREAM_STATE_MESSAGE_ID,
                    timeout: self.policy.transaction_timeout,
                    timeout_recovery: crate::transaction::TimeoutRecovery::RetrySafe,
                })?
                .request_frame(&[])?,
            PendingWork::Lifecycle(operation) => {
                if operation.phase() != LifecyclePhase::ReadyToSend
                    && operation.phase() != LifecyclePhase::ReconcileRequired
                {
                    return Ok(ChannelEvent::Idle);
                }
                let attempt = operation.begin_attempt(self.policy.transaction_timeout)?;
                self.transactions
                    .prepare(attempt.request)?
                    .request_frame(attempt.payload())?
            }
            PendingWork::Configuration(operation) => {
                if operation.phase() != ConfigurationPhase::ReadyToSend
                    && operation.phase() != ConfigurationPhase::ReconcileRequired
                {
                    return Ok(ChannelEvent::Idle);
                }
                let attempt = operation.begin_attempt(self.policy.transaction_timeout)?;
                self.transactions
                    .prepare(attempt.request)?
                    .request_frame(attempt.payload())?
            }
            PendingWork::Commissioning(operation) => {
                if operation.phase() != CommissioningPhase::ReadyToSend
                    && operation.phase() != CommissioningPhase::ReconcileRequired
                {
                    return Ok(ChannelEvent::Idle);
                }
                let attempt = operation.begin_attempt(self.policy.transaction_timeout)?;
                self.transactions
                    .prepare_routed(attempt.request)?
                    .request_frame(attempt.payload())?
            }
            PendingWork::Measurement(transfer) => {
                let attempt = transfer.begin_attempt(self.policy.transaction_timeout)?;
                active.attempts_started = active.attempts_started.saturating_add(1);
                self.transactions
                    .prepare(attempt.request)?
                    .request_frame(attempt.payload())?
            }
            PendingWork::TimeSync(attempt) => {
                attempt.host_send_us = None;
                self.transactions
                    .prepare(crate::transaction::RequestSpec {
                        node_address: attempt.node_address,
                        service: TIME_SERVICE_ID,
                        request_message: OBSERVE_CLOCK_MESSAGE_ID,
                        response_message: OBSERVE_CLOCK_MESSAGE_ID,
                        timeout: self.policy.transaction_timeout,
                        timeout_recovery: crate::transaction::TimeoutRecovery::RetrySafe,
                    })?
                    .request_frame(&[])?
            }
            PendingWork::Telemetry { node_address } => self
                .transactions
                .prepare(crate::transaction::RequestSpec {
                    node_address: *node_address,
                    service: TELEMETRY_SERVICE_ID,
                    request_message: GET_TELEMETRY_SNAPSHOT_MESSAGE_ID,
                    response_message: GET_TELEMETRY_SNAPSHOT_MESSAGE_ID,
                    timeout: self.policy.transaction_timeout,
                    timeout_recovery: crate::transaction::TimeoutRecovery::RetrySafe,
                })?
                .request_frame(&[])?,
        };
        self.outbound = Some(frame);
        Ok(ChannelEvent::TransmitReady)
    }

    /// Removes the frame announced by `ChannelEvent::TransmitReady`.
    /// The port owner remains responsible for encoding and physically sending it.
    pub fn take_outbound(&mut self) -> Option<Frame> {
        self.outbound.take()
    }

    /// Arms the response timeout after the complete request has left the port.
    pub fn transmit_completed(&mut self, now: MonotonicMicros) -> Result<(), ChannelAgentError> {
        if self.outbound.is_some() {
            return Err(ChannelAgentError::OutboundNotTaken);
        }
        self.transactions.mark_transmitted(now)?;
        if let Some(ActiveWork {
            work: PendingWork::TimeSync(attempt),
            ..
        }) = self.active.as_mut()
        {
            attempt.host_send_us = Some(now.get());
        }
        Ok(())
    }

    /// Cancels a prepared attempt whose delivery is uncertain. A lifecycle
    /// retry reconciles with the same operation ID.
    pub fn transmit_failed(
        &mut self,
        now: MonotonicMicros,
    ) -> Result<ChannelEvent, ChannelAgentError> {
        self.outbound = None;
        self.transactions.cancel_transmit()?;
        let active = self
            .active
            .as_mut()
            .ok_or(ChannelAgentError::InvariantViolation)?;
        match &mut active.work {
            PendingWork::Lifecycle(operation) => operation.mark_timeout()?,
            PendingWork::Configuration(operation) => operation.mark_timeout()?,
            PendingWork::Commissioning(operation) => operation.mark_transmit_failed()?,
            PendingWork::Measurement(transfer) => transfer.mark_attempt_uncertain()?,
            PendingWork::Handshake(_) => {}
            PendingWork::StreamState { .. } => {}
            PendingWork::TimeSync(_) => {}
            PendingWork::Telemetry { .. } => {}
        }
        active.retry_not_before = now
            .checked_add(self.policy.retry_delay)
            .ok_or(ChannelAgentError::TimeOverflow)?;
        if matches!(&active.work, PendingWork::Measurement(_))
            && active.attempts_started >= self.policy.measurement_attempts_per_slot
        {
            let node_address = self.handoff_active_measurement()?;
            return Ok(ChannelEvent::MeasurementAttemptsExhausted { node_address });
        }
        let node_address = active.work.node_address();
        if matches!(
            &active.work,
            PendingWork::TimeSync(_) | PendingWork::Telemetry { .. }
        ) {
            self.active = None;
        }
        Ok(ChannelEvent::WireAttemptFailed { node_address })
    }

    pub fn receive(
        &mut self,
        now: MonotonicMicros,
        frame: &Frame,
    ) -> Result<ChannelEvent, ChannelAgentError> {
        match self.transactions.receive(now, frame)? {
            ResponseDisposition::Unrelated => return Ok(ChannelEvent::UnrelatedResponse),
            ResponseDisposition::Late => return Ok(ChannelEvent::LateResponse),
            ResponseDisposition::Unsolicited => return Ok(ChannelEvent::UnsolicitedResponse),
            ResponseDisposition::Accepted => {}
        }
        let active = self
            .active
            .as_mut()
            .ok_or(ChannelAgentError::InvariantViolation)?;
        match &mut active.work {
            PendingWork::Handshake(handshake) => {
                let node_address = handshake.requirements().node_address;
                match handshake.phase() {
                    HandshakePhase::NeedIdentity => handshake.accept_identity(frame.payload())?,
                    HandshakePhase::NeedCapabilities => {
                        handshake.accept_capabilities(frame.payload())?
                    }
                    HandshakePhase::Ready => return Err(ChannelAgentError::InvariantViolation),
                }
                if handshake.phase() == HandshakePhase::Ready {
                    let identity = handshake
                        .identity()
                        .ok_or(ChannelAgentError::InvariantViolation)?;
                    let capabilities = handshake
                        .capabilities()
                        .ok_or(ChannelAgentError::InvariantViolation)?;
                    self.active = None;
                    Ok(ChannelEvent::HandshakeComplete {
                        node_address,
                        identity,
                        capabilities,
                    })
                } else {
                    active.retry_not_before = now;
                    Ok(ChannelEvent::HandshakeProgress {
                        node_address,
                        phase: handshake.phase(),
                    })
                }
            }
            PendingWork::StreamState { node_address } => {
                let node_address = *node_address;
                match decode_stream_state_success(frame.payload()) {
                    Ok(state) => {
                        self.active = None;
                        Ok(ChannelEvent::StreamState {
                            node_address,
                            state,
                        })
                    }
                    Err(MeasurementWireError::RemoteError(ResponseStatus::Busy)) => {
                        active.retry_not_before = now
                            .checked_add(self.policy.retry_delay)
                            .ok_or(ChannelAgentError::TimeOverflow)?;
                        Ok(ChannelEvent::StreamStateNotAccepted { node_address })
                    }
                    Err(error) => {
                        self.active = None;
                        Err(error.into())
                    }
                }
            }
            PendingWork::Lifecycle(operation) => {
                match operation.accept_response(frame.payload())? {
                    LifecycleOutcome::NotAccepted => {
                        active.retry_not_before = now
                            .checked_add(self.policy.retry_delay)
                            .ok_or(ChannelAgentError::TimeOverflow)?;
                        Ok(ChannelEvent::LifecycleNotAccepted {
                            node_address: operation.node_address(),
                            operation_id: operation.command().operation_id,
                        })
                    }
                    LifecycleOutcome::InProgress(result) => {
                        active.retry_not_before = now
                            .checked_add(self.policy.retry_delay)
                            .ok_or(ChannelAgentError::TimeOverflow)?;
                        Ok(ChannelEvent::LifecycleProgress {
                            node_address: operation.node_address(),
                            operation_id: result.operation_id,
                            state: result.state,
                        })
                    }
                    LifecycleOutcome::Complete(result) => {
                        let node_address = operation.node_address();
                        self.active = None;
                        Ok(ChannelEvent::LifecycleComplete {
                            node_address,
                            result,
                        })
                    }
                }
            }
            PendingWork::Configuration(operation) => {
                let node_address = operation.node_address();
                let operation_id = operation.request().operation_id();
                match operation.accept_response(frame.payload())? {
                    ConfigurationOutcome::NotAccepted => {
                        active.retry_not_before = now
                            .checked_add(self.policy.retry_delay)
                            .ok_or(ChannelAgentError::TimeOverflow)?;
                        Ok(ChannelEvent::ConfigurationNotAccepted {
                            node_address,
                            operation_id,
                        })
                    }
                    outcome => {
                        self.active = None;
                        Ok(ChannelEvent::ConfigurationComplete {
                            node_address,
                            outcome,
                        })
                    }
                }
            }
            PendingWork::Commissioning(operation) => {
                match operation.accept_response(frame.payload())? {
                    CommissioningOutcome::NotAccepted => {
                        let operation_id = operation
                            .request()
                            .operation_id()
                            .ok_or(ChannelAgentError::InvariantViolation)?;
                        active.retry_not_before = now
                            .checked_add(self.policy.retry_delay)
                            .ok_or(ChannelAgentError::TimeOverflow)?;
                        Ok(ChannelEvent::CommissioningNotAccepted { operation_id })
                    }
                    outcome => {
                        self.active = None;
                        Ok(ChannelEvent::CommissioningComplete { outcome })
                    }
                }
            }
            PendingWork::Measurement(transfer) => {
                let node_address = transfer.node_address();
                let outcome = transfer.accept_response(frame.payload())?;
                if outcome == MeasurementOutcome::NotAccepted {
                    active.retry_not_before = now
                        .checked_add(self.policy.retry_delay)
                        .ok_or(ChannelAgentError::TimeOverflow)?;
                    if active.attempts_started >= self.policy.measurement_attempts_per_slot {
                        let node_address = self.handoff_active_measurement()?;
                        return Ok(ChannelEvent::MeasurementAttemptsExhausted { node_address });
                    }
                    return Ok(ChannelEvent::MeasurementNotAccepted { node_address });
                }
                let event = match outcome {
                    MeasurementOutcome::NotAccepted => unreachable!(),
                    MeasurementOutcome::NoData(state) => ChannelEvent::MeasurementNoData {
                        node_address,
                        state,
                    },
                    MeasurementOutcome::PersistenceRequired(kind) => {
                        ChannelEvent::MeasurementPersistenceRequired { node_address, kind }
                    }
                    MeasurementOutcome::RecoveryRequired(reason) => {
                        ChannelEvent::MeasurementRecoveryRequired {
                            node_address,
                            reason,
                        }
                    }
                };
                let completed = self
                    .active
                    .take()
                    .ok_or(ChannelAgentError::InvariantViolation)?;
                let PendingWork::Measurement(transfer) = completed.work else {
                    return Err(ChannelAgentError::InvariantViolation);
                };
                if self.completed_measurement.replace(*transfer).is_some() {
                    return Err(ChannelAgentError::InvariantViolation);
                }
                Ok(event)
            }
            PendingWork::TimeSync(attempt) => {
                let node_address = attempt.node_address;
                let expected_boot_epoch = attempt.expected_boot_epoch;
                let host_send_us = attempt.host_send_us;
                let decoded = match decode_time_response(frame.payload()) {
                    Ok(value) => value,
                    Err(TimeWireError::Remote(_)) => {
                        self.active = None;
                        return Ok(ChannelEvent::TimeSyncNotAccepted { node_address });
                    }
                    Err(error) => {
                        self.active = None;
                        return Err(error.into());
                    }
                };
                if decoded.boot_epoch != expected_boot_epoch {
                    self.active = None;
                    return Err(ChannelAgentError::TimeBootEpochMismatch);
                }
                let Some(host_send_us) = host_send_us else {
                    self.active = None;
                    return Err(ChannelAgentError::TimeRequestNotTransmitted);
                };
                let observation = match ClockObservation::new(
                    decoded.boot_epoch,
                    host_send_us,
                    decoded.node_receive_us,
                    decoded.node_send_us,
                    now.get(),
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        self.active = None;
                        return Err(error.into());
                    }
                };
                self.active = None;
                Ok(ChannelEvent::TimeObservation {
                    node_address,
                    observation,
                })
            }
            PendingWork::Telemetry { node_address } => {
                let node_address = *node_address;
                match decode_telemetry_snapshot(frame.payload()) {
                    Ok(snapshot) => {
                        self.active = None;
                        Ok(ChannelEvent::TelemetrySnapshot {
                            node_address,
                            snapshot,
                        })
                    }
                    Err(TelemetryWireError::Remote(_)) => {
                        self.active = None;
                        Ok(ChannelEvent::TelemetryNotAccepted { node_address })
                    }
                    Err(error) => {
                        self.active = None;
                        Err(error.into())
                    }
                }
            }
        }
    }

    pub fn queued_work_items(&self) -> usize {
        self.pending.len()
            + usize::from(self.active.is_some())
            + usize::from(self.completed_measurement.is_some())
    }

    pub fn take_completed_measurement(&mut self) -> Option<MeasurementTransfer> {
        self.completed_measurement.take()
    }

    pub fn completed_measurement_node(&self) -> Option<u8> {
        self.completed_measurement
            .as_ref()
            .map(MeasurementTransfer::node_address)
    }

    /// Abandons only explicitly identified control work. An active wire
    /// attempt must first finish, time out and leave late-response quarantine;
    /// otherwise removing its logical owner could let a delayed response be
    /// mistaken for later work. The caller should keep stepping the endpoint
    /// and retry after `QuarantineEnded`.
    pub fn abandon_control_work(
        &mut self,
        key: ControlWorkKey,
    ) -> Result<AbandonedControlWork, ChannelAgentError> {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.work.matches_control_key(key))
        {
            if self.outbound.is_some() || !self.transactions.is_idle() {
                return Err(ChannelAgentError::ControlAttemptInFlight(key));
            }
            self.active = None;
            return Ok(AbandonedControlWork::Active(key));
        }

        let Some(position) = self
            .pending
            .iter()
            .position(|work| work.matches_control_key(key))
        else {
            return Err(ChannelAgentError::ControlWorkNotFound(key));
        };
        self.pending.remove(position);
        Ok(AbandonedControlWork::Queued(key))
    }

    fn handoff_active_measurement(&mut self) -> Result<u8, ChannelAgentError> {
        let active = self
            .active
            .take()
            .ok_or(ChannelAgentError::InvariantViolation)?;
        let PendingWork::Measurement(transfer) = active.work else {
            return Err(ChannelAgentError::InvariantViolation);
        };
        let node_address = transfer.node_address();
        if self.completed_measurement.replace(*transfer).is_some() {
            return Err(ChannelAgentError::InvariantViolation);
        }
        Ok(node_address)
    }

    fn ensure_queue_capacity(&self) -> Result<(), ChannelAgentError> {
        if self.queued_work_items() >= self.policy.max_pending_work_items {
            Err(ChannelAgentError::QueueFull)
        } else {
            Ok(())
        }
    }

    fn allocate_operation_id(&mut self) -> Result<u32, ChannelAgentError> {
        for _ in 0..self.policy.max_pending_work_items.saturating_add(1) {
            let candidate = self.next_operation_id;
            self.next_operation_id = self.next_operation_id.wrapping_add(1);
            if self.next_operation_id == 0 {
                self.next_operation_id = 1;
            }
            if candidate != 0 && !self.operation_id_in_use(candidate) {
                return Ok(candidate);
            }
        }
        Err(ChannelAgentError::OperationIdExhausted)
    }

    fn operation_id_in_use(&self, operation_id: u32) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| work_uses_operation_id(&active.work, operation_id))
            || self
                .pending
                .iter()
                .any(|work| work_uses_operation_id(work, operation_id))
    }
}

fn work_uses_operation_id(work: &PendingWork, operation_id: u32) -> bool {
    match work {
        PendingWork::Lifecycle(operation) => operation.command().operation_id == operation_id,
        PendingWork::Configuration(operation) => {
            operation.request().operation_id() == Some(operation_id)
        }
        PendingWork::Commissioning(operation) => {
            operation.request().operation_id() == Some(operation_id)
        }
        _ => false,
    }
}

#[derive(Debug)]
pub enum ChannelAgentError {
    ZeroQueueCapacity,
    ZeroMeasurementAttemptBudget,
    QueueFull,
    OperationIdExhausted,
    OutboundNotTaken,
    TimeOverflow,
    InvariantViolation,
    Transaction(TransactionError),
    Handshake(HandshakeError),
    Lifecycle(LifecycleError),
    Configuration(ConfigurationError),
    Commissioning(CommissioningError),
    Measurement(MeasurementTransferError),
    StreamStateWire(MeasurementWireError),
    TimeWire(TimeWireError),
    TelemetryWire(TelemetryWireError),
    TimeObservation(ClockObservationError),
    Frame(sensor_protocol::FrameError),
    InvalidTimeNodeAddress(u8),
    InvalidTelemetryNodeAddress(u8),
    InvalidStreamStateNodeAddress(u8),
    ZeroTimeBootEpoch,
    ControlAttemptInFlight(ControlWorkKey),
    ControlWorkNotFound(ControlWorkKey),
    TimeBootEpochMismatch,
    TimeRequestNotTransmitted,
}

impl From<TransactionError> for ChannelAgentError {
    fn from(value: TransactionError) -> Self {
        Self::Transaction(value)
    }
}
impl From<LifecycleError> for ChannelAgentError {
    fn from(value: LifecycleError) -> Self {
        Self::Lifecycle(value)
    }
}
impl From<ConfigurationError> for ChannelAgentError {
    fn from(value: ConfigurationError) -> Self {
        Self::Configuration(value)
    }
}
impl From<CommissioningError> for ChannelAgentError {
    fn from(value: CommissioningError) -> Self {
        Self::Commissioning(value)
    }
}
impl From<HandshakeError> for ChannelAgentError {
    fn from(value: HandshakeError) -> Self {
        Self::Handshake(value)
    }
}
impl From<MeasurementTransferError> for ChannelAgentError {
    fn from(value: MeasurementTransferError) -> Self {
        Self::Measurement(value)
    }
}
impl From<MeasurementWireError> for ChannelAgentError {
    fn from(value: MeasurementWireError) -> Self {
        Self::StreamStateWire(value)
    }
}
impl From<TimeWireError> for ChannelAgentError {
    fn from(value: TimeWireError) -> Self {
        Self::TimeWire(value)
    }
}
impl From<TelemetryWireError> for ChannelAgentError {
    fn from(value: TelemetryWireError) -> Self {
        Self::TelemetryWire(value)
    }
}
impl From<ClockObservationError> for ChannelAgentError {
    fn from(value: ClockObservationError) -> Self {
        Self::TimeObservation(value)
    }
}
impl From<sensor_protocol::FrameError> for ChannelAgentError {
    fn from(value: sensor_protocol::FrameError) -> Self {
        Self::Frame(value)
    }
}

impl fmt::Display for ChannelAgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "channel agent error: {self:?}")
    }
}

impl std::error::Error for ChannelAgentError {}

#[derive(Debug)]
pub struct RejectedMeasurement {
    pub error: ChannelAgentError,
    pub transfer: MeasurementTransfer,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_session::{DurableReceipt, NodeSession};
    use sensor_core::{BootEpoch, HardwareId, SampleAxes, SampleAxis, SequenceCursor};
    use sensor_protocol::{
        encode_lifecycle_response, encode_pull_response, encode_stream_state_success,
        encode_time_success, FirmwareVersion, FrameRole, PullResponse, ResponseStatus,
        RuntimeResultCode, RuntimeState, SampleChunk, SampleChunkHeader, SensorStatusCode,
        ServiceId, StreamState, TimeExchangeResponse, TimingEvidence, ADXL355_TEMPERATURE_FIELD,
        CAPABILITIES_SCHEMA_VERSION, FEATURE_SAMPLE_JOURNAL, GET_CAPABILITIES_MESSAGE_ID,
        GET_IDENTITY_MESSAGE_ID, HOST_ADDRESS, LOCAL_PROTOCOL_VERSION, MAX_PAYLOAD_BYTES,
        TELEMETRY_SCHEMA_VERSION, TELEMETRY_SNAPSHOT_RESPONSE_BYTES, TIMING_VALID,
    };

    const HARDWARE_ID: HardwareId = HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);

    fn duration(value: u64) -> DurationMicros {
        DurationMicros::new(value).unwrap()
    }

    fn policy() -> ChannelPolicy {
        ChannelPolicy {
            transaction_timeout: duration(100),
            retry_delay: duration(10),
            measurement_attempts_per_slot: 2,
            max_pending_work_items: 4,
        }
    }

    fn take_transmit(agent: &mut ChannelAgent, now: u64) -> Frame {
        let frame = agent.take_outbound().expect("expected outbound frame");
        agent.transmit_completed(MonotonicMicros::new(now)).unwrap();
        frame
    }

    #[test]
    fn time_sync_uses_physical_host_boundaries_and_validates_boot_epoch() {
        let mut agent = ChannelAgent::new(10, 100, duration(50), policy()).unwrap();
        let epoch = BootEpoch::new(77);
        agent.submit_time_sync(7, epoch).unwrap();
        assert_eq!(
            agent.poll(MonotonicMicros::new(90)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let request = take_transmit(&mut agent, 100);
        assert_eq!(request.service, TIME_SERVICE_ID);
        assert_eq!(request.message, OBSERVE_CLOCK_MESSAGE_ID);
        assert!(request.payload().is_empty());

        let payload = encode_time_success(TimeExchangeResponse {
            boot_epoch: epoch,
            node_receive_us: 1_000,
            node_send_us: 1_010,
        })
        .unwrap();
        let event = agent
            .receive(
                MonotonicMicros::new(140),
                &wire_response(&request, &payload),
            )
            .unwrap();
        let ChannelEvent::TimeObservation {
            node_address,
            observation,
        } = event
        else {
            panic!("expected clock observation");
        };
        assert_eq!(node_address, 7);
        assert_eq!(observation.host_send_us, 100);
        assert_eq!(observation.host_receive_us, 140);
        assert_eq!(observation.node_receive_us, 1_000);
        assert_eq!(observation.node_send_us, 1_010);
        assert_eq!(observation.path_delay_us(), 30);
        assert_eq!(observation.uncertainty_us(), 15);
        assert_eq!(agent.queued_work_items(), 0);
    }

    #[test]
    fn rejected_time_sync_yields_channel_without_leaving_active_work() {
        let mut agent = ChannelAgent::new(10, 100, duration(50), policy()).unwrap();
        agent.submit_time_sync(7, BootEpoch::new(77)).unwrap();
        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let request = take_transmit(&mut agent, 1);
        assert_eq!(
            agent
                .receive(
                    MonotonicMicros::new(2),
                    &wire_response(&request, &[ResponseStatus::Busy as u8]),
                )
                .unwrap(),
            ChannelEvent::TimeSyncNotAccepted { node_address: 7 }
        );
        assert_eq!(agent.queued_work_items(), 0);
    }

    #[test]
    fn telemetry_uses_the_shared_transaction_owner_and_returns_validated_snapshot() {
        let mut agent = ChannelAgent::new(10, 100, duration(50), policy()).unwrap();
        agent.submit_telemetry(7).unwrap();
        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let request = take_transmit(&mut agent, 1);
        assert_eq!(request.destination, 7);
        assert_eq!(request.service, TELEMETRY_SERVICE_ID);
        assert_eq!(request.message, GET_TELEMETRY_SNAPSHOT_MESSAGE_ID);
        assert!(request.payload().is_empty());

        let mut payload = [0u8; TELEMETRY_SNAPSHOT_RESPONSE_BYTES];
        payload[0] = ResponseStatus::Ok as u8;
        payload[1] = TELEMETRY_SCHEMA_VERSION;
        payload[2..10].copy_from_slice(&123_456u64.to_le_bytes());
        payload[10..12].copy_from_slice(&ADXL355_TEMPERATURE_FIELD.to_le_bytes());
        payload[12..14].copy_from_slice(&1885u16.to_le_bytes());
        let event = agent
            .receive(MonotonicMicros::new(2), &wire_response(&request, &payload))
            .unwrap();
        let ChannelEvent::TelemetrySnapshot {
            node_address,
            snapshot,
        } = event
        else {
            panic!("expected telemetry snapshot");
        };
        assert_eq!(node_address, 7);
        assert_eq!(snapshot.captured_device_time_us, 123_456);
        assert_eq!(snapshot.adxl355_temperature.get(), 1885);
        assert_eq!(agent.queued_work_items(), 0);
    }

    #[test]
    fn telemetry_busy_and_timeout_release_the_transaction_owner() {
        let mut agent = ChannelAgent::new(10, 100, duration(50), policy()).unwrap();
        agent.submit_telemetry(7).unwrap();
        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let request = take_transmit(&mut agent, 0);
        assert_eq!(
            agent
                .receive(
                    MonotonicMicros::new(1),
                    &wire_response(&request, &[ResponseStatus::Busy as u8]),
                )
                .unwrap(),
            ChannelEvent::TelemetryNotAccepted { node_address: 7 }
        );
        assert_eq!(agent.queued_work_items(), 0);

        agent.submit_telemetry(7).unwrap();
        assert_eq!(
            agent.poll(MonotonicMicros::new(2)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let _request = take_transmit(&mut agent, 2);
        assert_eq!(
            agent.poll(MonotonicMicros::new(103)).unwrap(),
            ChannelEvent::TelemetryTimedOut { node_address: 7 }
        );
        assert_eq!(agent.queued_work_items(), 0);
    }

    fn wire_response(request: &Frame, payload: &[u8]) -> Frame {
        Frame::new(
            FrameRole::Response,
            HOST_ADDRESS,
            request.destination,
            request.service,
            request.message,
            request.transaction_id,
            payload,
        )
        .unwrap()
    }

    fn response(request: &Frame, result: RuntimeResult) -> Frame {
        wire_response(request, &encode_lifecycle_response(result).unwrap())
    }

    fn requirements(node_address: u8) -> HandshakeRequirements {
        HandshakeRequirements::recording_node(HARDWARE_ID, node_address, 2, FEATURE_SAMPLE_JOURNAL)
    }

    fn identity(node_address: u8) -> NodeIdentity {
        NodeIdentity {
            hardware_id: HARDWARE_ID,
            boot_epoch: BootEpoch::new(77),
            node_address,
            board_revision: 2,
            firmware: FirmwareVersion {
                major: 0,
                minor: 5,
                patch: 0,
            },
            protocol: LOCAL_PROTOCOL_VERSION,
        }
    }

    fn capabilities() -> NodeCapabilities {
        NodeCapabilities {
            schema_version: CAPABILITIES_SCHEMA_VERSION,
            service_flags: ServiceId::Identity.capability_bit()
                | ServiceId::Measurement.capability_bit()
                | ServiceId::Lifecycle.capability_bit(),
            feature_flags: FEATURE_SAMPLE_JOURNAL,
            max_payload_bytes: MAX_PAYLOAD_BYTES as u16,
            journal_capacity_samples: 4096,
            output_rate_mask: sensor_protocol::IMPLEMENTED_OUTPUT_RATE_MASK,
            max_sample_chunk_samples: 48,
        }
    }

    fn stream_state() -> StreamState {
        StreamState {
            boot_epoch: BootEpoch::new(77),
            retained_from: SequenceCursor::new(40),
            produced: SequenceCursor::new(140),
            released: SequenceCursor::new(80),
            capacity_samples: 4096,
            overwritten_samples: 40,
        }
    }

    #[test]
    fn stream_state_shares_fifo_and_transaction_owner_with_lifecycle() {
        let mut agent = ChannelAgent::new(10, 100, duration(50), policy()).unwrap();
        agent.submit_stream_state(7).unwrap();
        agent
            .submit_lifecycle(8, RuntimeOperation::Start, ObservabilityProfile::Production)
            .unwrap();

        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let request = take_transmit(&mut agent, 0);
        assert_eq!(request.destination, 7);
        assert_eq!(request.service, ServiceId::Measurement as u8);
        assert_eq!(request.message, GET_STREAM_STATE_MESSAGE_ID);
        assert!(request.payload().is_empty());

        assert_eq!(
            agent
                .receive(
                    MonotonicMicros::new(1),
                    &wire_response(
                        &request,
                        &encode_stream_state_success(stream_state()).unwrap(),
                    ),
                )
                .unwrap(),
            ChannelEvent::StreamState {
                node_address: 7,
                state: stream_state(),
            }
        );

        assert_eq!(
            agent.poll(MonotonicMicros::new(2)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let lifecycle = take_transmit(&mut agent, 2);
        assert_eq!(lifecycle.destination, 8);
        assert_ne!(request.transaction_id, lifecycle.transaction_id);
    }

    #[test]
    fn stream_state_timeout_retries_with_a_new_transaction_id() {
        let mut agent = ChannelAgent::new(10, 100, duration(50), policy()).unwrap();
        agent.submit_stream_state(7).unwrap();

        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let first = take_transmit(&mut agent, 0);
        assert_eq!(
            agent.poll(MonotonicMicros::new(101)).unwrap(),
            ChannelEvent::StreamStateTimedOut { node_address: 7 }
        );
        assert_eq!(
            agent.poll(MonotonicMicros::new(150)).unwrap(),
            ChannelEvent::QuarantineEnded
        );
        assert_eq!(
            agent.poll(MonotonicMicros::new(151)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let second = take_transmit(&mut agent, 151);
        assert_ne!(first.transaction_id, second.transaction_id);
    }

    #[test]
    fn stream_state_busy_is_bounded_by_the_callers_deadline_and_keeps_ownership() {
        let mut agent = ChannelAgent::new(10, 100, duration(50), policy()).unwrap();
        agent.submit_stream_state(7).unwrap();
        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let first = take_transmit(&mut agent, 0);
        assert_eq!(
            agent
                .receive(
                    MonotonicMicros::new(1),
                    &wire_response(&first, &[ResponseStatus::Busy as u8]),
                )
                .unwrap(),
            ChannelEvent::StreamStateNotAccepted { node_address: 7 }
        );
        assert_eq!(agent.queued_work_items(), 1);
        assert_eq!(
            agent.poll(MonotonicMicros::new(11)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let second = take_transmit(&mut agent, 11);
        assert_ne!(first.transaction_id, second.transaction_id);
    }

    #[test]
    fn handshake_and_lifecycle_share_one_transaction_owner() {
        let mut agent = ChannelAgent::new(10, 100, duration(50), policy()).unwrap();
        agent.submit_handshake(requirements(7)).unwrap();
        agent
            .submit_lifecycle(7, RuntimeOperation::Start, ObservabilityProfile::Production)
            .unwrap();

        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let identity_request = take_transmit(&mut agent, 0);
        assert_eq!(identity_request.message, GET_IDENTITY_MESSAGE_ID);
        assert!(identity_request.payload().is_empty());
        assert_eq!(
            agent
                .receive(
                    MonotonicMicros::new(1),
                    &wire_response(&identity_request, &identity(7).encode_success().unwrap(),),
                )
                .unwrap(),
            ChannelEvent::HandshakeProgress {
                node_address: 7,
                phase: HandshakePhase::NeedCapabilities,
            }
        );

        assert_eq!(
            agent.poll(MonotonicMicros::new(2)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let capabilities_request = take_transmit(&mut agent, 2);
        assert_eq!(capabilities_request.message, GET_CAPABILITIES_MESSAGE_ID);
        assert!(matches!(
            agent
                .receive(
                    MonotonicMicros::new(3),
                    &wire_response(
                        &capabilities_request,
                        &capabilities().encode_success().unwrap(),
                    ),
                )
                .unwrap(),
            ChannelEvent::HandshakeComplete {
                node_address: 7,
                ..
            }
        ));

        assert_eq!(
            agent.poll(MonotonicMicros::new(4)).unwrap(),
            ChannelEvent::TransmitReady
        );
        assert_eq!(take_transmit(&mut agent, 4).destination, 7);
    }

    #[test]
    fn busy_is_reported_without_fabricating_a_runtime_state() {
        let mut agent = ChannelAgent::new(10, 100, duration(50), policy()).unwrap();
        let operation_id = agent
            .submit_lifecycle(7, RuntimeOperation::Stop, ObservabilityProfile::Production)
            .unwrap();
        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let request = take_transmit(&mut agent, 0);
        assert_eq!(
            agent
                .receive(
                    MonotonicMicros::new(1),
                    &wire_response(&request, &[ResponseStatus::Busy as u8]),
                )
                .unwrap(),
            ChannelEvent::LifecycleNotAccepted {
                node_address: 7,
                operation_id,
            }
        );
    }

    #[test]
    fn measurement_handoff_blocks_next_pull_until_durable_confirmation() {
        let mut agent = ChannelAgent::new(10, 100, duration(50), policy()).unwrap();
        let session = NodeSession::recovered(
            HARDWARE_ID,
            BootEpoch::new(9),
            SequenceCursor::ORIGIN,
            SequenceCursor::ORIGIN,
        )
        .unwrap();
        agent
            .submit_measurement(MeasurementTransfer::new(7, 16, session).unwrap())
            .unwrap();

        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let request = take_transmit(&mut agent, 0);
        assert_eq!(request.service, ServiceId::Measurement as u8);
        let axis = SampleAxis::new(17).unwrap();
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
            samples: vec![SampleAxes::new(axis, axis, axis)],
        };
        let mut payload = [0; MAX_PAYLOAD_BYTES];
        let len =
            encode_pull_response(&PullResponse::SampleChunk(chunk.clone()), &mut payload).unwrap();
        assert_eq!(
            agent
                .receive(
                    MonotonicMicros::new(1),
                    &wire_response(&request, &payload[..len]),
                )
                .unwrap(),
            ChannelEvent::MeasurementPersistenceRequired {
                node_address: 7,
                kind: PersistenceKind::SampleChunk { sample_count: 1 },
            }
        );
        assert_eq!(
            agent.poll(MonotonicMicros::new(2)).unwrap(),
            ChannelEvent::MeasurementHandoffPending { node_address: 7 }
        );

        let mut transfer = agent.take_completed_measurement().unwrap();
        assert_eq!(transfer.session().pending_wire_chunk(), Some(&chunk));
        transfer
            .confirm_durable(DurableReceipt {
                hardware_id: HARDWARE_ID,
                boot_epoch: BootEpoch::new(9),
                end: SequenceCursor::new(1),
            })
            .unwrap();
        agent.submit_measurement(transfer).unwrap();
        assert_eq!(
            agent.poll(MonotonicMicros::new(3)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let next = take_transmit(&mut agent, 3);
        assert_eq!(
            u64::from_le_bytes(next.payload()[8..16].try_into().unwrap()),
            1
        );
        assert_eq!(
            u64::from_le_bytes(next.payload()[16..24].try_into().unwrap()),
            1
        );
    }

    #[test]
    fn rotates_queued_nodes_without_overlapping_transactions() {
        let mut agent = ChannelAgent::new(10, 100, duration(50), policy()).unwrap();
        let first_id = agent
            .submit_lifecycle(7, RuntimeOperation::Start, ObservabilityProfile::Production)
            .unwrap();
        let second_id = agent
            .submit_lifecycle(8, RuntimeOperation::Start, ObservabilityProfile::Production)
            .unwrap();

        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let first = take_transmit(&mut agent, 0);
        assert_eq!(first.destination, 7);
        let first_response = response(
            &first,
            RuntimeResult {
                operation_id: first_id,
                code: RuntimeResultCode::Ok,
                state: RuntimeState::Recording,
                observability: ObservabilityProfile::Production,
                sensor_status: SensorStatusCode::Ok,
            },
        );
        assert!(matches!(
            agent
                .receive(MonotonicMicros::new(1), &first_response)
                .unwrap(),
            ChannelEvent::LifecycleComplete {
                node_address: 7,
                ..
            }
        ));

        assert_eq!(
            agent.poll(MonotonicMicros::new(2)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let second = take_transmit(&mut agent, 2);
        assert_eq!(second.destination, 8);
        assert_ne!(first.transaction_id, second.transaction_id);
        assert_eq!(
            u32::from_le_bytes(second.payload().try_into().unwrap()),
            second_id
        );
    }

    #[test]
    fn timeout_quarantine_retries_same_operation_with_new_transaction() {
        let mut agent = ChannelAgent::new(20, 200, duration(50), policy()).unwrap();
        let operation_id = agent
            .submit_lifecycle(7, RuntimeOperation::Stop, ObservabilityProfile::Production)
            .unwrap();
        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let first = take_transmit(&mut agent, 0);
        assert!(matches!(agent.poll(MonotonicMicros::new(101)).unwrap(),
            ChannelEvent::LifecycleTimedOut { operation_id: id, .. } if id == operation_id));
        assert_eq!(
            agent.poll(MonotonicMicros::new(150)).unwrap(),
            ChannelEvent::QuarantineEnded
        );
        assert_eq!(
            agent.poll(MonotonicMicros::new(151)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let second = take_transmit(&mut agent, 151);
        assert_ne!(first.transaction_id, second.transaction_id);
        assert_eq!(first.payload(), second.payload());
    }

    #[test]
    fn timed_out_lifecycle_can_only_be_abandoned_after_quarantine() {
        let mut agent = ChannelAgent::new(20, 200, duration(50), policy()).unwrap();
        let operation_id = agent
            .submit_lifecycle(7, RuntimeOperation::Stop, ObservabilityProfile::Production)
            .unwrap();
        let key = ControlWorkKey::Lifecycle {
            node_address: 7,
            operation_id,
        };

        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let request = take_transmit(&mut agent, 0);
        assert!(matches!(
            agent.abandon_control_work(key),
            Err(ChannelAgentError::ControlAttemptInFlight(found)) if found == key
        ));
        assert!(matches!(
            agent.poll(MonotonicMicros::new(101)).unwrap(),
            ChannelEvent::LifecycleTimedOut { .. }
        ));
        assert!(matches!(
            agent.abandon_control_work(key),
            Err(ChannelAgentError::ControlAttemptInFlight(found)) if found == key
        ));

        let late = response(
            &request,
            RuntimeResult {
                operation_id,
                code: RuntimeResultCode::Ok,
                state: RuntimeState::Idle,
                observability: ObservabilityProfile::Production,
                sensor_status: SensorStatusCode::Ok,
            },
        );
        assert_eq!(
            agent.receive(MonotonicMicros::new(120), &late).unwrap(),
            ChannelEvent::LateResponse
        );
        assert_eq!(
            agent.poll(MonotonicMicros::new(150)).unwrap(),
            ChannelEvent::QuarantineEnded
        );
        assert_eq!(
            agent.abandon_control_work(key).unwrap(),
            AbandonedControlWork::Active(key)
        );
        assert_eq!(agent.queued_work_items(), 0);
        assert_eq!(
            agent.poll(MonotonicMicros::new(151)).unwrap(),
            ChannelEvent::Idle
        );
    }

    #[test]
    fn queued_lifecycle_can_be_removed_without_disturbing_fifo_neighbors() {
        let mut agent = ChannelAgent::new(20, 200, duration(50), policy()).unwrap();
        let first_id = agent
            .submit_lifecycle(
                7,
                RuntimeOperation::GetState,
                ObservabilityProfile::Production,
            )
            .unwrap();
        let removed_id = agent
            .submit_lifecycle(8, RuntimeOperation::Stop, ObservabilityProfile::Production)
            .unwrap();
        let third_id = agent
            .submit_lifecycle(
                9,
                RuntimeOperation::RestartNode,
                ObservabilityProfile::Production,
            )
            .unwrap();
        let key = ControlWorkKey::Lifecycle {
            node_address: 8,
            operation_id: removed_id,
        };

        assert_eq!(
            agent.abandon_control_work(key).unwrap(),
            AbandonedControlWork::Queued(key)
        );
        assert_eq!(agent.queued_work_items(), 2);

        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let first = take_transmit(&mut agent, 0);
        assert_eq!(first.destination, 7);
        assert_eq!(
            u32::from_le_bytes(first.payload().try_into().unwrap()),
            first_id
        );
        let first_response = response(
            &first,
            RuntimeResult {
                operation_id: first_id,
                code: RuntimeResultCode::Ok,
                state: RuntimeState::Idle,
                observability: ObservabilityProfile::Production,
                sensor_status: SensorStatusCode::Ok,
            },
        );
        agent
            .receive(MonotonicMicros::new(1), &first_response)
            .unwrap();

        assert_eq!(
            agent.poll(MonotonicMicros::new(2)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let third = take_transmit(&mut agent, 2);
        assert_eq!(third.destination, 9);
        assert_eq!(
            u32::from_le_bytes(third.payload().try_into().unwrap()),
            third_id
        );
    }

    #[test]
    fn uncertain_transmit_retries_same_operation_without_starting_response_timeout() {
        let mut agent = ChannelAgent::new(20, 200, duration(50), policy()).unwrap();
        agent
            .submit_lifecycle(7, RuntimeOperation::Stop, ObservabilityProfile::Production)
            .unwrap();
        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let first = agent.take_outbound().unwrap();
        assert_eq!(
            agent.transmit_failed(MonotonicMicros::new(1)).unwrap(),
            ChannelEvent::WireAttemptFailed { node_address: 7 }
        );
        assert_eq!(
            agent.poll(MonotonicMicros::new(10)).unwrap(),
            ChannelEvent::Idle
        );
        assert_eq!(
            agent.poll(MonotonicMicros::new(11)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let second = take_transmit(&mut agent, 11);
        assert_ne!(first.transaction_id, second.transaction_id);
        assert_eq!(first.payload(), second.payload());
    }

    #[test]
    fn measurement_yields_after_the_small_error_budget_is_exhausted() {
        let mut agent = ChannelAgent::new(20, 200, duration(50), policy()).unwrap();
        let session = NodeSession::recovered(
            HARDWARE_ID,
            BootEpoch::new(9),
            SequenceCursor::ORIGIN,
            SequenceCursor::ORIGIN,
        )
        .unwrap();
        agent
            .submit_measurement(MeasurementTransfer::new(7, 16, session).unwrap())
            .unwrap();

        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        agent.take_outbound().unwrap();
        assert_eq!(
            agent.transmit_failed(MonotonicMicros::new(1)).unwrap(),
            ChannelEvent::WireAttemptFailed { node_address: 7 }
        );
        assert_eq!(
            agent.poll(MonotonicMicros::new(11)).unwrap(),
            ChannelEvent::TransmitReady
        );
        agent.take_outbound().unwrap();
        assert_eq!(
            agent.transmit_failed(MonotonicMicros::new(12)).unwrap(),
            ChannelEvent::MeasurementAttemptsExhausted { node_address: 7 }
        );
        let transfer = agent.take_completed_measurement().unwrap();
        assert_eq!(
            transfer.session().phase(),
            crate::node_session::SessionPhase::Ready
        );
        assert_eq!(agent.queued_work_items(), 0);
    }

    #[test]
    fn queue_capacity_is_enforced() {
        let mut limited = policy();
        limited.max_pending_work_items = 1;
        let mut agent = ChannelAgent::new(1, 1, duration(50), limited).unwrap();
        agent
            .submit_lifecycle(7, RuntimeOperation::Start, ObservabilityProfile::Production)
            .unwrap();
        assert!(matches!(
            agent.submit_lifecycle(8, RuntimeOperation::Start, ObservabilityProfile::Production),
            Err(ChannelAgentError::QueueFull)
        ));
    }

    fn measurement_configuration() -> sensor_protocol::MeasurementConfiguration {
        sensor_protocol::MeasurementConfiguration {
            output_rate: sensor_protocol::OutputRateCode::Hz125,
            range_g: 2,
            high_pass_corner: 0,
            filter_profile: sensor_protocol::DecimationFilterProfile::Balanced,
            fifo_watermark: 30,
            offset_x: 0,
            offset_y: 0,
            offset_z: 0,
        }
    }

    #[test]
    fn configuration_uses_the_same_transaction_owner_and_operation_id_retry() {
        let mut agent = ChannelAgent::new(10, 70, duration(50), policy()).unwrap();
        let desired = measurement_configuration();
        let operation_id = agent.submit_apply_configuration(7, 3, desired).unwrap();
        assert_eq!(operation_id, 70);
        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let first = take_transmit(&mut agent, 1);
        assert_eq!(first.service, sensor_protocol::CONFIGURATION_SERVICE_ID);
        assert_eq!(first.message, sensor_protocol::APPLY_CONFIG_MESSAGE_ID);
        assert_eq!(
            u32::from_le_bytes(first.payload()[..4].try_into().unwrap()),
            70
        );

        assert_eq!(
            agent
                .receive(
                    MonotonicMicros::new(2),
                    &wire_response(&first, &[ResponseStatus::Busy as u8]),
                )
                .unwrap(),
            ChannelEvent::ConfigurationNotAccepted {
                node_address: 7,
                operation_id: Some(70),
            }
        );
        assert_eq!(
            agent.poll(MonotonicMicros::new(12)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let second = take_transmit(&mut agent, 13);
        assert_ne!(first.transaction_id, second.transaction_id);
        assert_eq!(first.payload(), second.payload());

        let result = sensor_protocol::ConfigOperationResult {
            operation_id: 70,
            code: sensor_protocol::ConfigOperationCode::Applied,
            observed: sensor_protocol::ObservedConfiguration {
                revision: 4,
                desired,
                sensor_odr: sensor_protocol::Adxl355Odr::Hz250,
                decimation_factor: 2,
            },
            effective_sample_seq: 99,
        };
        assert_eq!(
            agent
                .receive(
                    MonotonicMicros::new(14),
                    &wire_response(
                        &second,
                        &sensor_protocol::encode_config_operation_response(result).unwrap()
                    ),
                )
                .unwrap(),
            ChannelEvent::ConfigurationComplete {
                node_address: 7,
                outcome: ConfigurationOutcome::Applied(result),
            }
        );
        assert_eq!(agent.queued_work_items(), 0);
    }

    #[test]
    fn commissioning_broadcast_accepts_only_the_reserved_response_source() {
        let mut agent = ChannelAgent::new(10, 70, duration(50), policy()).unwrap();
        let hardware_id = HARDWARE_ID;
        let nonce = 0x0102_0304_0506_0708;
        let slot_index = sensor_protocol::commissioning_slot(hardware_id, nonce, 16).unwrap();
        agent
            .submit_commissioning_probe(sensor_protocol::ProbeSlotRequest {
                round_nonce: nonce,
                slot_count: 16,
                slot_index,
            })
            .unwrap();
        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let request = take_transmit(&mut agent, 1);
        assert_eq!(request.destination, sensor_protocol::BROADCAST_ADDRESS);
        assert_eq!(request.service, sensor_protocol::COMMISSIONING_SERVICE_ID);

        let identity = sensor_protocol::CommissioningIdentity {
            round_nonce: nonce,
            hardware_id,
            board_revision: 2,
            firmware: FirmwareVersion {
                major: 0,
                minor: 5,
                patch: 7,
            },
            protocol: LOCAL_PROTOCOL_VERSION,
        };
        let response = Frame::new(
            FrameRole::Response,
            HOST_ADDRESS,
            sensor_protocol::COMMISSIONING_ADDRESS,
            request.service,
            request.message,
            request.transaction_id,
            &sensor_protocol::encode_probe_slot_response(identity).unwrap(),
        )
        .unwrap();
        assert_eq!(
            agent.receive(MonotonicMicros::new(2), &response).unwrap(),
            ChannelEvent::CommissioningComplete {
                outcome: CommissioningOutcome::Discovered(identity),
            }
        );
    }

    #[test]
    fn zero_measurement_attempt_budget_is_rejected() {
        let mut invalid = policy();
        invalid.measurement_attempts_per_slot = 0;
        assert!(matches!(
            ChannelAgent::new(1, 1, duration(50), invalid),
            Err(ChannelAgentError::ZeroMeasurementAttemptBudget)
        ));
    }
}
