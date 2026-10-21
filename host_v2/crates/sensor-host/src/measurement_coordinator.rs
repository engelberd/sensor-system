use core::fmt;
use std::collections::VecDeque;

use sensor_core::SequenceCursor;

use crate::bounded_writer::{
    PersistenceEnvelope, PersistenceEnvelopeError, PersistenceSubmitter, SubmitErrorKind,
    WriteCompletion, WriteJobId,
};
use crate::channel_agent::{ChannelAgent, ChannelAgentError, ChannelEvent};
use crate::channel_runtime::{
    ChannelDispatch, ChannelRuntime, ChannelRuntimeError, MeasurementDispatch, MeasurementProgress,
    NodeScheduleConfig, TelemetryDispatch, TelemetryScheduleConfig, TimeSyncDispatch,
    TimeSyncScheduleConfig,
};
use crate::measurement_persistence::{persist_pending, DurableMeasurementSink, PersistenceError};
use crate::measurement_transfer::{MeasurementTransfer, MeasurementTransferError, PersistenceKind};
use crate::node_metrics::{NodeMetricsPublisher, NodeMetricsReader};
use crate::node_session::{NodeSession, SessionPhase};
use crate::persistence_executor::WriteRecoveryRange;
use crate::stream_recovery::{
    reconcile_existing_capture, CaptureRecoveryEvidence, ExpectedStreamState, RecoveryContinuity,
    StreamRecoveryError,
};
use crate::telemetry_persistence::{
    TelemetryObservationSubmitErrorKind, TelemetryObservationSubmitter,
};
use crate::telemetry_repository::{TelemetryObservationRecord, TelemetryObservationRecordError};
use crate::time_persistence::{
    TimeObservationRecord, TimeObservationRecordError, TimeObservationSubmitErrorKind,
    TimeObservationSubmitter,
};
use crate::transaction::MonotonicMicros;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeasurementSettlement {
    Persisted {
        node_address: u8,
        progress: MeasurementProgress,
        kind: PersistenceKind,
    },
    NoData {
        node_address: u8,
        progress: MeasurementProgress,
    },
    RetryScheduled {
        node_address: u8,
    },
    RecoveryRequired {
        node_address: u8,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeSyncSettlement {
    Submitted {
        node_address: u8,
    },
    RetryScheduled {
        node_address: u8,
    },
    Backpressured {
        node_address: u8,
        reason: TimeObservationSubmitErrorKind,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetrySettlement {
    Submitted {
        node_address: u8,
    },
    RetryScheduled {
        node_address: u8,
    },
    Backpressured {
        node_address: u8,
        reason: TelemetryObservationSubmitErrorKind,
    },
}

struct ManagedSession {
    node_address: u8,
    session: Option<NodeSession>,
    metrics: NodeMetricsPublisher,
    metrics_reader: NodeMetricsReader,
}

struct PendingCompletion {
    dispatch: MeasurementDispatch,
    transfer: MeasurementTransfer,
    envelope: Option<PersistenceEnvelope>,
}

struct InFlightWrite {
    job_id: WriteJobId,
    dispatch: MeasurementDispatch,
    transfer: MeasurementTransfer,
}

/// Coordinates the selected service work for one channel without owning wire,
/// scheduling or persistence policy. Measurement keeps its dedicated session
/// and durable-settlement path; TimeSync only shares arbitration and dispatch.
pub struct ChannelWorkCoordinator {
    channel_id: u8,
    runtime: ChannelRuntime,
    sessions: Vec<ManagedSession>,
    active_dispatch: Option<MeasurementDispatch>,
    active_time_sync: Option<TimeSyncDispatch>,
    active_telemetry: Option<TelemetryDispatch>,
    pending_completion: Option<PendingCompletion>,
    in_flight_writes: Vec<InFlightWrite>,
    quiescing: bool,
    shutdown_drain_nodes: Option<VecDeque<u8>>,
}

impl ChannelWorkCoordinator {
    pub fn new(runtime: ChannelRuntime, channel_id: u8) -> Self {
        let capacity = runtime.node_capacity();
        Self {
            channel_id,
            runtime,
            sessions: Vec::with_capacity(capacity),
            active_dispatch: None,
            active_time_sync: None,
            active_telemetry: None,
            pending_completion: None,
            in_flight_writes: Vec::with_capacity(capacity),
            quiescing: false,
            shutdown_drain_nodes: None,
        }
    }

    pub fn configure_time_sync(
        &mut self,
        node_address: u8,
        config: TimeSyncScheduleConfig,
        first_deadline: MonotonicMicros,
    ) -> Result<(), MeasurementCoordinatorError> {
        self.runtime
            .configure_time_sync(node_address, config, first_deadline)
            .map_err(MeasurementCoordinatorError::Runtime)
    }

    pub fn configure_telemetry(
        &mut self,
        node_address: u8,
        config: TelemetryScheduleConfig,
        first_deadline: MonotonicMicros,
    ) -> Result<(), MeasurementCoordinatorError> {
        self.runtime
            .configure_telemetry(node_address, config, first_deadline)
            .map_err(MeasurementCoordinatorError::Runtime)
    }

    pub fn dispatch_next_work(
        &mut self,
        agent: &mut ChannelAgent,
        now: MonotonicMicros,
    ) -> Result<Option<ChannelDispatch>, MeasurementCoordinatorError> {
        if self.has_active_dispatch() || self.pending_completion.is_some() {
            return Ok(None);
        }
        if let Some(nodes) = &self.shutdown_drain_nodes {
            let Some(node_address) = nodes.front().copied() else {
                return Ok(None);
            };
            let Some(dispatch) = self
                .runtime
                .select_shutdown_measurement(node_address, now)
                .map_err(MeasurementCoordinatorError::Runtime)?
            else {
                return Ok(None);
            };
            if let Err(error) = self.submit_measurement(agent, dispatch) {
                self.runtime
                    .cancel_measurement(dispatch)
                    .map_err(MeasurementCoordinatorError::Runtime)?;
                return Err(error);
            }
            self.active_dispatch = Some(dispatch);
            return Ok(Some(ChannelDispatch::Measurement(dispatch)));
        }
        if self.quiescing {
            return Ok(None);
        }
        let Some(dispatch) = self
            .runtime
            .select_work(now)
            .map_err(MeasurementCoordinatorError::Runtime)?
        else {
            return Ok(None);
        };
        match dispatch {
            ChannelDispatch::Measurement(measurement) => {
                if let Err(error) = self.submit_measurement(agent, measurement) {
                    self.runtime
                        .cancel_measurement(measurement)
                        .map_err(MeasurementCoordinatorError::Runtime)?;
                    return Err(error);
                }
                self.active_dispatch = Some(measurement);
            }
            ChannelDispatch::TimeSync(sync) => {
                let boot_epoch = self
                    .managed_session(sync.node_address)?
                    .session
                    .as_ref()
                    .ok_or(MeasurementCoordinatorError::SessionUnavailable(
                        sync.node_address,
                    ))?
                    .boot_epoch();
                if let Err(error) = agent.submit_time_sync(sync.node_address, boot_epoch) {
                    self.runtime
                        .cancel_time_sync(sync)
                        .map_err(MeasurementCoordinatorError::Runtime)?;
                    return Err(MeasurementCoordinatorError::Agent(error));
                }
                self.active_time_sync = Some(sync);
            }
            ChannelDispatch::Telemetry(telemetry) => {
                if let Err(error) = agent.submit_telemetry(telemetry.node_address) {
                    self.runtime
                        .cancel_telemetry(telemetry)
                        .map_err(MeasurementCoordinatorError::Runtime)?;
                    return Err(MeasurementCoordinatorError::Agent(error));
                }
                self.active_telemetry = Some(telemetry);
            }
        }
        Ok(Some(dispatch))
    }

    pub fn register_node(
        &mut self,
        config: NodeScheduleConfig,
        session: NodeSession,
        now: MonotonicMicros,
    ) -> Result<(), MeasurementCoordinatorError> {
        if self
            .sessions
            .iter()
            .any(|managed| managed.node_address == config.node_address)
        {
            return Err(MeasurementCoordinatorError::DuplicateNode(
                config.node_address,
            ));
        }
        let recovered_cursor = session.cursors().received();
        self.runtime
            .register_node(config, recovered_cursor, now)
            .map_err(MeasurementCoordinatorError::Runtime)?;
        let (metrics, metrics_reader) =
            NodeMetricsPublisher::new(self.channel_id, config.node_address, session.cursors());
        self.sessions.push(ManagedSession {
            node_address: config.node_address,
            session: Some(session),
            metrics,
            metrics_reader,
        });
        Ok(())
    }

    pub fn reconfigure_node(
        &mut self,
        config: NodeScheduleConfig,
        now: MonotonicMicros,
    ) -> Result<(), MeasurementCoordinatorError> {
        if !self.quiescing
            || self.has_active_dispatch()
            || self.pending_completion.is_some()
            || !self.in_flight_writes.is_empty()
            || self.shutdown_drain_nodes.is_some()
        {
            return Err(MeasurementCoordinatorError::ReconfigurationNotQuiescent);
        }
        self.runtime
            .reconfigure_node(config, now)
            .map_err(MeasurementCoordinatorError::Runtime)
    }

    pub fn node_metrics_readers(&self) -> Vec<NodeMetricsReader> {
        self.sessions
            .iter()
            .map(|managed| managed.metrics_reader.clone())
            .collect()
    }

    pub fn observe_configuration(
        &self,
        node_address: u8,
        configuration: sensor_protocol::ObservedConfiguration,
    ) -> Result<(), MeasurementCoordinatorError> {
        let managed = self
            .sessions
            .iter()
            .find(|managed| managed.node_address == node_address)
            .ok_or(MeasurementCoordinatorError::UnknownNode(node_address))?;
        managed.metrics.observe_configuration(configuration);
        Ok(())
    }

    pub fn dispatch_next(
        &mut self,
        agent: &mut ChannelAgent,
        now: MonotonicMicros,
    ) -> Result<Option<MeasurementDispatch>, MeasurementCoordinatorError> {
        if self.quiescing || self.active_dispatch.is_some() || self.pending_completion.is_some() {
            return Ok(None);
        }
        let Some(dispatch) = self
            .runtime
            .select_measurement(now)
            .map_err(MeasurementCoordinatorError::Runtime)?
        else {
            return Ok(None);
        };
        let managed = self.managed_session_mut(dispatch.node_address)?;
        let session =
            managed
                .session
                .take()
                .ok_or(MeasurementCoordinatorError::SessionUnavailable(
                    dispatch.node_address,
                ))?;
        let transfer =
            MeasurementTransfer::new(dispatch.node_address, dispatch.max_samples, session)
                .map_err(MeasurementCoordinatorError::Transfer)?;
        if let Err(rejected) = agent.try_submit_measurement(transfer) {
            let rejected = *rejected;
            let session = rejected.transfer.into_session();
            self.managed_session_mut(dispatch.node_address)?.session = Some(session);
            self.runtime
                .cancel_measurement(dispatch)
                .map_err(MeasurementCoordinatorError::Runtime)?;
            return Err(MeasurementCoordinatorError::Agent(rejected.error));
        }
        self.active_dispatch = Some(dispatch);
        Ok(Some(dispatch))
    }

    pub fn settle_time_sync_event<T>(
        &mut self,
        event: &ChannelEvent,
        observations: &mut T,
        now: MonotonicMicros,
    ) -> Result<Option<TimeSyncSettlement>, MeasurementCoordinatorError>
    where
        T: TimeObservationSubmitter,
    {
        let Some(dispatch) = self.active_time_sync else {
            return Ok(None);
        };
        let node_address = match event {
            ChannelEvent::TimeObservation { node_address, .. }
            | ChannelEvent::TimeSyncTimedOut { node_address }
            | ChannelEvent::TimeSyncNotAccepted { node_address }
            | ChannelEvent::TimeSyncInvalidObservation { node_address, .. }
            | ChannelEvent::WireAttemptFailed { node_address } => *node_address,
            _ => return Ok(None),
        };
        if node_address != dispatch.node_address {
            return Err(MeasurementCoordinatorError::WrongCompletedNode);
        }
        let settlement = if let ChannelEvent::TimeObservation { observation, .. } = event {
            let hardware_id = self
                .managed_session(node_address)?
                .session
                .as_ref()
                .ok_or(MeasurementCoordinatorError::SessionUnavailable(
                    node_address,
                ))?
                .hardware_id();
            let record = TimeObservationRecord::new(hardware_id, node_address, *observation)
                .map_err(MeasurementCoordinatorError::TimeRecord)?;
            match observations.try_submit(record) {
                Ok(()) => {
                    self.runtime
                        .complete_time_sync(dispatch, now)
                        .map_err(MeasurementCoordinatorError::Runtime)?;
                    self.managed_session(node_address)?
                        .metrics
                        .observe_time_sync(now);
                    TimeSyncSettlement::Submitted { node_address }
                }
                Err(error) => {
                    let reason = error.kind();
                    self.runtime
                        .fail_time_sync(dispatch, now)
                        .map_err(MeasurementCoordinatorError::Runtime)?;
                    TimeSyncSettlement::Backpressured {
                        node_address,
                        reason,
                    }
                }
            }
        } else {
            self.runtime
                .fail_time_sync(dispatch, now)
                .map_err(MeasurementCoordinatorError::Runtime)?;
            TimeSyncSettlement::RetryScheduled { node_address }
        };
        self.active_time_sync = None;
        Ok(Some(settlement))
    }

    pub fn settle_telemetry_event<T>(
        &mut self,
        event: &ChannelEvent,
        observations: &mut T,
        now: MonotonicMicros,
    ) -> Result<Option<TelemetrySettlement>, MeasurementCoordinatorError>
    where
        T: TelemetryObservationSubmitter + ?Sized,
    {
        let Some(dispatch) = self.active_telemetry else {
            return Ok(None);
        };
        let node_address = match event {
            ChannelEvent::TelemetrySnapshot { node_address, .. }
            | ChannelEvent::TelemetryTimedOut { node_address }
            | ChannelEvent::TelemetryNotAccepted { node_address }
            | ChannelEvent::WireAttemptFailed { node_address } => *node_address,
            _ => return Ok(None),
        };
        if node_address != dispatch.node_address {
            return Err(MeasurementCoordinatorError::WrongCompletedNode);
        }
        let settlement = if let ChannelEvent::TelemetrySnapshot { snapshot, .. } = event {
            let session = self.managed_session(node_address)?.session.as_ref().ok_or(
                MeasurementCoordinatorError::SessionUnavailable(node_address),
            )?;
            let record = TelemetryObservationRecord::new(
                session.hardware_id(),
                node_address,
                session.boot_epoch(),
                *snapshot,
            )
            .map_err(MeasurementCoordinatorError::TelemetryRecord)?;
            match observations.try_submit(record) {
                Ok(()) => {
                    self.managed_session(node_address)?
                        .metrics
                        .observe_telemetry(*snapshot, now);
                    self.runtime
                        .complete_telemetry(dispatch, now)
                        .map_err(MeasurementCoordinatorError::Runtime)?;
                    TelemetrySettlement::Submitted { node_address }
                }
                Err(error) => {
                    let reason = error.kind();
                    self.runtime
                        .fail_telemetry(dispatch, now)
                        .map_err(MeasurementCoordinatorError::Runtime)?;
                    TelemetrySettlement::Backpressured {
                        node_address,
                        reason,
                    }
                }
            }
        } else {
            self.runtime
                .fail_telemetry(dispatch, now)
                .map_err(MeasurementCoordinatorError::Runtime)?;
            TelemetrySettlement::RetryScheduled { node_address }
        };
        self.active_telemetry = None;
        Ok(Some(settlement))
    }

    /// Completes the active slot. A sink failure keeps the transfer owned here
    /// with its pending record intact; calling this method again retries only
    /// persistence and never repeats a successful wire response.
    pub fn settle_completed<S>(
        &mut self,
        agent: &mut ChannelAgent,
        sink: &mut S,
        now: MonotonicMicros,
    ) -> Result<MeasurementSettlement, MeasurementSettlementError<S::Error>>
    where
        S: DurableMeasurementSink,
    {
        if self.pending_completion.is_none() {
            let dispatch = self
                .active_dispatch
                .ok_or(MeasurementSettlementError::Coordinator(
                    MeasurementCoordinatorError::NoActiveDispatch,
                ))?;
            let transfer = agent.take_completed_measurement().ok_or(
                MeasurementSettlementError::Coordinator(
                    MeasurementCoordinatorError::CompletedTransferUnavailable,
                ),
            )?;
            if transfer.node_address() != dispatch.node_address {
                return Err(MeasurementSettlementError::Coordinator(
                    MeasurementCoordinatorError::WrongCompletedNode,
                ));
            }
            self.pending_completion = Some(PendingCompletion {
                dispatch,
                transfer,
                envelope: None,
            });
        }

        let mut pending =
            self.pending_completion
                .take()
                .ok_or(MeasurementSettlementError::Coordinator(
                    MeasurementCoordinatorError::InvariantViolation,
                ))?;
        if pending.envelope.is_some() {
            self.pending_completion = Some(pending);
            return Err(MeasurementSettlementError::Coordinator(
                MeasurementCoordinatorError::PersistenceAlreadyHandedOff,
            ));
        }
        let phase_before_settlement = pending.transfer.session().phase();
        let persistence_kind = pending.transfer.persistence_kind();
        if matches!(
            phase_before_settlement,
            SessionPhase::PersistingChunk
                | SessionPhase::PersistingGap
                | SessionPhase::PersistingContinuityBreak
        ) {
            if let Err(error) = persist_pending(&mut pending.transfer, sink) {
                self.pending_completion = Some(pending);
                return Err(MeasurementSettlementError::Persistence(error));
            }
        }

        let node_address = pending.dispatch.node_address;
        let settlement = match pending.transfer.session().phase() {
            SessionPhase::Ready => {
                if let Some(produced) = pending.transfer.observed_produced() {
                    let progress = MeasurementProgress {
                        received: pending.transfer.session().cursors().received(),
                        produced,
                    };
                    match self
                        .runtime
                        .complete_measurement(pending.dispatch, progress, now)
                    {
                        Ok(()) => {
                            if phase_before_settlement == SessionPhase::Ready {
                                MeasurementSettlement::NoData {
                                    node_address,
                                    progress,
                                }
                            } else {
                                MeasurementSettlement::Persisted {
                                    node_address,
                                    progress,
                                    kind: persistence_kind.ok_or(
                                        MeasurementSettlementError::Coordinator(
                                            MeasurementCoordinatorError::InvariantViolation,
                                        ),
                                    )?,
                                }
                            }
                        }
                        Err(ChannelRuntimeError::StreamRegressed) => {
                            MeasurementSettlement::RecoveryRequired { node_address }
                        }
                        Err(error) => {
                            self.pending_completion = Some(pending);
                            return Err(MeasurementSettlementError::Coordinator(
                                MeasurementCoordinatorError::Runtime(error),
                            ));
                        }
                    }
                } else {
                    if let Err(error) = self.runtime.fail_measurement(pending.dispatch, now) {
                        self.pending_completion = Some(pending);
                        return Err(MeasurementSettlementError::Coordinator(
                            MeasurementCoordinatorError::Runtime(error),
                        ));
                    }
                    MeasurementSettlement::RetryScheduled { node_address }
                }
            }
            SessionPhase::RecoveryRequired => {
                if let Err(error) = self.runtime.require_recovery(pending.dispatch) {
                    self.pending_completion = Some(pending);
                    return Err(MeasurementSettlementError::Coordinator(
                        MeasurementCoordinatorError::Runtime(error),
                    ));
                }
                MeasurementSettlement::RecoveryRequired { node_address }
            }
            _ => {
                self.pending_completion = Some(pending);
                return Err(MeasurementSettlementError::Coordinator(
                    MeasurementCoordinatorError::UnexpectedSessionPhase,
                ));
            }
        };

        let produced = settlement_progress(settlement).map(|progress| progress.produced.get());
        let session = pending.transfer.into_session();
        let managed = self
            .managed_session_mut(node_address)
            .map_err(MeasurementSettlementError::Coordinator)?;
        if let MeasurementSettlement::Persisted { kind, .. } = settlement {
            managed.metrics.observe_persistence(kind);
        }
        managed.metrics.observe_session(&session, produced, now);
        managed.session = Some(session);
        self.active_dispatch = None;
        self.observe_shutdown_settlement(settlement)
            .map_err(MeasurementSettlementError::Coordinator)?;
        Ok(settlement)
    }

    /// Moves a completed measurement record to bounded storage ownership.
    /// The physical channel is released after enqueue, while this node remains
    /// excluded from scheduling until its correlated durable completion.
    pub fn handoff_completed<Q>(
        &mut self,
        agent: &mut ChannelAgent,
        writer: &mut Q,
    ) -> Result<WriteJobId, MeasurementHandoffError>
    where
        Q: PersistenceSubmitter,
    {
        if self.pending_completion.is_none() {
            let dispatch = self
                .active_dispatch
                .ok_or(MeasurementHandoffError::Coordinator(
                    MeasurementCoordinatorError::NoActiveDispatch,
                ))?;
            let transfer =
                agent
                    .take_completed_measurement()
                    .ok_or(MeasurementHandoffError::Coordinator(
                        MeasurementCoordinatorError::CompletedTransferUnavailable,
                    ))?;
            if transfer.node_address() != dispatch.node_address {
                return Err(MeasurementHandoffError::Coordinator(
                    MeasurementCoordinatorError::WrongCompletedNode,
                ));
            }
            self.pending_completion = Some(PendingCompletion {
                dispatch,
                transfer,
                envelope: None,
            });
        }

        let mut pending =
            self.pending_completion
                .take()
                .ok_or(MeasurementHandoffError::Coordinator(
                    MeasurementCoordinatorError::InvariantViolation,
                ))?;
        if !matches!(
            pending.transfer.session().phase(),
            SessionPhase::PersistingChunk
                | SessionPhase::PersistingGap
                | SessionPhase::PersistingContinuityBreak
        ) {
            self.pending_completion = Some(pending);
            return Err(MeasurementHandoffError::Coordinator(
                MeasurementCoordinatorError::UnexpectedSessionPhase,
            ));
        }
        if pending.envelope.is_none() {
            let record = pending
                .transfer
                .take_pending_persistence()
                .map_err(MeasurementHandoffError::Transfer)?;
            pending.envelope = Some(
                PersistenceEnvelope::from_pending(record)
                    .map_err(MeasurementHandoffError::Envelope)?,
            );
        }
        let envelope = pending
            .envelope
            .take()
            .ok_or(MeasurementHandoffError::Coordinator(
                MeasurementCoordinatorError::InvariantViolation,
            ))?;
        let job_id = match writer.submit_record(envelope) {
            Ok(job_id) => job_id,
            Err(error) => {
                let kind = error.kind();
                pending.envelope = Some(error.into_envelope());
                self.pending_completion = Some(pending);
                return Err(MeasurementHandoffError::Submit(kind));
            }
        };
        self.in_flight_writes.push(InFlightWrite {
            job_id,
            dispatch: pending.dispatch,
            transfer: pending.transfer,
        });
        if let Err(error) = self.runtime.park_measurement(pending.dispatch) {
            return Err(MeasurementHandoffError::Coordinator(
                MeasurementCoordinatorError::Runtime(error),
            ));
        }
        self.active_dispatch = None;
        Ok(job_id)
    }

    pub fn settle_write_completion<E>(
        &mut self,
        completion: WriteCompletion<E>,
        now: MonotonicMicros,
    ) -> Result<MeasurementSettlement, MeasurementWriteSettlementError<E>> {
        let index = self
            .in_flight_writes
            .iter()
            .position(|write| write.job_id == completion.id)
            .ok_or(MeasurementWriteSettlementError::UnknownJob(completion.id))?;
        let receipt = match completion.result {
            Ok(receipt) => receipt,
            Err(error) => {
                return Err(MeasurementWriteSettlementError::Storage {
                    job_id: completion.id,
                    error,
                });
            }
        };
        self.in_flight_writes[index]
            .transfer
            .confirm_durable(receipt)
            .map_err(MeasurementWriteSettlementError::Transfer)?;
        self.finish_durable_write(index, now)
            .map_err(MeasurementWriteSettlementError::Coordinator)
    }

    /// Retries only coordinator settlement after a durable completion was
    /// accepted but local scheduling state could not yet be advanced.
    pub fn retry_durable_settlement(
        &mut self,
        job_id: WriteJobId,
        now: MonotonicMicros,
    ) -> Result<MeasurementSettlement, MeasurementCoordinatorError> {
        let index = self
            .in_flight_writes
            .iter()
            .position(|write| write.job_id == job_id)
            .ok_or(MeasurementCoordinatorError::UnknownWriteJob(job_id))?;
        if self.in_flight_writes[index].transfer.session().phase() != SessionPhase::Ready {
            return Err(MeasurementCoordinatorError::DurabilityNotConfirmed(job_id));
        }
        self.finish_durable_write(index, now)
    }

    fn finish_durable_write(
        &mut self,
        index: usize,
        now: MonotonicMicros,
    ) -> Result<MeasurementSettlement, MeasurementCoordinatorError> {
        let write = &self.in_flight_writes[index];
        let node_address = write.dispatch.node_address;
        let produced = write
            .transfer
            .observed_produced()
            .ok_or(MeasurementCoordinatorError::MissingObservedProgress)?;
        let progress = MeasurementProgress {
            received: write.transfer.session().cursors().received(),
            produced,
        };
        let persistence_kind = write
            .transfer
            .persistence_kind()
            .ok_or(MeasurementCoordinatorError::InvariantViolation)?;
        let settlement =
            match self
                .runtime
                .complete_parked_measurement(write.dispatch, progress, now)
            {
                Ok(()) => MeasurementSettlement::Persisted {
                    node_address,
                    progress,
                    kind: persistence_kind,
                },
                Err(ChannelRuntimeError::StreamRegressed) => {
                    MeasurementSettlement::RecoveryRequired { node_address }
                }
                Err(error) => return Err(MeasurementCoordinatorError::Runtime(error)),
            };
        let write = self.in_flight_writes.remove(index);
        let session = write.transfer.into_session();
        let managed = self.managed_session_mut(node_address)?;
        if let MeasurementSettlement::Persisted { kind, .. } = settlement {
            managed.metrics.observe_persistence(kind);
        }
        managed
            .metrics
            .observe_session(&session, Some(produced.get()), now);
        managed.session = Some(session);
        Ok(settlement)
    }

    pub fn recover_node(
        &mut self,
        node_address: u8,
        session: NodeSession,
        now: MonotonicMicros,
    ) -> Result<(), MeasurementCoordinatorError> {
        self.runtime
            .recover_node(node_address, session.cursors().received(), now)
            .map_err(MeasurementCoordinatorError::Runtime)?;
        let managed = self.managed_session_mut(node_address)?;
        managed.metrics.observe_session(&session, None, now);
        managed.session = Some(session);
        Ok(())
    }

    /// Rebuilds one quiesced session from fresh node evidence and the cursor
    /// already proven durable by Capture. This is the same fail-closed rule as
    /// process startup; no cursor is advanced from volatile host state.
    pub fn reconcile_resume_stream(
        &mut self,
        node_address: u8,
        journal_capacity_samples: u32,
        stream: sensor_protocol::StreamState,
        now: MonotonicMicros,
    ) -> Result<RecoveryContinuity, MeasurementCoordinatorError> {
        if !self.is_quiescent() || self.shutdown_drain_nodes.is_some() {
            return Err(MeasurementCoordinatorError::ResumeNotReady);
        }
        let session = self.managed_session(node_address)?.session.as_ref().ok_or(
            MeasurementCoordinatorError::SessionUnavailable(node_address),
        )?;
        let expected = ExpectedStreamState {
            hardware_id: session.hardware_id(),
            boot_epoch: session.boot_epoch(),
            journal_capacity_samples,
        };
        let capture = CaptureRecoveryEvidence {
            hardware_id: session.hardware_id(),
            boot_epoch: session.boot_epoch(),
            durable_cursor: session.cursors().durable(),
        };
        let reconciled = reconcile_existing_capture(expected, capture, stream)
            .map_err(MeasurementCoordinatorError::StreamRecovery)?;
        let continuity = reconciled.continuity();
        let session = reconciled.into_session();
        self.runtime
            .resume_node(
                node_address,
                session.cursors().received(),
                stream.produced,
                now,
            )
            .map_err(MeasurementCoordinatorError::Runtime)?;
        let managed = self.managed_session_mut(node_address)?;
        managed
            .metrics
            .observe_session(&session, Some(stream.produced.get()), now);
        managed.session = Some(session);
        Ok(continuity)
    }

    /// Installs a session for a newly reconciled boot epoch. The durable
    /// cursor comes from the newly opened Capture WAL, never from volatile
    /// scheduler state.
    pub fn replace_resume_stream(
        &mut self,
        node_address: u8,
        expected: ExpectedStreamState,
        durable_cursor: SequenceCursor,
        stream: sensor_protocol::StreamState,
        now: MonotonicMicros,
    ) -> Result<RecoveryContinuity, MeasurementCoordinatorError> {
        if !self.is_quiescent() || self.shutdown_drain_nodes.is_some() {
            return Err(MeasurementCoordinatorError::ResumeNotReady);
        }
        let previous = self.managed_session(node_address)?.session.as_ref().ok_or(
            MeasurementCoordinatorError::SessionUnavailable(node_address),
        )?;
        if previous.hardware_id() != expected.hardware_id {
            return Err(MeasurementCoordinatorError::ResumeHardwareMismatch(
                node_address,
            ));
        }
        let reconciled = reconcile_existing_capture(
            expected,
            CaptureRecoveryEvidence {
                hardware_id: expected.hardware_id,
                boot_epoch: expected.boot_epoch,
                durable_cursor,
            },
            stream,
        )
        .map_err(MeasurementCoordinatorError::StreamRecovery)?;
        let continuity = reconciled.continuity();
        let session = reconciled.into_session();
        self.runtime
            .replace_quiesced_node(node_address, durable_cursor, stream.produced, now)
            .map_err(MeasurementCoordinatorError::Runtime)?;
        let managed = self.managed_session_mut(node_address)?;
        managed
            .metrics
            .observe_session(&session, Some(stream.produced.get()), now);
        managed.session = Some(session);
        Ok(continuity)
    }

    pub const fn runtime(&self) -> &ChannelRuntime {
        &self.runtime
    }

    pub const fn has_active_dispatch(&self) -> bool {
        self.active_dispatch.is_some()
            || self.active_time_sync.is_some()
            || self.active_telemetry.is_some()
    }

    pub const fn has_pending_completion(&self) -> bool {
        self.pending_completion.is_some()
    }

    /// Prevents all new Measurement and TimeSync dispatches. Work already
    /// owned by the wire or persistence path remains settleable.
    pub fn begin_quiesce(&mut self) {
        self.quiescing = true;
    }

    /// Begins the post-Stop drain. It is legal only after the initial barrier
    /// settled every operation that existed before acquisition was stopped.
    pub fn begin_shutdown_drain(&mut self) -> Result<(), MeasurementCoordinatorError> {
        if !self.is_quiescent() || self.shutdown_drain_nodes.is_some() {
            return Err(MeasurementCoordinatorError::ShutdownDrainNotReady);
        }
        self.shutdown_drain_nodes = Some(
            self.sessions
                .iter()
                .map(|session| session.node_address)
                .collect(),
        );
        self.quiescing = false;
        Ok(())
    }

    pub fn is_shutdown_draining(&self) -> bool {
        self.shutdown_drain_nodes.is_some()
    }

    pub fn is_shutdown_drain_complete(&self) -> bool {
        self.shutdown_drain_nodes
            .as_ref()
            .is_some_and(VecDeque::is_empty)
            && !self.has_active_dispatch()
            && self.pending_completion.is_none()
            && self.in_flight_writes.is_empty()
    }

    pub fn finish_shutdown_drain(&mut self) -> Result<(), MeasurementCoordinatorError> {
        if !self.is_shutdown_drain_complete() {
            return Err(MeasurementCoordinatorError::ShutdownDrainIncomplete);
        }
        self.shutdown_drain_nodes = None;
        self.quiescing = true;
        Ok(())
    }

    /// Cancels only the not-yet-dispatched tail of a service drain. Work
    /// already owned by wire or storage is still settled under quiescing.
    /// This is used when an explicit maintenance deadline expires; it never
    /// invents a durable cursor or releases node retention.
    pub fn abort_shutdown_drain(&mut self) -> Result<(), MeasurementCoordinatorError> {
        if self.shutdown_drain_nodes.take().is_none() {
            return Err(MeasurementCoordinatorError::ShutdownDrainNotActive);
        }
        self.quiescing = true;
        Ok(())
    }

    /// Re-enables normal scheduling after a completed service interval. The
    /// caller must first restore every node/session contract and leave no
    /// post-Stop drain active. This method changes only scheduler admission;
    /// it never starts a node or invents cursor recovery.
    pub fn resume_scheduling(&mut self) -> Result<(), MeasurementCoordinatorError> {
        if !self.is_quiescent() || self.shutdown_drain_nodes.is_some() {
            return Err(MeasurementCoordinatorError::ResumeNotReady);
        }
        self.quiescing = false;
        Ok(())
    }

    pub const fn is_quiescing(&self) -> bool {
        self.quiescing
    }

    pub fn is_quiescent(&self) -> bool {
        self.quiescing
            && !self.has_active_dispatch()
            && self.pending_completion.is_none()
            && self.in_flight_writes.is_empty()
    }

    pub fn pending_completion_node(&self) -> Option<u8> {
        self.pending_completion
            .as_ref()
            .map(|pending| pending.dispatch.node_address)
    }

    pub fn in_flight_write_count(&self) -> usize {
        self.in_flight_writes.len()
    }

    pub fn write_recovery_range(
        &self,
        job_id: WriteJobId,
    ) -> Result<WriteRecoveryRange, MeasurementCoordinatorError> {
        let write = self
            .in_flight_writes
            .iter()
            .find(|write| write.job_id == job_id)
            .ok_or(MeasurementCoordinatorError::UnknownWriteJob(job_id))?;
        let session = write.transfer.session();
        let receipt = session
            .pending_receipt()
            .ok_or(MeasurementCoordinatorError::MissingPendingReceipt(job_id))?;
        Ok(WriteRecoveryRange {
            hardware_id: receipt.hardware_id,
            boot_epoch: receipt.boot_epoch,
            start: session.cursors().durable(),
            end: receipt.end,
        })
    }

    fn managed_session_mut(
        &mut self,
        node_address: u8,
    ) -> Result<&mut ManagedSession, MeasurementCoordinatorError> {
        self.sessions
            .iter_mut()
            .find(|managed| managed.node_address == node_address)
            .ok_or(MeasurementCoordinatorError::UnknownNode(node_address))
    }

    fn observe_shutdown_settlement(
        &mut self,
        settlement: MeasurementSettlement,
    ) -> Result<(), MeasurementCoordinatorError> {
        let MeasurementSettlement::NoData { node_address, .. } = settlement else {
            return Ok(());
        };
        let Some(nodes) = &mut self.shutdown_drain_nodes else {
            return Ok(());
        };
        if nodes.front().copied() != Some(node_address) {
            return Err(MeasurementCoordinatorError::ShutdownDrainOrder);
        }
        nodes.pop_front();
        Ok(())
    }

    fn managed_session(
        &self,
        node_address: u8,
    ) -> Result<&ManagedSession, MeasurementCoordinatorError> {
        self.sessions
            .iter()
            .find(|managed| managed.node_address == node_address)
            .ok_or(MeasurementCoordinatorError::UnknownNode(node_address))
    }

    fn submit_measurement(
        &mut self,
        agent: &mut ChannelAgent,
        dispatch: MeasurementDispatch,
    ) -> Result<(), MeasurementCoordinatorError> {
        let managed = self.managed_session_mut(dispatch.node_address)?;
        let session =
            managed
                .session
                .take()
                .ok_or(MeasurementCoordinatorError::SessionUnavailable(
                    dispatch.node_address,
                ))?;
        let transfer =
            MeasurementTransfer::new(dispatch.node_address, dispatch.max_samples, session)
                .map_err(MeasurementCoordinatorError::Transfer)?;
        if let Err(rejected) = agent.try_submit_measurement(transfer) {
            let rejected = *rejected;
            let session = rejected.transfer.into_session();
            self.managed_session_mut(dispatch.node_address)?.session = Some(session);
            return Err(MeasurementCoordinatorError::Agent(rejected.error));
        }
        Ok(())
    }
}

/// Transitional name retained for callers that only compose Measurement.
/// New channel-level composition should use `ChannelWorkCoordinator`.
pub type MeasurementCoordinator = ChannelWorkCoordinator;

fn settlement_progress(settlement: MeasurementSettlement) -> Option<MeasurementProgress> {
    match settlement {
        MeasurementSettlement::Persisted { progress, .. }
        | MeasurementSettlement::NoData { progress, .. } => Some(progress),
        MeasurementSettlement::RetryScheduled { .. }
        | MeasurementSettlement::RecoveryRequired { .. } => None,
    }
}

#[derive(Debug)]
pub enum MeasurementSettlementError<E> {
    Coordinator(MeasurementCoordinatorError),
    Persistence(PersistenceError<E>),
}

impl<E> fmt::Display for MeasurementSettlementError<E>
where
    E: fmt::Display,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Coordinator(error) => write!(formatter, "{error}"),
            Self::Persistence(error) => write!(formatter, "{error}"),
        }
    }
}

impl<E> std::error::Error for MeasurementSettlementError<E> where E: std::error::Error + 'static {}

#[derive(Debug)]
pub enum MeasurementCoordinatorError {
    DuplicateNode(u8),
    UnknownNode(u8),
    SessionUnavailable(u8),
    NoActiveDispatch,
    CompletedTransferUnavailable,
    WrongCompletedNode,
    UnexpectedSessionPhase,
    InvariantViolation,
    PersistenceAlreadyHandedOff,
    MissingObservedProgress,
    UnknownWriteJob(WriteJobId),
    DurabilityNotConfirmed(WriteJobId),
    MissingPendingReceipt(WriteJobId),
    Runtime(ChannelRuntimeError),
    Agent(ChannelAgentError),
    Transfer(MeasurementTransferError),
    TimeRecord(TimeObservationRecordError),
    TelemetryRecord(TelemetryObservationRecordError),
    ShutdownDrainNotReady,
    ShutdownDrainNotActive,
    ShutdownDrainIncomplete,
    ShutdownDrainOrder,
    ResumeNotReady,
    ReconfigurationNotQuiescent,
    StreamRecovery(StreamRecoveryError),
    ResumeHardwareMismatch(u8),
}

impl fmt::Display for MeasurementCoordinatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "measurement coordinator error: {self:?}")
    }
}

impl std::error::Error for MeasurementCoordinatorError {}

#[derive(Debug)]
pub enum MeasurementHandoffError {
    Coordinator(MeasurementCoordinatorError),
    Transfer(MeasurementTransferError),
    Envelope(PersistenceEnvelopeError),
    Submit(SubmitErrorKind),
}

impl fmt::Display for MeasurementHandoffError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "measurement handoff error: {self:?}")
    }
}

impl std::error::Error for MeasurementHandoffError {}

#[derive(Debug)]
pub enum MeasurementWriteSettlementError<E> {
    UnknownJob(WriteJobId),
    Storage { job_id: WriteJobId, error: E },
    Transfer(MeasurementTransferError),
    Coordinator(MeasurementCoordinatorError),
}

impl<E> fmt::Display for MeasurementWriteSettlementError<E>
where
    E: fmt::Display,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownJob(job_id) => write!(formatter, "unknown write job {}", job_id.get()),
            Self::Storage { job_id, error } => {
                write!(formatter, "storage job {} failed: {error}", job_id.get())
            }
            Self::Transfer(error) => write!(formatter, "durable receipt failed: {error}"),
            Self::Coordinator(error) => write!(formatter, "{error}"),
        }
    }
}

impl<E> std::error::Error for MeasurementWriteSettlementError<E> where E: std::error::Error + 'static
{}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bounded_writer::BoundedPersistenceWorker;
    use sensor_core::{BootEpoch, HardwareId, SampleAxes, SampleAxis, SampleRate, SequenceCursor};
    use sensor_protocol::{
        encode_pull_response, encode_time_success, Adxl355TemperatureCode, Frame, FrameRole,
        ObservabilityProfile, PullResponse, SampleChunk, SampleChunkHeader, StreamState,
        TelemetrySnapshot, TimeExchangeResponse, TimingEvidence, HOST_ADDRESS, MAX_PAYLOAD_BYTES,
        TIMING_VALID,
    };

    use crate::channel_agent::{ChannelEvent, ChannelPolicy};
    use crate::channel_scheduler::{ChannelScheduler, ChannelSchedulerPolicy};
    use crate::transaction::DurationMicros;

    const HARDWARE_ID: HardwareId = HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);

    #[derive(Debug, Eq, PartialEq)]
    struct SinkError;

    impl fmt::Display for SinkError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("injected sink failure")
        }
    }

    impl std::error::Error for SinkError {}

    struct Sink {
        failures_remaining: usize,
        chunk_calls: usize,
    }

    impl DurableMeasurementSink for Sink {
        type Error = SinkError;

        fn persist_chunk(
            &mut self,
            _hardware_id: HardwareId,
            _chunk: &SampleChunk,
        ) -> Result<(), Self::Error> {
            self.chunk_calls += 1;
            if self.failures_remaining > 0 {
                self.failures_remaining -= 1;
                Err(SinkError)
            } else {
                Ok(())
            }
        }

        fn persist_gap(&mut self, _gap: sensor_core::Gap) -> Result<(), Self::Error> {
            Ok(())
        }

        fn persist_continuity_break(
            &mut self,
            _observed: sensor_core::ContinuityBreak,
        ) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    fn duration(value: u64) -> DurationMicros {
        DurationMicros::new(value).unwrap()
    }

    fn agent() -> ChannelAgent {
        ChannelAgent::new(
            10,
            20,
            duration(50),
            ChannelPolicy {
                transaction_timeout: duration(100),
                retry_delay: duration(10),
                measurement_attempts_per_slot: 2,
                max_pending_work_items: 1,
            },
        )
        .unwrap()
    }

    fn coordinator() -> MeasurementCoordinator {
        let runtime = ChannelRuntime::new(
            ChannelScheduler::new(ChannelSchedulerPolicy {
                overflow_guard: duration(250_000),
                max_slot: duration(50_000),
            }),
            ObservabilityProfile::Production,
            1,
            100,
        )
        .unwrap();
        let mut coordinator = MeasurementCoordinator::new(runtime, 1);
        coordinator
            .register_node(
                NodeScheduleConfig {
                    node_address: 7,
                    output_rate: SampleRate::HZ_125,
                    journal_capacity_samples: 2_048,
                    max_sample_chunk_samples: 48,
                    target_chunk_samples: 32,
                    measurement_slot: duration(50_000),
                    maximum_service_interval: duration(500_000),
                    retry_base_delay: duration(10_000),
                    retry_max_delay: duration(80_000),
                    retry_attempt_limit: 3,
                    offline_probe_interval: duration(2_000_000),
                },
                NodeSession::recovered(
                    HARDWARE_ID,
                    BootEpoch::new(9),
                    SequenceCursor::ORIGIN,
                    SequenceCursor::ORIGIN,
                )
                .unwrap(),
                MonotonicMicros::new(0),
            )
            .unwrap();
        coordinator
    }

    fn begin_wire_attempt(
        coordinator: &mut MeasurementCoordinator,
        agent: &mut ChannelAgent,
    ) -> Frame {
        coordinator
            .dispatch_next(agent, MonotonicMicros::new(0))
            .unwrap()
            .unwrap();
        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let request = agent.take_outbound().unwrap();
        agent.transmit_completed(MonotonicMicros::new(0)).unwrap();
        request
    }

    fn response(request: &Frame, payload: &[u8]) -> Frame {
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

    fn settle_initial_empty_measurement(
        coordinator: &mut MeasurementCoordinator,
        agent: &mut ChannelAgent,
    ) {
        assert!(matches!(
            coordinator
                .dispatch_next_work(agent, MonotonicMicros::new(0))
                .unwrap(),
            Some(ChannelDispatch::Measurement(_))
        ));
        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let request = agent.take_outbound().unwrap();
        agent.transmit_completed(MonotonicMicros::new(0)).unwrap();
        let state = StreamState {
            boot_epoch: BootEpoch::new(9),
            retained_from: SequenceCursor::ORIGIN,
            produced: SequenceCursor::ORIGIN,
            released: SequenceCursor::ORIGIN,
            capacity_samples: 2_048,
            overwritten_samples: 0,
        };
        let mut payload = [0; MAX_PAYLOAD_BYTES];
        let len = encode_pull_response(&PullResponse::NoData(state), &mut payload).unwrap();
        agent
            .receive(
                MonotonicMicros::new(1),
                &response(&request, &payload[..len]),
            )
            .unwrap();
        coordinator
            .settle_completed(
                agent,
                &mut Sink {
                    failures_remaining: 0,
                    chunk_calls: 0,
                },
                MonotonicMicros::new(1),
            )
            .unwrap();
    }

    #[test]
    fn telemetry_completion_requires_bounded_persistence_handoff() {
        let mut coordinator = coordinator();
        coordinator
            .configure_telemetry(
                7,
                TelemetryScheduleConfig {
                    interval: duration(600_000_000),
                    estimated_slot: duration(20_000),
                    retry_delay: duration(5_000_000),
                },
                MonotonicMicros::new(0),
            )
            .unwrap();
        let mut agent = agent();
        settle_initial_empty_measurement(&mut coordinator, &mut agent);
        assert!(matches!(
            coordinator
                .dispatch_next_work(&mut agent, MonotonicMicros::new(1))
                .unwrap(),
            Some(ChannelDispatch::Telemetry(_))
        ));
        let snapshot = TelemetrySnapshot {
            captured_device_time_us: 123_456,
            adxl355_temperature: Adxl355TemperatureCode::new(1885).unwrap(),
        };
        let event = ChannelEvent::TelemetrySnapshot {
            node_address: 7,
            snapshot,
        };
        let (mut sender, receiver) =
            crate::telemetry_persistence::telemetry_observation_queue(1).unwrap();
        assert_eq!(
            coordinator
                .settle_telemetry_event(&event, &mut sender, MonotonicMicros::new(2))
                .unwrap(),
            Some(TelemetrySettlement::Submitted { node_address: 7 })
        );
        let record = receiver.try_receive().unwrap().unwrap();
        assert_eq!(record.hardware_id, HARDWARE_ID);
        assert_eq!(record.node_address, 7);
        assert_eq!(record.boot_epoch, BootEpoch::new(9));
        assert_eq!(record.snapshot, snapshot);
        let metrics = coordinator.node_metrics_readers()[0].snapshot();
        assert_eq!(
            metrics.latest_telemetry.unwrap().adxl355_temperature_raw,
            1885
        );
        assert_eq!(
            metrics.latest_telemetry.unwrap().captured_device_time_us,
            123_456
        );
    }

    #[test]
    fn telemetry_backpressure_schedules_retry_and_releases_channel() {
        let mut coordinator = coordinator();
        coordinator
            .configure_telemetry(
                7,
                TelemetryScheduleConfig {
                    interval: duration(600_000_000),
                    estimated_slot: duration(20_000),
                    retry_delay: duration(5_000_000),
                },
                MonotonicMicros::new(0),
            )
            .unwrap();
        let mut agent = agent();
        settle_initial_empty_measurement(&mut coordinator, &mut agent);
        coordinator
            .dispatch_next_work(&mut agent, MonotonicMicros::new(1))
            .unwrap();
        let event = ChannelEvent::TelemetrySnapshot {
            node_address: 7,
            snapshot: TelemetrySnapshot {
                captured_device_time_us: 123_456,
                adxl355_temperature: Adxl355TemperatureCode::new(1885).unwrap(),
            },
        };
        let mut rejecting = crate::telemetry_persistence::RejectingTelemetryObservationSubmitter;
        assert_eq!(
            coordinator
                .settle_telemetry_event(&event, &mut rejecting, MonotonicMicros::new(2))
                .unwrap(),
            Some(TelemetrySettlement::Backpressured {
                node_address: 7,
                reason:
                    crate::telemetry_persistence::TelemetryObservationSubmitErrorKind::Disconnected,
            })
        );
        assert!(!coordinator.has_active_dispatch());
        assert_eq!(
            coordinator.node_metrics_readers()[0]
                .snapshot()
                .latest_telemetry,
            None
        );
    }

    #[test]
    fn no_data_updates_runtime_from_validated_stream_state() {
        let mut coordinator = coordinator();
        let mut agent = agent();
        let request = begin_wire_attempt(&mut coordinator, &mut agent);
        let mut payload = [0; MAX_PAYLOAD_BYTES];
        let state = StreamState {
            boot_epoch: BootEpoch::new(9),
            retained_from: SequenceCursor::ORIGIN,
            produced: SequenceCursor::ORIGIN,
            released: SequenceCursor::ORIGIN,
            capacity_samples: 2_048,
            overwritten_samples: 0,
        };
        let len = encode_pull_response(&PullResponse::NoData(state), &mut payload).unwrap();
        assert!(matches!(
            agent
                .receive(
                    MonotonicMicros::new(1),
                    &response(&request, &payload[..len])
                )
                .unwrap(),
            ChannelEvent::MeasurementNoData { .. }
        ));

        let mut sink = Sink {
            failures_remaining: 0,
            chunk_calls: 0,
        };
        assert!(matches!(
            coordinator
                .settle_completed(&mut agent, &mut sink, MonotonicMicros::new(1))
                .unwrap(),
            MeasurementSettlement::NoData {
                node_address: 7,
                ..
            }
        ));
        let snapshot = coordinator
            .runtime()
            .snapshot(7, MonotonicMicros::new(1))
            .unwrap();
        assert_eq!(snapshot.service_deadline, MonotonicMicros::new(500_001));
        assert_eq!(snapshot.ready_at, MonotonicMicros::new(256_001));
        assert_eq!(sink.chunk_calls, 0);
    }

    #[test]
    fn quiesce_blocks_new_work_and_waits_for_owned_work_to_settle() {
        let mut coordinator = coordinator();
        let mut agent = agent();
        let request = begin_wire_attempt(&mut coordinator, &mut agent);
        coordinator.begin_quiesce();
        assert!(coordinator.is_quiescing());
        assert!(!coordinator.is_quiescent());

        let mut payload = [0; MAX_PAYLOAD_BYTES];
        let state = StreamState {
            boot_epoch: BootEpoch::new(9),
            retained_from: SequenceCursor::ORIGIN,
            produced: SequenceCursor::ORIGIN,
            released: SequenceCursor::ORIGIN,
            capacity_samples: 2_048,
            overwritten_samples: 0,
        };
        let len = encode_pull_response(&PullResponse::NoData(state), &mut payload).unwrap();
        agent
            .receive(
                MonotonicMicros::new(1),
                &response(&request, &payload[..len]),
            )
            .unwrap();
        let mut sink = Sink {
            failures_remaining: 0,
            chunk_calls: 0,
        };
        coordinator
            .settle_completed(&mut agent, &mut sink, MonotonicMicros::new(1))
            .unwrap();

        assert!(coordinator.is_quiescent());
        assert!(coordinator
            .dispatch_next_work(&mut agent, MonotonicMicros::new(2))
            .unwrap()
            .is_none());
    }

    #[test]
    fn shutdown_drain_forces_a_post_stop_probe_until_no_data() {
        let mut coordinator = coordinator();
        let mut agent = agent();
        let first_request = begin_wire_attempt(&mut coordinator, &mut agent);
        let mut payload = [0; MAX_PAYLOAD_BYTES];
        let state = StreamState {
            boot_epoch: BootEpoch::new(9),
            retained_from: SequenceCursor::ORIGIN,
            produced: SequenceCursor::ORIGIN,
            released: SequenceCursor::ORIGIN,
            capacity_samples: 2_048,
            overwritten_samples: 0,
        };
        let len = encode_pull_response(&PullResponse::NoData(state), &mut payload).unwrap();
        agent
            .receive(
                MonotonicMicros::new(1),
                &response(&first_request, &payload[..len]),
            )
            .unwrap();
        let mut sink = Sink {
            failures_remaining: 0,
            chunk_calls: 0,
        };
        coordinator
            .settle_completed(&mut agent, &mut sink, MonotonicMicros::new(1))
            .unwrap();
        assert!(
            coordinator
                .runtime()
                .snapshot(7, MonotonicMicros::new(2))
                .unwrap()
                .ready_at
                > MonotonicMicros::new(2)
        );

        coordinator.begin_quiesce();
        coordinator.begin_shutdown_drain().unwrap();
        assert!(coordinator.is_shutdown_draining());
        assert!(matches!(
            coordinator
                .dispatch_next_work(&mut agent, MonotonicMicros::new(2))
                .unwrap(),
            Some(ChannelDispatch::Measurement(_))
        ));
        assert_eq!(
            agent.poll(MonotonicMicros::new(2)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let final_request = agent.take_outbound().unwrap();
        agent.transmit_completed(MonotonicMicros::new(2)).unwrap();
        agent
            .receive(
                MonotonicMicros::new(3),
                &response(&final_request, &payload[..len]),
            )
            .unwrap();
        assert!(matches!(
            coordinator
                .settle_completed(&mut agent, &mut sink, MonotonicMicros::new(3))
                .unwrap(),
            MeasurementSettlement::NoData {
                node_address: 7,
                ..
            }
        ));

        assert!(coordinator.is_shutdown_drain_complete());
        coordinator.finish_shutdown_drain().unwrap();
        assert!(coordinator.is_quiescent());
        coordinator.resume_scheduling().unwrap();
        assert!(!coordinator.is_quiescing());
    }

    #[test]
    fn aborted_service_drain_drops_only_undispatched_probes_and_returns_to_quiesce() {
        let mut coordinator = coordinator();
        let mut agent = agent();
        coordinator.begin_quiesce();
        assert!(coordinator.is_quiescent());
        coordinator.begin_shutdown_drain().unwrap();
        assert!(coordinator.is_shutdown_draining());
        coordinator.abort_shutdown_drain().unwrap();
        assert!(coordinator.is_quiescent());
        assert!(coordinator
            .dispatch_next_work(&mut agent, MonotonicMicros::new(1))
            .unwrap()
            .is_none());
        assert!(matches!(
            coordinator.abort_shutdown_drain(),
            Err(MeasurementCoordinatorError::ShutdownDrainNotActive)
        ));
    }

    #[test]
    fn unified_dispatch_interleaves_one_time_sync_without_blocking_measurement() {
        let mut coordinator = coordinator();
        coordinator
            .configure_time_sync(
                7,
                TimeSyncScheduleConfig {
                    interval: duration(100_000),
                    estimated_slot: duration(10_000),
                    retry_delay: duration(20_000),
                },
                MonotonicMicros::new(0),
            )
            .unwrap();
        let mut agent = agent();

        assert!(matches!(
            coordinator
                .dispatch_next_work(&mut agent, MonotonicMicros::new(0))
                .unwrap(),
            Some(ChannelDispatch::Measurement(_))
        ));
        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let measurement_request = agent.take_outbound().unwrap();
        agent.transmit_completed(MonotonicMicros::new(0)).unwrap();
        let state = StreamState {
            boot_epoch: BootEpoch::new(9),
            retained_from: SequenceCursor::ORIGIN,
            produced: SequenceCursor::ORIGIN,
            released: SequenceCursor::ORIGIN,
            capacity_samples: 2_048,
            overwritten_samples: 0,
        };
        let mut payload = [0; MAX_PAYLOAD_BYTES];
        let len = encode_pull_response(&PullResponse::NoData(state), &mut payload).unwrap();
        agent
            .receive(
                MonotonicMicros::new(1),
                &response(&measurement_request, &payload[..len]),
            )
            .unwrap();
        let mut sink = Sink {
            failures_remaining: 0,
            chunk_calls: 0,
        };
        coordinator
            .settle_completed(&mut agent, &mut sink, MonotonicMicros::new(1))
            .unwrap();

        assert!(matches!(
            coordinator
                .dispatch_next_work(&mut agent, MonotonicMicros::new(1))
                .unwrap(),
            Some(ChannelDispatch::TimeSync(_))
        ));
        assert_eq!(
            agent.poll(MonotonicMicros::new(1)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let time_request = agent.take_outbound().unwrap();
        agent.transmit_completed(MonotonicMicros::new(2)).unwrap();
        let time_payload = encode_time_success(TimeExchangeResponse {
            boot_epoch: BootEpoch::new(9),
            node_receive_us: 1_000,
            node_send_us: 1_001,
        })
        .unwrap();
        let event = agent
            .receive(
                MonotonicMicros::new(10),
                &response(&time_request, &time_payload),
            )
            .unwrap();
        assert!(matches!(event, ChannelEvent::TimeObservation { .. }));
        let (mut time_sender, time_receiver) =
            crate::time_persistence::time_observation_queue(1).unwrap();
        assert_eq!(
            coordinator
                .settle_time_sync_event(&event, &mut time_sender, MonotonicMicros::new(10),)
                .unwrap(),
            Some(TimeSyncSettlement::Submitted { node_address: 7 })
        );
        let persisted = time_receiver.try_receive().unwrap().unwrap();
        assert_eq!(persisted.hardware_id, HARDWARE_ID);
        assert_eq!(persisted.node_address, 7);
        assert!(!coordinator.has_active_dispatch());

        assert!(coordinator
            .dispatch_next_work(&mut agent, MonotonicMicros::new(90_009))
            .unwrap()
            .is_none());
        assert!(matches!(
            coordinator
                .dispatch_next_work(&mut agent, MonotonicMicros::new(90_010))
                .unwrap(),
            Some(ChannelDispatch::TimeSync(_))
        ));
        time_sender.try_submit(persisted).unwrap();
        assert_eq!(
            agent.poll(MonotonicMicros::new(90_010)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let retry_request = agent.take_outbound().unwrap();
        agent
            .transmit_completed(MonotonicMicros::new(90_011))
            .unwrap();
        let retry_payload = encode_time_success(TimeExchangeResponse {
            boot_epoch: BootEpoch::new(9),
            node_receive_us: 2_000,
            node_send_us: 2_001,
        })
        .unwrap();
        let retry_event = agent
            .receive(
                MonotonicMicros::new(90_020),
                &response(&retry_request, &retry_payload),
            )
            .unwrap();
        assert_eq!(
            coordinator
                .settle_time_sync_event(
                    &retry_event,
                    &mut time_sender,
                    MonotonicMicros::new(90_020),
                )
                .unwrap(),
            Some(TimeSyncSettlement::Backpressured {
                node_address: 7,
                reason: TimeObservationSubmitErrorKind::QueueFull,
            })
        );
        assert!(!coordinator.has_active_dispatch());
        assert!(coordinator
            .dispatch_next_work(&mut agent, MonotonicMicros::new(110_019))
            .unwrap()
            .is_none());
        assert!(matches!(
            coordinator
                .dispatch_next_work(&mut agent, MonotonicMicros::new(110_020))
                .unwrap(),
            Some(ChannelDispatch::TimeSync(_))
        ));
    }

    #[test]
    fn sink_failure_retries_persistence_without_repeating_wire_transfer() {
        let mut coordinator = coordinator();
        let mut agent = agent();
        let request = begin_wire_attempt(&mut coordinator, &mut agent);
        let axis = SampleAxis::new(17).unwrap();
        let chunk = SampleChunk {
            header: SampleChunkHeader {
                boot_epoch: BootEpoch::new(9),
                retained_from: SequenceCursor::ORIGIN,
                produced: SequenceCursor::new(1),
                first: SequenceCursor::ORIGIN,
                sample_count: 1,
            },
            config_revision: 3,
            timing: TimingEvidence {
                segment_id: 1,
                first_device_time_us: 1_000,
                last_device_time_us: 1_000,
                max_fit_residual_us: 0,
                quality_flags: TIMING_VALID,
            },
            samples: vec![SampleAxes::new(axis, axis, axis)],
        };
        let mut payload = [0; MAX_PAYLOAD_BYTES];
        let len = encode_pull_response(&PullResponse::SampleChunk(chunk), &mut payload).unwrap();
        assert!(matches!(
            agent
                .receive(
                    MonotonicMicros::new(1),
                    &response(&request, &payload[..len])
                )
                .unwrap(),
            ChannelEvent::MeasurementPersistenceRequired { .. }
        ));

        let mut sink = Sink {
            failures_remaining: 1,
            chunk_calls: 0,
        };
        assert!(matches!(
            coordinator.settle_completed(&mut agent, &mut sink, MonotonicMicros::new(2)),
            Err(MeasurementSettlementError::Persistence(_))
        ));
        assert_eq!(sink.chunk_calls, 1);
        assert!(agent.take_completed_measurement().is_none());

        let settled = coordinator
            .settle_completed(&mut agent, &mut sink, MonotonicMicros::new(3))
            .unwrap();
        assert_eq!(
            settled,
            MeasurementSettlement::Persisted {
                node_address: 7,
                progress: MeasurementProgress {
                    received: SequenceCursor::new(1),
                    produced: SequenceCursor::new(1),
                },
                kind: PersistenceKind::SampleChunk { sample_count: 1 },
            }
        );
        assert_eq!(sink.chunk_calls, 2);
    }

    #[test]
    fn bounded_handoff_releases_channel_and_settles_only_after_durable_completion() {
        let mut coordinator = coordinator();
        let mut agent = agent();
        let request = begin_wire_attempt(&mut coordinator, &mut agent);
        let axis = SampleAxis::new(17).unwrap();
        let chunk = SampleChunk {
            header: SampleChunkHeader {
                boot_epoch: BootEpoch::new(9),
                retained_from: SequenceCursor::ORIGIN,
                produced: SequenceCursor::new(1),
                first: SequenceCursor::ORIGIN,
                sample_count: 1,
            },
            config_revision: 3,
            timing: TimingEvidence {
                segment_id: 1,
                first_device_time_us: 1_000,
                last_device_time_us: 1_000,
                max_fit_residual_us: 0,
                quality_flags: TIMING_VALID,
            },
            samples: vec![SampleAxes::new(axis, axis, axis)],
        };
        let mut payload = [0; MAX_PAYLOAD_BYTES];
        let len = encode_pull_response(&PullResponse::SampleChunk(chunk), &mut payload).unwrap();
        assert!(matches!(
            agent
                .receive(
                    MonotonicMicros::new(1),
                    &response(&request, &payload[..len])
                )
                .unwrap(),
            ChannelEvent::MeasurementPersistenceRequired { .. }
        ));

        let sink = Sink {
            failures_remaining: 1,
            chunk_calls: 0,
        };
        let mut writer = BoundedPersistenceWorker::new(sink, 2).unwrap();
        let job_id = coordinator
            .handoff_completed(&mut agent, &mut writer)
            .unwrap();
        assert!(!coordinator.has_active_dispatch());
        assert_eq!(coordinator.in_flight_write_count(), 1);
        assert!(
            coordinator
                .runtime()
                .snapshot(7, MonotonicMicros::new(1))
                .unwrap()
                .in_flight
        );

        assert_eq!(writer.process_ready(1), Ok(1));
        let completion = writer.poll_completion().unwrap();
        assert_eq!(completion.id, job_id);
        assert!(matches!(
            coordinator.settle_write_completion(completion, MonotonicMicros::new(2)),
            Err(MeasurementWriteSettlementError::Storage {
                job_id: failed_id,
                error: SinkError,
            }) if failed_id == job_id
        ));
        assert_eq!(coordinator.in_flight_write_count(), 1);
        assert_eq!(
            coordinator.write_recovery_range(job_id).unwrap(),
            WriteRecoveryRange {
                hardware_id: HARDWARE_ID,
                boot_epoch: BootEpoch::new(9),
                start: SequenceCursor::ORIGIN,
                end: SequenceCursor::new(1),
            }
        );
        assert!(writer.is_faulted());

        writer.replace_recovered_sink(Sink {
            failures_remaining: 0,
            chunk_calls: 0,
        });
        assert_eq!(writer.process_ready(1), Ok(1));
        let completion = writer.poll_completion().unwrap();
        assert_eq!(completion.id, job_id);
        assert_eq!(
            coordinator
                .settle_write_completion(completion, MonotonicMicros::new(3))
                .unwrap(),
            MeasurementSettlement::Persisted {
                node_address: 7,
                progress: MeasurementProgress {
                    received: SequenceCursor::new(1),
                    produced: SequenceCursor::new(1),
                },
                kind: PersistenceKind::SampleChunk { sample_count: 1 },
            }
        );
        assert_eq!(coordinator.in_flight_write_count(), 0);
        assert_eq!(writer.into_sink().chunk_calls, 1);
    }

    #[test]
    fn full_writer_retry_uses_the_coordinator_owned_record() {
        let mut coordinator = coordinator();
        let mut agent = agent();
        let request = begin_wire_attempt(&mut coordinator, &mut agent);
        let axis = SampleAxis::new(17).unwrap();
        let chunk = SampleChunk {
            header: SampleChunkHeader {
                boot_epoch: BootEpoch::new(9),
                retained_from: SequenceCursor::ORIGIN,
                produced: SequenceCursor::new(1),
                first: SequenceCursor::ORIGIN,
                sample_count: 1,
            },
            config_revision: 3,
            timing: TimingEvidence {
                segment_id: 1,
                first_device_time_us: 1_000,
                last_device_time_us: 1_000,
                max_fit_residual_us: 0,
                quality_flags: TIMING_VALID,
            },
            samples: vec![SampleAxes::new(axis, axis, axis)],
        };
        let mut payload = [0; MAX_PAYLOAD_BYTES];
        let len =
            encode_pull_response(&PullResponse::SampleChunk(chunk.clone()), &mut payload).unwrap();
        agent
            .receive(
                MonotonicMicros::new(1),
                &response(&request, &payload[..len]),
            )
            .unwrap();

        let mut writer = BoundedPersistenceWorker::new(
            Sink {
                failures_remaining: 0,
                chunk_calls: 0,
            },
            1,
        )
        .unwrap();
        writer
            .submit(PersistenceEnvelope::chunk(HARDWARE_ID, chunk).unwrap())
            .unwrap();
        assert!(matches!(
            coordinator.handoff_completed(&mut agent, &mut writer),
            Err(MeasurementHandoffError::Submit(SubmitErrorKind::QueueFull))
        ));
        assert_eq!(coordinator.pending_completion_node(), Some(7));
        assert!(agent.take_completed_measurement().is_none());

        writer.process_ready(1).unwrap();
        writer.poll_completion().unwrap();
        assert!(coordinator
            .handoff_completed(&mut agent, &mut writer)
            .is_ok());
        assert_eq!(coordinator.pending_completion_node(), None);
        assert_eq!(coordinator.in_flight_write_count(), 1);
    }

    #[test]
    fn exhausted_small_error_budget_becomes_runtime_backoff() {
        let mut coordinator = coordinator();
        let mut agent = agent();
        let dispatch = coordinator
            .dispatch_next(&mut agent, MonotonicMicros::new(0))
            .unwrap()
            .unwrap();

        for now in [0, 11] {
            assert_eq!(
                agent.poll(MonotonicMicros::new(now)).unwrap(),
                ChannelEvent::TransmitReady
            );
            agent.take_outbound().unwrap();
            let event = agent
                .transmit_failed(MonotonicMicros::new(now + 1))
                .unwrap();
            if now == 0 {
                assert_eq!(event, ChannelEvent::WireAttemptFailed { node_address: 7 });
            } else {
                assert_eq!(
                    event,
                    ChannelEvent::MeasurementAttemptsExhausted { node_address: 7 }
                );
            }
        }

        let mut sink = Sink {
            failures_remaining: 0,
            chunk_calls: 0,
        };
        assert_eq!(
            coordinator
                .settle_completed(&mut agent, &mut sink, MonotonicMicros::new(12))
                .unwrap(),
            MeasurementSettlement::RetryScheduled { node_address: 7 }
        );
        let snapshot = coordinator
            .runtime()
            .snapshot(7, MonotonicMicros::new(12))
            .unwrap();
        assert_eq!(snapshot.consecutive_failures, 1);
        assert_eq!(snapshot.ready_at, MonotonicMicros::new(10_012));
        assert_eq!(dispatch.node_address, 7);
    }
}
