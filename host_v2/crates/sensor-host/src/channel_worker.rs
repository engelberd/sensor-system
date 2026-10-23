use core::fmt;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use sensor_protocol::RuntimeState;

use crate::capture_profile::{CaptureInstallationProfile, CaptureProfileError};
use crate::capture_repository::CaptureRepository;
use crate::capture_repository::CaptureRepositoryError;
use crate::capture_runtime::{CaptureRolloverError, CaptureRuntimeError};
use crate::channel_activation::{
    activate_recording_nodes, start_recording_nodes, stop_recording_nodes,
    RecordingActivationError, RecordingActivationPolicy, RecordingShutdownError,
    RecordingShutdownPolicy,
};
use crate::channel_agent::{ChannelAgentError, ChannelEvent, ControlWorkKey};
use crate::channel_bootstrap::{BootstrappedChannel, NodeBootstrapEvidence};
use crate::channel_endpoint::{ChannelEndpointEvent, MonotonicClock};
use crate::channel_metrics::{ChannelMetricsPublisher, ChannelMetricsReader};
use crate::channel_mode::{
    ChannelModeError, MaintenanceEntryDirective, MaintenanceEntryEvent, MaintenanceEntryPlan,
    ResumeRecordingDirective, ResumeRecordingError, ResumeRecordingEvent, ResumeRecordingPlan,
};
use crate::channel_recording_loop::{
    ThreadedChannelRecordingEvent, ThreadedChannelRecordingLoop, ThreadedChannelRecordingLoopError,
};
use crate::channel_recovery::{
    prepare_capture_stream, reconcile_channel_streams, ChannelRecoveryError,
};
use crate::channel_resume::{
    ResumeNodeContract, ResumeVerificationError, ResumeVerificationGroup, ResumeVerificationPolicy,
    ResumeVerificationProgress,
};
use crate::channel_startup::{
    assemble_recording_coordinator, ChannelAssemblyError, ChannelStartupPlan,
    RecordingAssemblyPolicy,
};
use crate::channel_supervisor::SupervisedChannelTask;
use crate::configuration_history::{
    append_configuration_snapshot, ConfigurationHistoryError, ConfigurationSnapshot,
};
use crate::discovery::DiscoveryTargetId;
use crate::handshake::HandshakeRequirements;
use crate::measurement_coordinator::MeasurementCoordinatorError;
use crate::node_metrics::NodeMetricsReader;
use crate::operations::{
    ChannelCommandDecision, ChannelOperatingMode, ChannelOperationsController, OperationKind,
    OperationRequest, OperationResult, OperationTarget, OperationsError,
};
use crate::persistence_executor::{
    GroupedPersistenceExecutor, PersistenceExecutorError, PersistenceShutdown,
};
use crate::safe_state::{SafeStatePolicy, SafeStateTarget, SafeStateWirePolicy};
use crate::safe_state_group::{
    SafeStateGroupError, SafeStateGroupOperation, SafeStateGroupProgress, SafeStateNodeContract,
};
use crate::stream_recovery::ExpectedStreamState;
use crate::telemetry_persistence::TelemetryObservationSubmitter;
use crate::telemetry_profile::{
    DiscoveredTelemetryStream, TelemetryPersistenceInstallationProfile,
};
use crate::telemetry_repository::TelemetryStreamKey;
use crate::telemetry_rollover::{
    TelemetryRolloverClient, TelemetryRolloverSubmitError, TelemetryRolloverTicket,
};
use crate::time_persistence::TimeObservationSubmitter;
use crate::time_profile::DiscoveredTimeStream;
use crate::time_profile::TimePersistenceInstallationProfile;
use crate::time_repository::TimeStreamKey;
use crate::time_rollover::{TimeRolloverClient, TimeRolloverSubmitError, TimeRolloverTicket};
use crate::time_wal::HostClockId;

pub type ProductionChannelLoop<P, C, T> =
    ThreadedChannelRecordingLoop<P, C, GroupedPersistenceExecutor<CaptureRepository>, T>;

const NODE_RESTART_SETTLE_TIME: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelCompositionPolicy {
    pub activation: RecordingActivationPolicy,
    pub takeover: ControlledChannelShutdownPolicy,
    pub assembly: RecordingAssemblyPolicy,
}

#[derive(Clone)]
pub struct ChannelRolloverServices {
    channel_name: String,
    capture: CaptureInstallationProfile,
    time: TimePersistenceInstallationProfile,
    host_clock_id: HostClockId,
    time_client: TimeRolloverClient,
    telemetry: TelemetryPersistenceInstallationProfile,
    telemetry_client: Option<TelemetryRolloverClient>,
}

impl ChannelRolloverServices {
    pub fn new(
        channel_name: impl Into<String>,
        capture: CaptureInstallationProfile,
        time: TimePersistenceInstallationProfile,
        host_clock_id: HostClockId,
        time_client: TimeRolloverClient,
        telemetry: TelemetryPersistenceInstallationProfile,
        telemetry_client: Option<TelemetryRolloverClient>,
    ) -> Self {
        Self {
            channel_name: channel_name.into(),
            capture,
            time,
            host_clock_id,
            time_client,
            telemetry,
            telemetry_client,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlledChannelShutdownPolicy {
    pub barrier_deadline: Duration,
    pub node: RecordingShutdownPolicy,
    pub drain_deadline: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelMaintenancePolicy {
    pub barrier_deadline: Duration,
    pub drain_deadline: Duration,
    pub service_operation_deadline: Duration,
    pub safe_state: SafeStatePolicy,
    pub wire: SafeStateWirePolicy,
    pub resume_verification: ResumeVerificationPolicy,
}

impl ChannelMaintenancePolicy {
    pub fn validate(self) -> Result<Self, ChannelMaintenanceError> {
        if self.barrier_deadline.is_zero() {
            return Err(ChannelMaintenanceError::ZeroBarrierDeadline);
        }
        if self.drain_deadline.is_zero() {
            return Err(ChannelMaintenanceError::ZeroDrainDeadline);
        }
        if self.service_operation_deadline.is_zero() {
            return Err(ChannelMaintenanceError::ZeroServiceOperationDeadline);
        }
        self.safe_state
            .validate()
            .map_err(ChannelMaintenanceError::SafeStatePolicy)?;
        if self.resume_verification.node_deadline.get() == 0 {
            return Err(ChannelMaintenanceError::ZeroResumeNodeDeadline);
        }
        Ok(self)
    }
}

impl ControlledChannelShutdownPolicy {
    pub fn validate(self) -> Result<Self, ControlledChannelShutdownError> {
        if self.barrier_deadline.is_zero() {
            return Err(ControlledChannelShutdownError::ZeroBarrierDeadline);
        }
        self.node
            .validate()
            .map_err(ControlledChannelShutdownError::Stop)?;
        if self.drain_deadline.is_zero() {
            return Err(ControlledChannelShutdownError::ZeroDrainDeadline);
        }
        Ok(self)
    }
}

pub struct ProductionChannelTask<P, C, T> {
    recording: ProductionChannelLoop<P, C, T>,
    nodes: Vec<ChannelControlNode>,
    shutdown_policy: ControlledChannelShutdownPolicy,
    metrics: ChannelMetricsPublisher,
    metrics_reader: ChannelMetricsReader,
    node_metrics_readers: Vec<NodeMetricsReader>,
    operations: ChannelOperationsController,
    maintenance_policy: ChannelMaintenancePolicy,
    maintenance: Option<MaintenanceEntryExecution>,
    resume: Option<ResumeRecordingExecution>,
    configuration: Option<ConfigurationExecution>,
    configuration_workflow: Option<AutomaticConfigurationWorkflow>,
    commissioning: Option<CommissioningExecution>,
    restart: Option<RestartExecution>,
    commissioning_discoveries: Vec<sensor_protocol::CommissioningIdentity>,
    configuration_plan: ChannelStartupPlan,
    rollover: ChannelRolloverServices,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelControlNode {
    pub id: DiscoveryTargetId,
    pub requirements: HandshakeRequirements,
    pub boot_epoch: sensor_core::BootEpoch,
    pub configuration: sensor_protocol::ObservedConfiguration,
}

struct MaintenanceEntryExecution {
    plan: MaintenanceEntryPlan,
    contracts: Vec<SafeStateNodeContract>,
    group: Option<SafeStateGroupOperation>,
    barrier_deadline: Instant,
    drain_deadline: Option<Instant>,
    drain_started: bool,
    finish_active_operation: bool,
}

struct ResumeRecordingExecution {
    plan: ResumeRecordingPlan,
    verification: ResumeVerificationGroup,
    activation: Option<SafeStateGroupOperation>,
    reconciled_nodes: usize,
    pending_time: Option<PendingEpochRollover>,
    reconciliation_deadline: Option<Instant>,
    completion_result: Option<OperationResult>,
}

struct PendingEpochRollover {
    evidence: crate::channel_resume::ResumeNodeEvidence,
    capture_durable: sensor_core::SequenceCursor,
    expected_time_stream: TimeStreamKey,
    time_ticket: Option<TimeRolloverTicket>,
    expected_telemetry_stream: Option<TelemetryStreamKey>,
    telemetry_ticket: Option<TelemetryRolloverTicket>,
}

struct ConfigurationExecution {
    key: ControlWorkKey,
    deadline: Instant,
    terminal_error: Option<OperationsError>,
    finish_active_operation: bool,
}

enum AutomaticConfigurationWorkflow {
    EnteringMaintenance(OperationRequest),
    Configuring,
    ReadyToResume(OperationResult),
    Resuming,
}

enum CommissioningExecution {
    Scan {
        round_nonce: u64,
        slot_count: u16,
        next_slot: u16,
        active_slot: Option<u16>,
        discovered: Vec<sensor_protocol::CommissioningIdentity>,
        deadline: Instant,
    },
    Assign {
        expected: sensor_protocol::CommissioningIdentity,
        node_address: u8,
        phase: CommissioningAssignPhase,
        deadline: Instant,
    },
}

enum CommissioningAssignPhase {
    Assigning {
        operation_id: u32,
    },
    Verifying {
        assigned: sensor_protocol::AssignAddressResponse,
    },
}

struct RestartExecution {
    node_address: u8,
    operation_id: u32,
    deadline: Instant,
    settle_until: Option<Instant>,
    terminal_error: Option<OperationsError>,
}

fn configuration_key_node(key: ControlWorkKey) -> u8 {
    let ControlWorkKey::Configuration { node_address, .. } = key else {
        unreachable!("configuration execution owns a configuration key")
    };
    node_address
}

fn default_measurement_configuration() -> sensor_protocol::MeasurementConfiguration {
    sensor_protocol::MeasurementConfiguration::product_default()
}

fn configuration_requires_quiesce(kind: &OperationKind) -> bool {
    matches!(
        kind,
        OperationKind::ApplyConfiguration { .. } | OperationKind::ResetConfiguration { .. }
    )
}

impl ResumeRecordingExecution {
    fn new(
        nodes: &[ChannelControlNode],
        policy: ChannelMaintenancePolicy,
        completion_result: Option<OperationResult>,
    ) -> Result<Self, ChannelMaintenanceError> {
        let contracts = nodes
            .iter()
            .map(|node| ResumeNodeContract {
                requirements: node.requirements,
                previous_boot_epoch: node.boot_epoch,
            })
            .collect::<Vec<_>>();
        Ok(Self {
            plan: ResumeRecordingPlan::new(),
            verification: ResumeVerificationGroup::new(&contracts, policy.resume_verification)?,
            activation: None,
            reconciled_nodes: 0,
            pending_time: None,
            reconciliation_deadline: None,
            completion_result,
        })
    }
}

impl MaintenanceEntryExecution {
    fn new(
        nodes: &[ChannelControlNode],
        policy: ChannelMaintenancePolicy,
        finish_active_operation: bool,
    ) -> Result<Self, ChannelMaintenanceError> {
        let contracts = nodes
            .iter()
            .map(|node| SafeStateNodeContract {
                requirements: node.requirements,
                boot_epoch: node.boot_epoch,
            })
            .collect::<Vec<_>>();
        if contracts.is_empty() {
            return Err(ChannelMaintenanceError::EmptyChannel);
        }
        let barrier_deadline = Instant::now()
            .checked_add(policy.barrier_deadline)
            .ok_or(ChannelMaintenanceError::DeadlineOverflow)?;
        Ok(Self {
            plan: MaintenanceEntryPlan::new(),
            contracts,
            group: None,
            barrier_deadline,
            drain_deadline: None,
            drain_started: false,
            finish_active_operation,
        })
    }
}

impl<P, C, T> ProductionChannelTask<P, C, T>
where
    P: Read + Write,
    C: MonotonicClock,
    T: TimeObservationSubmitter,
{
    pub fn new(
        recording: ProductionChannelLoop<P, C, T>,
        startup: &ChannelStartupPlan,
        shutdown_policy: ControlledChannelShutdownPolicy,
        operations: ChannelOperationsController,
        maintenance_policy: ChannelMaintenancePolicy,
        rollover: ChannelRolloverServices,
    ) -> Result<Self, ProductionChannelTaskBuildError> {
        let shutdown_policy = shutdown_policy.validate()?;
        let maintenance_policy = maintenance_policy.validate()?;
        let nodes: Vec<ChannelControlNode> = startup
            .nodes()
            .iter()
            .map(|node| ChannelControlNode {
                id: DiscoveryTargetId {
                    channel_id: node.discovered.channel_id,
                    node_address: node.discovered.node_address,
                },
                requirements: node.requirements,
                boot_epoch: node.discovered.boot_epoch,
                configuration: node.configuration,
            })
            .collect();
        for node in &nodes {
            recording
                .observe_configuration(node.id.node_address, node.configuration)
                .map_err(ProductionChannelTaskBuildError::Measurement)?;
        }
        let storage_metrics = recording.persistence().metrics_reader();
        let node_metrics_readers = recording.measurements().node_metrics_readers();
        let (metrics, metrics_reader) =
            ChannelMetricsPublisher::new(startup.channel_id(), Some(storage_metrics));
        Ok(Self {
            recording,
            nodes,
            shutdown_policy,
            metrics,
            metrics_reader,
            node_metrics_readers,
            operations,
            maintenance_policy,
            maintenance: None,
            resume: None,
            configuration: None,
            configuration_workflow: None,
            commissioning: None,
            restart: None,
            commissioning_discoveries: Vec::new(),
            configuration_plan: startup.clone(),
            rollover,
        })
    }

    pub fn metrics_reader(&self) -> ChannelMetricsReader {
        self.metrics_reader.clone()
    }

    pub fn node_metrics_readers(&self) -> Vec<NodeMetricsReader> {
        self.node_metrics_readers.clone()
    }

    pub fn control_nodes(&self) -> &[ChannelControlNode] {
        &self.nodes
    }

    fn begin_automatic_configuration(
        &mut self,
        request: OperationRequest,
    ) -> Result<(), ProductionChannelStepError> {
        if !self.preflight_configuration(&request)? {
            return Ok(());
        }
        self.recording.begin_quiesce();
        self.operations
            .set_mode(ChannelOperatingMode::EnteringMaintenance)?;
        self.maintenance = Some(MaintenanceEntryExecution::new(
            &self.nodes,
            self.maintenance_policy,
            false,
        )?);
        self.configuration_workflow =
            Some(AutomaticConfigurationWorkflow::EnteringMaintenance(request));
        Ok(())
    }

    fn preflight_configuration(
        &mut self,
        request: &OperationRequest,
    ) -> Result<bool, ProductionChannelStepError> {
        let OperationTarget::Node { node_address, .. } = request.target else {
            self.operations
                .fail_active(OperationsError::ExecutorUnavailable)?;
            return Ok(false);
        };
        if !self
            .nodes
            .iter()
            .any(|node| node.id.node_address == node_address)
        {
            self.operations
                .fail_active(OperationsError::NodeNotConfigured)?;
            return Ok(false);
        }
        let proposed = match &request.kind {
            OperationKind::ApplyConfiguration { configuration, .. } => Some(*configuration),
            OperationKind::ResetConfiguration { .. } => Some(default_measurement_configuration()),
            _ => None,
        };
        if let Some(configuration) = proposed {
            match self
                .configuration_plan
                .preview_configuration(node_address, configuration)
            {
                Ok(report) if report.accepted() => {}
                Ok(_) => {
                    self.operations
                        .fail_active(OperationsError::ConfigurationAdmissionRejected)?;
                    return Ok(false);
                }
                Err(_) => {
                    self.operations
                        .fail_active(OperationsError::ConfigurationFailed)?;
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn advance_automatic_configuration(&mut self) -> Result<(), ProductionChannelStepError> {
        if self.operations.active_operation().is_none() {
            self.configuration_workflow = None;
            return Ok(());
        }
        let Some(workflow) = self.configuration_workflow.take() else {
            return Ok(());
        };
        self.configuration_workflow = match workflow {
            AutomaticConfigurationWorkflow::EnteringMaintenance(request)
                if self.maintenance.is_none() =>
            {
                self.configuration = self.begin_configuration(request, false)?;
                self.configuration
                    .as_ref()
                    .map(|_| AutomaticConfigurationWorkflow::Configuring)
            }
            AutomaticConfigurationWorkflow::ReadyToResume(result)
                if self.configuration.is_none() && self.resume.is_none() =>
            {
                self.operations
                    .set_mode(ChannelOperatingMode::ResumingRecording)?;
                self.resume = Some(ResumeRecordingExecution::new(
                    &self.nodes,
                    self.maintenance_policy,
                    Some(result),
                )?);
                Some(AutomaticConfigurationWorkflow::Resuming)
            }
            workflow => Some(workflow),
        };
        if self.operations.active_operation().is_none() {
            self.configuration_workflow = None;
        }
        Ok(())
    }

    fn begin_configuration(
        &mut self,
        request: OperationRequest,
        finish_active_operation: bool,
    ) -> Result<Option<ConfigurationExecution>, ProductionChannelStepError> {
        if finish_active_operation && !self.preflight_configuration(&request)? {
            return Ok(None);
        }
        let OperationTarget::Node { node_address, .. } = request.target else {
            unreachable!("configuration preflight validates node scope")
        };

        let submission = match request.kind {
            OperationKind::GetConfiguration => self
                .recording
                .endpoint_mut()
                .agent_mut()
                .submit_get_configuration(node_address)
                .map(|()| None),
            OperationKind::ValidateConfiguration { configuration } => self
                .recording
                .endpoint_mut()
                .agent_mut()
                .submit_validate_configuration(node_address, configuration)
                .map(|()| None),
            OperationKind::ApplyConfiguration {
                expected_revision,
                configuration,
            } => self
                .recording
                .endpoint_mut()
                .agent_mut()
                .submit_apply_configuration(node_address, expected_revision, configuration)
                .map(Some),
            OperationKind::ResetConfiguration { expected_revision } => self
                .recording
                .endpoint_mut()
                .agent_mut()
                .submit_reset_configuration(node_address, expected_revision)
                .map(Some),
            _ => {
                self.operations
                    .fail_active(OperationsError::ExecutorUnavailable)?;
                return Ok(None);
            }
        };
        let wire_operation_id = match submission {
            Ok(value) => value,
            Err(_) => {
                self.operations
                    .fail_active(OperationsError::ConfigurationFailed)?;
                return Ok(None);
            }
        };
        let deadline = Instant::now()
            .checked_add(self.maintenance_policy.service_operation_deadline)
            .ok_or(ChannelMaintenanceError::DeadlineOverflow)?;
        Ok(Some(ConfigurationExecution {
            key: ControlWorkKey::Configuration {
                node_address,
                operation_id: wire_operation_id,
            },
            deadline,
            terminal_error: None,
            finish_active_operation,
        }))
    }

    fn drive_configuration(
        &mut self,
        mut execution: ConfigurationExecution,
    ) -> Result<Option<ConfigurationExecution>, ProductionChannelStepError> {
        if execution.terminal_error.is_none() && Instant::now() >= execution.deadline {
            execution.terminal_error = Some(OperationsError::ConfigurationDeadline);
        }
        if let Some(error) = execution.terminal_error {
            match self
                .recording
                .endpoint_mut()
                .agent_mut()
                .abandon_control_work(execution.key)
            {
                Ok(_) | Err(ChannelAgentError::ControlWorkNotFound(_)) => {
                    self.operations.fail_active(error)?;
                    return Ok(None);
                }
                Err(ChannelAgentError::ControlAttemptInFlight(_)) => {}
                Err(_) => return Err(ProductionChannelStepError::Operations(error)),
            }
        }

        let started = Instant::now();
        let endpoint_event = match self.recording.endpoint_mut().step() {
            Ok(event) => event,
            Err(_) => {
                execution.terminal_error = Some(OperationsError::ConfigurationFailed);
                return Ok(Some(execution));
            }
        };
        self.metrics.observe_step(
            &ThreadedChannelRecordingEvent::Endpoint(endpoint_event.clone()),
            started.elapsed(),
        );
        if execution.terminal_error.is_some() {
            return Ok(Some(execution));
        }
        match endpoint_event {
            ChannelEndpointEvent::Agent(ChannelEvent::ConfigurationComplete {
                node_address,
                outcome,
            }) if node_address == configuration_key_node(execution.key) => {
                let result = match outcome {
                    crate::configuration::ConfigurationOutcome::Observed(value) => {
                        OperationResult::ConfigurationObserved(value)
                    }
                    crate::configuration::ConfigurationOutcome::Validated(value) => {
                        OperationResult::ConfigurationValidated(value)
                    }
                    crate::configuration::ConfigurationOutcome::Applied(value) => {
                        let result = OperationResult::ConfigurationApplied(value);
                        if value.code != sensor_protocol::ConfigOperationCode::Applied {
                            self.operations.fail_active_with_result(
                                result,
                                OperationsError::ConfigurationRejected,
                            )?;
                            return Ok(None);
                        }
                        let mut candidate = self.configuration_plan.clone();
                        if candidate
                            .accept_configuration(node_address, value.observed)
                            .is_err()
                        {
                            execution.terminal_error =
                                Some(OperationsError::ConfigurationAdmissionRejected);
                            return Ok(Some(execution));
                        }
                        let schedule = candidate
                            .node(node_address)
                            .ok_or(ProductionChannelStepError::Operations(
                                OperationsError::NodeNotConfigured,
                            ))?
                            .schedule;
                        let identity = self
                            .nodes
                            .iter()
                            .find(|node| node.id.node_address == node_address)
                            .copied()
                            .ok_or(ProductionChannelStepError::Operations(
                                OperationsError::NodeNotConfigured,
                            ))?;
                        if self
                            .recording
                            .persistence()
                            .finish_capture_stream(
                                identity.requirements.hardware_id,
                                identity.boot_epoch,
                            )
                            .is_err()
                        {
                            execution.terminal_error = Some(OperationsError::ConfigurationFailed);
                            return Ok(Some(execution));
                        }
                        if append_configuration_snapshot(
                            self.rollover.capture.root(),
                            ConfigurationSnapshot {
                                hardware_id: identity.requirements.hardware_id,
                                boot_epoch: identity.boot_epoch,
                                observed: value.observed,
                                effective_sample_seq: value.effective_sample_seq,
                            },
                        )
                        .is_err()
                        {
                            execution.terminal_error = Some(OperationsError::ConfigurationFailed);
                            return Ok(Some(execution));
                        }
                        if self.recording.reconfigure_node(schedule).is_err() {
                            execution.terminal_error = Some(OperationsError::ConfigurationFailed);
                            return Ok(Some(execution));
                        }
                        if self
                            .recording
                            .observe_configuration(node_address, value.observed)
                            .is_err()
                        {
                            execution.terminal_error = Some(OperationsError::ConfigurationFailed);
                            return Ok(Some(execution));
                        }
                        self.configuration_plan = candidate;
                        let node = self
                            .nodes
                            .iter_mut()
                            .find(|node| node.id.node_address == node_address)
                            .ok_or(ProductionChannelStepError::Operations(
                                OperationsError::NodeNotConfigured,
                            ))?;
                        node.configuration = value.observed;
                        node.requirements.required_output_rate =
                            Some(value.observed.desired.output_rate);
                        result
                    }
                    crate::configuration::ConfigurationOutcome::NotAccepted => {
                        execution.terminal_error = Some(OperationsError::ConfigurationFailed);
                        return Ok(Some(execution));
                    }
                };
                if execution.finish_active_operation {
                    self.operations.complete_active(result)?;
                } else {
                    self.configuration_workflow =
                        Some(AutomaticConfigurationWorkflow::ReadyToResume(result));
                }
                Ok(None)
            }
            ChannelEndpointEvent::Agent(ChannelEvent::ConfigurationNotAccepted {
                node_address,
                ..
            })
            | ChannelEndpointEvent::Agent(ChannelEvent::ConfigurationTimedOut {
                node_address,
                ..
            }) if node_address == configuration_key_node(execution.key) => Ok(Some(execution)),
            _ => Ok(Some(execution)),
        }
    }

    fn begin_commissioning(
        &mut self,
        request: OperationRequest,
    ) -> Result<Option<CommissioningExecution>, ProductionChannelStepError> {
        match request.kind {
            OperationKind::ScanUnassigned {
                round_nonce,
                slot_count,
            } => {
                let duration = self
                    .maintenance_policy
                    .service_operation_deadline
                    .checked_mul(u32::from(slot_count))
                    .ok_or(ChannelMaintenanceError::DeadlineOverflow)?;
                let deadline = Instant::now()
                    .checked_add(duration)
                    .ok_or(ChannelMaintenanceError::DeadlineOverflow)?;
                Ok(Some(CommissioningExecution::Scan {
                    round_nonce,
                    slot_count,
                    next_slot: 0,
                    active_slot: None,
                    discovered: Vec::with_capacity(usize::from(slot_count)),
                    deadline,
                }))
            }
            OperationKind::AssignAddress {
                round_nonce,
                hardware_id,
                node_address,
            } => {
                if self
                    .nodes
                    .iter()
                    .any(|node| node.id.node_address == node_address)
                {
                    self.operations
                        .fail_active(OperationsError::CommissioningAddressConflict)?;
                    return Ok(None);
                }
                let Some(expected) =
                    self.commissioning_discoveries
                        .iter()
                        .copied()
                        .find(|identity| {
                            identity.round_nonce == round_nonce
                                && identity.hardware_id == hardware_id
                        })
                else {
                    self.operations
                        .fail_active(OperationsError::CommissioningIdentityNotScanned)?;
                    return Ok(None);
                };
                let operation_id = match self
                    .recording
                    .endpoint_mut()
                    .agent_mut()
                    .submit_commissioning_assignment(round_nonce, hardware_id, node_address)
                {
                    Ok(value) => value,
                    Err(_) => {
                        self.operations
                            .fail_active(OperationsError::CommissioningFailed)?;
                        return Ok(None);
                    }
                };
                let deadline = Instant::now()
                    .checked_add(self.maintenance_policy.service_operation_deadline)
                    .ok_or(ChannelMaintenanceError::DeadlineOverflow)?;
                Ok(Some(CommissioningExecution::Assign {
                    expected,
                    node_address,
                    phase: CommissioningAssignPhase::Assigning { operation_id },
                    deadline,
                }))
            }
            _ => {
                self.operations
                    .fail_active(OperationsError::ExecutorUnavailable)?;
                Ok(None)
            }
        }
    }

    fn drive_commissioning(
        &mut self,
        mut execution: CommissioningExecution,
    ) -> Result<Option<CommissioningExecution>, ProductionChannelStepError> {
        if Instant::now()
            >= match &execution {
                CommissioningExecution::Scan { deadline, .. }
                | CommissioningExecution::Assign { deadline, .. } => *deadline,
            }
        {
            let key = match &execution {
                CommissioningExecution::Scan {
                    round_nonce,
                    active_slot: Some(slot_index),
                    ..
                } => Some(ControlWorkKey::CommissioningProbe {
                    round_nonce: *round_nonce,
                    slot_index: *slot_index,
                }),
                CommissioningExecution::Assign {
                    phase: CommissioningAssignPhase::Assigning { operation_id },
                    ..
                } => Some(ControlWorkKey::CommissioningAssignment {
                    operation_id: *operation_id,
                }),
                CommissioningExecution::Assign {
                    node_address,
                    phase: CommissioningAssignPhase::Verifying { .. },
                    ..
                } => Some(ControlWorkKey::Handshake {
                    node_address: *node_address,
                }),
                _ => None,
            };
            if let Some(key) = key {
                match self
                    .recording
                    .endpoint_mut()
                    .agent_mut()
                    .abandon_control_work(key)
                {
                    Ok(_) | Err(ChannelAgentError::ControlWorkNotFound(_)) => {}
                    Err(ChannelAgentError::ControlAttemptInFlight(_)) => {
                        return Ok(Some(execution))
                    }
                    Err(_) => {
                        return Err(ProductionChannelStepError::Operations(
                            OperationsError::CommissioningFailed,
                        ))
                    }
                }
            }
            self.operations
                .fail_active(OperationsError::CommissioningDeadline)?;
            return Ok(None);
        }

        if let CommissioningExecution::Scan {
            round_nonce,
            slot_count,
            next_slot,
            active_slot,
            discovered,
            ..
        } = &mut execution
        {
            if active_slot.is_none() {
                if *next_slot == *slot_count {
                    self.commissioning_discoveries = discovered.clone();
                    self.operations
                        .complete_active(OperationResult::CommissioningScan {
                            round_nonce: *round_nonce,
                            slot_count: *slot_count,
                            discovered: discovered.clone(),
                        })?;
                    return Ok(None);
                }
                self.recording
                    .endpoint_mut()
                    .agent_mut()
                    .submit_commissioning_probe(sensor_protocol::ProbeSlotRequest {
                        round_nonce: *round_nonce,
                        slot_count: *slot_count,
                        slot_index: *next_slot,
                    })
                    .map_err(|_| {
                        ProductionChannelStepError::Operations(OperationsError::CommissioningFailed)
                    })?;
                *active_slot = Some(*next_slot);
            }
        }

        let started = Instant::now();
        let endpoint_event = self.recording.endpoint_mut().step().map_err(|_| {
            ProductionChannelStepError::Operations(OperationsError::CommissioningFailed)
        })?;
        self.metrics.observe_step(
            &ThreadedChannelRecordingEvent::Endpoint(endpoint_event.clone()),
            started.elapsed(),
        );
        match (&mut execution, endpoint_event) {
            (
                CommissioningExecution::Scan {
                    round_nonce,
                    next_slot,
                    active_slot,
                    discovered,
                    ..
                },
                ChannelEndpointEvent::Agent(ChannelEvent::CommissioningComplete {
                    outcome: crate::commissioning::CommissioningOutcome::Discovered(identity),
                }),
            ) if identity.round_nonce == *round_nonce => {
                if !discovered
                    .iter()
                    .any(|value| value.hardware_id == identity.hardware_id)
                {
                    discovered.push(identity);
                }
                *next_slot = next_slot.saturating_add(1);
                *active_slot = None;
            }
            (
                CommissioningExecution::Scan {
                    round_nonce,
                    next_slot,
                    active_slot,
                    ..
                },
                ChannelEndpointEvent::Agent(ChannelEvent::CommissioningSlotEmpty {
                    round_nonce: observed_nonce,
                    slot_index,
                }),
            ) if observed_nonce == *round_nonce && Some(slot_index) == *active_slot => {
                *next_slot = next_slot.saturating_add(1);
                *active_slot = None;
            }
            (
                CommissioningExecution::Assign {
                    expected,
                    node_address,
                    phase,
                    ..
                },
                ChannelEndpointEvent::Agent(ChannelEvent::CommissioningComplete {
                    outcome: crate::commissioning::CommissioningOutcome::Assigned(assigned),
                }),
            ) if assigned.hardware_id == expected.hardware_id
                && assigned.node_address == *node_address =>
            {
                let mut requirements = HandshakeRequirements::recording_node(
                    expected.hardware_id,
                    *node_address,
                    expected.board_revision,
                    0,
                );
                requirements.required_services |= sensor_protocol::ServiceId::Configuration
                    .capability_bit()
                    | sensor_protocol::ServiceId::Time.capability_bit();
                self.recording
                    .endpoint_mut()
                    .agent_mut()
                    .submit_handshake(requirements)
                    .map_err(|_| {
                        ProductionChannelStepError::Operations(OperationsError::CommissioningFailed)
                    })?;
                *phase = CommissioningAssignPhase::Verifying { assigned };
            }
            (
                CommissioningExecution::Assign {
                    expected,
                    node_address,
                    phase: CommissioningAssignPhase::Verifying { assigned },
                    ..
                },
                ChannelEndpointEvent::Agent(ChannelEvent::HandshakeComplete {
                    node_address: observed_address,
                    identity,
                    ..
                }),
            ) if observed_address == *node_address
                && identity.hardware_id == expected.hardware_id =>
            {
                self.operations
                    .complete_active(OperationResult::AddressAssigned(*assigned))?;
                return Ok(None);
            }
            _ => {}
        }
        Ok(Some(execution))
    }

    fn begin_restart(
        &mut self,
        request: OperationRequest,
    ) -> Result<Option<RestartExecution>, ProductionChannelStepError> {
        let OperationTarget::Node { node_address, .. } = request.target else {
            self.operations
                .fail_active(OperationsError::ExecutorUnavailable)?;
            return Ok(None);
        };
        if !self
            .nodes
            .iter()
            .any(|node| node.id.node_address == node_address)
        {
            self.operations
                .fail_active(OperationsError::NodeNotConfigured)?;
            return Ok(None);
        }
        let operation_id = self
            .recording
            .endpoint_mut()
            .agent_mut()
            .submit_lifecycle(
                node_address,
                sensor_protocol::RuntimeOperation::RestartNode,
                sensor_protocol::ObservabilityProfile::Production,
            )
            .map_err(|_| ProductionChannelStepError::Operations(OperationsError::RestartFailed))?;
        let deadline = Instant::now()
            .checked_add(self.maintenance_policy.service_operation_deadline)
            .ok_or(ChannelMaintenanceError::DeadlineOverflow)?;
        Ok(Some(RestartExecution {
            node_address,
            operation_id,
            deadline,
            settle_until: None,
            terminal_error: None,
        }))
    }

    fn drive_restart(
        &mut self,
        mut execution: RestartExecution,
    ) -> Result<Option<RestartExecution>, ProductionChannelStepError> {
        if let Some(settle_until) = execution.settle_until {
            if Instant::now() < settle_until {
                // The reset response was already received. Do not touch the
                // serial endpoint while the bootloader and application are
                // handing the transceiver back to protocol 5.
                return Ok(Some(execution));
            }
            if matches!(
                self.operations.mode(),
                ChannelOperatingMode::Faulted | ChannelOperatingMode::Offline
            ) {
                self.operations
                    .set_mode(ChannelOperatingMode::Maintenance)?;
            }
            self.operations
                .complete_active(OperationResult::NodeRestartRequested {
                    node_address: execution.node_address,
                })?;
            return Ok(None);
        }
        if execution.terminal_error.is_none() && Instant::now() >= execution.deadline {
            execution.terminal_error = Some(OperationsError::RestartDeadline);
        }
        if let Some(error) = execution.terminal_error {
            let key = ControlWorkKey::Lifecycle {
                node_address: execution.node_address,
                operation_id: execution.operation_id,
            };
            match self
                .recording
                .endpoint_mut()
                .agent_mut()
                .abandon_control_work(key)
            {
                Ok(_) | Err(ChannelAgentError::ControlWorkNotFound(_)) => {
                    self.operations.fail_active(error)?;
                    return Ok(None);
                }
                Err(ChannelAgentError::ControlAttemptInFlight(_)) => return Ok(Some(execution)),
                Err(_) => {
                    return Err(ProductionChannelStepError::Operations(
                        OperationsError::RestartFailed,
                    ))
                }
            }
        }
        let started = Instant::now();
        let endpoint_event = match self.recording.endpoint_mut().step() {
            Ok(value) => value,
            Err(_) => {
                execution.terminal_error = Some(OperationsError::RestartFailed);
                return Ok(Some(execution));
            }
        };
        self.metrics.observe_step(
            &ThreadedChannelRecordingEvent::Endpoint(endpoint_event.clone()),
            started.elapsed(),
        );
        if let ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete {
            node_address,
            result,
        }) = endpoint_event
        {
            if node_address == execution.node_address
                && result.operation_id == execution.operation_id
                && result.code == sensor_protocol::RuntimeResultCode::Ok
                && execution.settle_until.is_none()
            {
                execution.settle_until = Some(
                    Instant::now()
                        .checked_add(NODE_RESTART_SETTLE_TIME)
                        .ok_or(ChannelMaintenanceError::DeadlineOverflow)?,
                );
            }
        }
        Ok(Some(execution))
    }

    fn drive_maintenance(
        &mut self,
        mut execution: MaintenanceEntryExecution,
    ) -> Result<Option<MaintenanceEntryExecution>, ProductionChannelStepError> {
        match execution.plan.directive() {
            MaintenanceEntryDirective::Quiesce => {
                if self.recording.is_quiescent() {
                    execution.plan.accept(MaintenanceEntryEvent::Quiesced)?;
                    execution.group =
                        Some(self.safe_state_group(SafeStateTarget::Idle, &execution.contracts)?);
                } else if Instant::now() >= execution.barrier_deadline {
                    self.fail_maintenance(
                        ChannelOperatingMode::Faulted,
                        OperationsError::MaintenanceBarrierDeadline,
                    )?;
                    return Ok(None);
                } else {
                    self.step_recording()?;
                }
            }
            MaintenanceEntryDirective::Converge(target) => {
                let group = execution
                    .group
                    .as_mut()
                    .ok_or(ChannelMaintenanceError::MissingSafeStateGroup)?;
                let now = self.recording.endpoint_mut().now();
                match group.drive(self.recording.endpoint_mut().agent_mut(), now)? {
                    SafeStateGroupProgress::Complete => {
                        execution
                            .plan
                            .accept(MaintenanceEntryEvent::SafeStateReached(target))?;
                    }
                    SafeStateGroupProgress::NodeConvergenceFailed { .. }
                        if target == SafeStateTarget::Maintenance =>
                    {
                        execution
                            .plan
                            .accept(MaintenanceEntryEvent::MaintenanceConvergenceFailed)?;
                        group.fall_back_to_idle()?;
                    }
                    SafeStateGroupProgress::NodeConvergenceFailed { .. } => {
                        self.fail_maintenance(
                            ChannelOperatingMode::Faulted,
                            OperationsError::MaintenanceConvergenceFailed,
                        )?;
                        return Ok(None);
                    }
                    SafeStateGroupProgress::NodeOffline { .. } => {
                        execution.plan.accept(MaintenanceEntryEvent::NodeOffline)?;
                        self.fail_maintenance(
                            ChannelOperatingMode::Offline,
                            OperationsError::NodeOffline,
                        )?;
                        return Ok(None);
                    }
                    SafeStateGroupProgress::Node { .. }
                    | SafeStateGroupProgress::NodeReached { .. } => {
                        self.step_safe_state_endpoint(group)?;
                    }
                }
            }
            MaintenanceEntryDirective::ReconcileStoppedStreams => {
                let observed = execution
                    .group
                    .as_ref()
                    .ok_or(ChannelMaintenanceError::MissingSafeStateGroup)?
                    .contracts();
                if observed != execution.contracts {
                    self.fail_maintenance(
                        ChannelOperatingMode::Faulted,
                        OperationsError::StreamReconciliationRequired,
                    )?;
                    return Ok(None);
                }
                execution.contracts = observed;
                execution.group = None;
                execution
                    .plan
                    .accept(MaintenanceEntryEvent::StoppedStreamsReconciled)?;
            }
            MaintenanceEntryDirective::DrainStoppedStreams => {
                if !execution.drain_started {
                    self.recording.begin_shutdown_drain()?;
                    execution.drain_deadline = Some(
                        Instant::now()
                            .checked_add(self.maintenance_policy.drain_deadline)
                            .ok_or(ChannelMaintenanceError::DeadlineOverflow)?,
                    );
                    execution.drain_started = true;
                }
                if self.recording.is_shutdown_drain_complete() {
                    self.recording.finish_shutdown_drain()?;
                    execution
                        .plan
                        .accept(MaintenanceEntryEvent::StoppedStreamsDrained)?;
                    execution.group = Some(
                        self.safe_state_group(SafeStateTarget::Maintenance, &execution.contracts)?,
                    );
                } else if execution
                    .drain_deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    self.recording.abort_shutdown_drain()?;
                    self.fail_maintenance(
                        ChannelOperatingMode::Faulted,
                        OperationsError::MaintenanceDrainDeadline,
                    )?;
                    return Ok(None);
                } else {
                    self.step_recording()?;
                }
            }
            MaintenanceEntryDirective::CompleteMaintenance => {
                self.operations
                    .set_mode(ChannelOperatingMode::Maintenance)?;
                if execution.finish_active_operation {
                    self.operations.finish_active(true)?;
                }
                return Ok(None);
            }
            MaintenanceEntryDirective::CompleteIdle => {
                self.fail_maintenance(
                    ChannelOperatingMode::Faulted,
                    OperationsError::MaintenanceConvergenceFailed,
                )?;
                return Ok(None);
            }
            MaintenanceEntryDirective::Offline => {
                self.fail_maintenance(ChannelOperatingMode::Offline, OperationsError::NodeOffline)?;
                return Ok(None);
            }
        }
        Ok(Some(execution))
    }

    fn safe_state_group(
        &self,
        target: SafeStateTarget,
        contracts: &[SafeStateNodeContract],
    ) -> Result<SafeStateGroupOperation, ChannelMaintenanceError> {
        SafeStateGroupOperation::new(
            target,
            self.maintenance_policy.safe_state,
            self.maintenance_policy.wire,
            contracts,
        )
        .map_err(ChannelMaintenanceError::SafeStateGroup)
    }

    fn step_recording(&mut self) -> Result<(), ProductionChannelStepError> {
        let started = Instant::now();
        let event = self
            .recording
            .step()
            .map_err(ProductionChannelStepError::Recording)?;
        self.metrics.observe_step(&event, started.elapsed());
        Ok(())
    }

    fn step_safe_state_endpoint(
        &mut self,
        group: &mut SafeStateGroupOperation,
    ) -> Result<(), ProductionChannelStepError> {
        let started = Instant::now();
        let endpoint_event = self
            .recording
            .endpoint_mut()
            .step()
            .map_err(ThreadedChannelRecordingLoopError::Endpoint)
            .map_err(ProductionChannelStepError::Recording)?;
        if let ChannelEndpointEvent::Agent(event) = &endpoint_event {
            let now = self.recording.endpoint_mut().now();
            group.handle_event(event, now)?;
        }
        self.metrics.observe_step(
            &ThreadedChannelRecordingEvent::Endpoint(endpoint_event),
            started.elapsed(),
        );
        Ok(())
    }

    fn drive_resume(
        &mut self,
        mut execution: ResumeRecordingExecution,
    ) -> Result<Option<ResumeRecordingExecution>, ProductionChannelStepError> {
        match execution.plan.directive() {
            ResumeRecordingDirective::VerifyNodes => {
                let now = self.recording.endpoint_mut().now();
                let progress = match execution
                    .verification
                    .drive(self.recording.endpoint_mut().agent_mut(), now)
                {
                    Ok(progress) => progress,
                    Err(_) => {
                        self.fail_resume(
                            ChannelOperatingMode::Faulted,
                            OperationsError::ResumeVerificationFailed,
                        )?;
                        return Ok(None);
                    }
                };
                match progress {
                    ResumeVerificationProgress::Complete => {
                        let stream_recovery_required =
                            execution.verification.stream_recovery_required()?;
                        if !stream_recovery_required {
                            for evidence in execution.verification.evidence().iter().copied() {
                                if self
                                    .recording
                                    .reconcile_resume_stream(
                                        evidence.identity.node_address,
                                        evidence.capabilities.journal_capacity_samples,
                                        evidence.stream,
                                    )
                                    .is_err()
                                {
                                    self.fail_resume(
                                        ChannelOperatingMode::Faulted,
                                        OperationsError::ResumeVerificationFailed,
                                    )?;
                                    return Ok(None);
                                }
                            }
                        }
                        execution.plan.accept(ResumeRecordingEvent::NodesVerified {
                            stream_recovery_required,
                        })?;
                    }
                    ResumeVerificationProgress::NodeOffline { .. } => {
                        execution.plan.accept(ResumeRecordingEvent::NodeOffline)?;
                    }
                    ResumeVerificationProgress::Submitted { .. }
                    | ResumeVerificationProgress::Waiting { .. }
                    | ResumeVerificationProgress::AwaitingWireSettlement { .. } => {
                        let started = Instant::now();
                        let endpoint_event = match self.recording.endpoint_mut().step() {
                            Ok(event) => event,
                            Err(_) => {
                                self.fail_resume(
                                    ChannelOperatingMode::Faulted,
                                    OperationsError::ResumeVerificationFailed,
                                )?;
                                return Ok(None);
                            }
                        };
                        if let ChannelEndpointEvent::Agent(event) = &endpoint_event {
                            if execution.verification.handle_event(event).is_err() {
                                self.fail_resume(
                                    ChannelOperatingMode::Faulted,
                                    OperationsError::ResumeVerificationFailed,
                                )?;
                                return Ok(None);
                            }
                        }
                        self.metrics.observe_step(
                            &ThreadedChannelRecordingEvent::Endpoint(endpoint_event),
                            started.elapsed(),
                        );
                    }
                    ResumeVerificationProgress::NodeVerified { .. } => {}
                }
            }
            ResumeRecordingDirective::ReconcileStreams => {
                let deadline = match execution.reconciliation_deadline {
                    Some(deadline) => deadline,
                    None => {
                        let deadline = Instant::now()
                            .checked_add(self.maintenance_policy.barrier_deadline)
                            .ok_or(ChannelMaintenanceError::DeadlineOverflow)?;
                        execution.reconciliation_deadline = Some(deadline);
                        deadline
                    }
                };
                if Instant::now() >= deadline {
                    return self.fail_stream_reconciliation();
                }
                if let Some(pending) = execution.pending_time.take() {
                    let mut pending = pending;
                    if let Some(ticket) = pending.time_ticket.as_ref() {
                        match ticket.try_result() {
                            Ok(None) => {}
                            Ok(Some(Ok(result)))
                                if result.stream == pending.expected_time_stream =>
                            {
                                pending.time_ticket = None;
                            }
                            Ok(Some(Ok(_))) | Ok(Some(Err(_))) | Err(_) => {
                                return self.fail_stream_reconciliation();
                            }
                        }
                    }
                    if let Some(ticket) = pending.telemetry_ticket.as_ref() {
                        match ticket.try_result() {
                            Ok(None) => {}
                            Ok(Some(Ok(result)))
                                if Some(result.stream) == pending.expected_telemetry_stream =>
                            {
                                pending.telemetry_ticket = None;
                            }
                            Ok(Some(Ok(_))) | Ok(Some(Err(_))) | Err(_) => {
                                return self.fail_stream_reconciliation();
                            }
                        }
                    }
                    if pending.time_ticket.is_none() && pending.telemetry_ticket.is_none() {
                        let evidence = pending.evidence;
                        if self
                            .recording
                            .replace_resume_stream(
                                evidence.identity.node_address,
                                ExpectedStreamState {
                                    hardware_id: evidence.identity.hardware_id,
                                    boot_epoch: evidence.identity.boot_epoch,
                                    journal_capacity_samples: evidence
                                        .capabilities
                                        .journal_capacity_samples,
                                },
                                pending.capture_durable,
                                evidence.stream,
                            )
                            .is_err()
                        {
                            return self.fail_stream_reconciliation();
                        }
                        let Some(node) = self
                            .nodes
                            .iter_mut()
                            .find(|node| node.id.node_address == evidence.identity.node_address)
                        else {
                            return self.fail_stream_reconciliation();
                        };
                        node.boot_epoch = evidence.identity.boot_epoch;
                        execution.reconciled_nodes += 1;
                    } else {
                        execution.pending_time = Some(pending);
                    }
                } else if execution.reconciled_nodes == execution.verification.evidence().len() {
                    execution
                        .plan
                        .accept(ResumeRecordingEvent::StreamsReconciled)?;
                } else {
                    let evidence = execution.verification.evidence()[execution.reconciled_nodes];
                    let Some(node) = self
                        .nodes
                        .iter()
                        .find(|node| node.id.node_address == evidence.identity.node_address)
                        .copied()
                    else {
                        return self.fail_stream_reconciliation();
                    };
                    if !evidence.stream_recovery_required {
                        if self
                            .recording
                            .reconcile_resume_stream(
                                evidence.identity.node_address,
                                evidence.capabilities.journal_capacity_samples,
                                evidence.stream,
                            )
                            .is_err()
                        {
                            return self.fail_stream_reconciliation();
                        }
                        execution.reconciled_nodes += 1;
                    } else {
                        match self.begin_epoch_rollover(node, evidence) {
                            Ok(pending) => execution.pending_time = pending,
                            Err(_) => {
                                return self.fail_stream_reconciliation();
                            }
                        }
                    }
                }
            }
            ResumeRecordingDirective::CheckAdmission => {
                if !self.configuration_plan.admission().accepted() {
                    execution.plan.accept(ResumeRecordingEvent::Failed)?;
                } else {
                    execution
                        .plan
                        .accept(ResumeRecordingEvent::AdmissionAccepted)?;
                    let contracts = self
                        .nodes
                        .iter()
                        .map(|node| SafeStateNodeContract {
                            requirements: node.requirements,
                            boot_epoch: node.boot_epoch,
                        })
                        .collect::<Vec<_>>();
                    execution.activation =
                        Some(self.safe_state_group(SafeStateTarget::Recording, &contracts)?);
                }
            }
            ResumeRecordingDirective::ActivateNodes => {
                let group = execution
                    .activation
                    .as_mut()
                    .ok_or(ChannelMaintenanceError::MissingSafeStateGroup)?;
                let now = self.recording.endpoint_mut().now();
                match group.drive(self.recording.endpoint_mut().agent_mut(), now)? {
                    SafeStateGroupProgress::Complete => {
                        execution
                            .plan
                            .accept(ResumeRecordingEvent::NodesActivated)?;
                    }
                    SafeStateGroupProgress::NodeOffline { .. } => {
                        execution.plan.accept(ResumeRecordingEvent::NodeOffline)?;
                    }
                    SafeStateGroupProgress::NodeConvergenceFailed { .. } => {
                        execution.plan.accept(ResumeRecordingEvent::Failed)?;
                    }
                    SafeStateGroupProgress::Node { .. }
                    | SafeStateGroupProgress::NodeReached { .. } => {
                        self.step_safe_state_endpoint(group)?;
                    }
                }
            }
            ResumeRecordingDirective::ResumeScheduling => {
                self.recording.resume_scheduling()?;
                execution
                    .plan
                    .accept(ResumeRecordingEvent::SchedulingResumed)?;
            }
            ResumeRecordingDirective::Complete => {
                self.operations.set_mode(ChannelOperatingMode::Recording)?;
                if let Some(result) = execution.completion_result.take() {
                    self.operations.complete_active(result)?;
                } else {
                    self.operations.finish_active(true)?;
                }
                return Ok(None);
            }
            ResumeRecordingDirective::Faulted => {
                let error = if self.configuration_plan.admission().accepted() {
                    OperationsError::ResumeActivationFailed
                } else {
                    OperationsError::ResumeAdmissionRejected
                };
                self.fail_resume(ChannelOperatingMode::Faulted, error)?;
                return Ok(None);
            }
            ResumeRecordingDirective::Offline => {
                self.fail_resume(ChannelOperatingMode::Offline, OperationsError::NodeOffline)?;
                return Ok(None);
            }
        }
        Ok(Some(execution))
    }

    fn fail_maintenance(
        &mut self,
        mode: ChannelOperatingMode,
        error: OperationsError,
    ) -> Result<(), ProductionChannelStepError> {
        self.operations.set_mode(mode)?;
        self.operations.fail_active(error)?;
        Ok(())
    }

    fn fail_resume(
        &mut self,
        mode: ChannelOperatingMode,
        error: OperationsError,
    ) -> Result<(), ProductionChannelStepError> {
        self.operations.set_mode(mode)?;
        self.operations.fail_active(error)?;
        Ok(())
    }

    fn fail_stream_reconciliation(
        &mut self,
    ) -> Result<Option<ResumeRecordingExecution>, ProductionChannelStepError> {
        self.fail_resume(
            ChannelOperatingMode::Maintenance,
            OperationsError::StreamReconciliationRequired,
        )?;
        Ok(None)
    }

    fn begin_epoch_rollover(
        &self,
        node: ChannelControlNode,
        evidence: crate::channel_resume::ResumeNodeEvidence,
    ) -> Result<Option<PendingEpochRollover>, ChannelMaintenanceError> {
        let bootstrap = NodeBootstrapEvidence {
            id: node.id,
            identity: evidence.identity,
            capabilities: evidence.capabilities,
            configuration: node.configuration,
            runtime: evidence.runtime,
            stream: evidence.stream,
        };
        let origin = self
            .rollover
            .capture
            .inspect_stream_origin(
                &self.rollover.channel_name,
                evidence.identity.hardware_id,
                evidence.identity.boot_epoch,
            )
            .map_err(ChannelMaintenanceError::CaptureProfile)?;
        let prepared = prepare_capture_stream(bootstrap, origin)
            .map_err(ChannelMaintenanceError::CaptureRecovery)?;
        let capture_spec = self
            .rollover
            .capture
            .stream_spec(&self.rollover.channel_name, prepared.stream)
            .map_err(ChannelMaintenanceError::CaptureProfile)?;
        let capture = self
            .recording
            .persistence()
            .rollover_capture_stream(node.boot_epoch, capture_spec)
            .map_err(ChannelMaintenanceError::CaptureRollover)?;
        let next_time = self
            .rollover
            .time
            .stream_spec(
                self.rollover.host_clock_id,
                DiscoveredTimeStream {
                    channel_name: self.rollover.channel_name.clone(),
                    hardware_id: evidence.identity.hardware_id,
                    node_address: evidence.identity.node_address,
                    boot_epoch: evidence.identity.boot_epoch,
                },
            )
            .map_err(ChannelMaintenanceError::TimeProfile)?;
        let previous_time = TimeStreamKey {
            hardware_id: evidence.identity.hardware_id,
            node_address: evidence.identity.node_address,
            boot_epoch: node.boot_epoch,
        };
        let ticket = match self
            .rollover
            .time_client
            .try_request(previous_time, next_time)
        {
            Ok(ticket) => ticket,
            Err(TimeRolloverSubmitError::QueueFull) => return Ok(None),
            Err(TimeRolloverSubmitError::Disconnected) => {
                return Err(ChannelMaintenanceError::TimeRolloverDisconnected)
            }
        };
        let telemetry = if evidence
            .capabilities
            .supports_service(sensor_protocol::ServiceId::Telemetry)
        {
            match self.rollover.telemetry_client.as_ref() {
                Some(client) => {
                    let previous = TelemetryStreamKey {
                        hardware_id: evidence.identity.hardware_id,
                        node_address: evidence.identity.node_address,
                        boot_epoch: node.boot_epoch,
                    };
                    let next = self
                        .rollover
                        .telemetry
                        .stream_spec(DiscoveredTelemetryStream {
                            channel_name: self.rollover.channel_name.clone(),
                            hardware_id: evidence.identity.hardware_id,
                            node_address: evidence.identity.node_address,
                            boot_epoch: evidence.identity.boot_epoch,
                        })
                        .map_err(ChannelMaintenanceError::TelemetryProfile)?;
                    let expected = TelemetryStreamKey::from_config(next.config);
                    let ticket = match client.try_request(previous, next) {
                        Ok(ticket) => ticket,
                        Err(TelemetryRolloverSubmitError::QueueFull) => return Ok(None),
                        Err(TelemetryRolloverSubmitError::Disconnected) => {
                            return Err(ChannelMaintenanceError::TelemetryRolloverDisconnected)
                        }
                    };
                    (Some(expected), Some(ticket))
                }
                None => return Err(ChannelMaintenanceError::TelemetryRolloverDisconnected),
            }
        } else {
            (None, None)
        };
        Ok(Some(PendingEpochRollover {
            evidence,
            capture_durable: capture.durable,
            expected_time_stream: TimeStreamKey {
                hardware_id: evidence.identity.hardware_id,
                node_address: evidence.identity.node_address,
                boot_epoch: evidence.identity.boot_epoch,
            },
            time_ticket: Some(ticket),
            expected_telemetry_stream: telemetry.0,
            telemetry_ticket: telemetry.1,
        }))
    }
}

impl<P, C, T> SupervisedChannelTask for ProductionChannelTask<P, C, T>
where
    P: Read + Write + Send + 'static,
    C: MonotonicClock + Send + 'static,
    T: TimeObservationSubmitter + Send + 'static,
{
    type StepError = ProductionChannelStepError;
    type Shutdown = ControlledChannelShutdownReport;

    fn step(&mut self) -> Result<(), Self::StepError> {
        match self
            .operations
            .poll_command()
            .map_err(ProductionChannelStepError::Operations)?
        {
            ChannelCommandDecision::Started(request) => {
                if request.kind == OperationKind::EnterMaintenance && self.maintenance.is_none() {
                    self.recording.begin_quiesce();
                    self.operations
                        .set_mode(ChannelOperatingMode::EnteringMaintenance)
                        .map_err(ProductionChannelStepError::Operations)?;
                    self.maintenance = Some(MaintenanceEntryExecution::new(
                        &self.nodes,
                        self.maintenance_policy,
                        true,
                    )?);
                } else if request.kind == OperationKind::ResumeRecording && self.resume.is_none() {
                    self.operations
                        .set_mode(ChannelOperatingMode::ResumingRecording)
                        .map_err(ProductionChannelStepError::Operations)?;
                    self.resume = Some(ResumeRecordingExecution::new(
                        &self.nodes,
                        self.maintenance_policy,
                        None,
                    )?);
                } else if matches!(
                    request.kind,
                    OperationKind::GetConfiguration
                        | OperationKind::ValidateConfiguration { .. }
                        | OperationKind::ApplyConfiguration { .. }
                        | OperationKind::ResetConfiguration { .. }
                ) && self.configuration.is_none()
                {
                    if self.operations.mode() == ChannelOperatingMode::Recording
                        && configuration_requires_quiesce(&request.kind)
                    {
                        self.begin_automatic_configuration(request)?;
                    } else {
                        self.configuration = self.begin_configuration(request, true)?;
                    }
                } else if let OperationKind::SetTelemetryInterval { interval_seconds } =
                    request.kind
                {
                    let OperationTarget::Node { node_address, .. } = request.target else {
                        unreachable!("telemetry interval is node-scoped")
                    };
                    let mut candidate = self.configuration_plan.clone();
                    match candidate.accept_telemetry_interval(node_address, interval_seconds) {
                        Ok((_, telemetry)) => {
                            if self
                                .recording
                                .reconfigure_telemetry(node_address, telemetry)
                                .is_err()
                            {
                                self.operations
                                    .fail_active(OperationsError::ConfigurationFailed)?;
                            } else {
                                self.configuration_plan = candidate;
                                self.operations.complete_active(
                                    OperationResult::TelemetryIntervalApplied {
                                        node_address,
                                        interval_seconds,
                                    },
                                )?;
                            }
                        }
                        Err(_) => {
                            self.operations
                                .fail_active(OperationsError::ConfigurationAdmissionRejected)?;
                        }
                    }
                } else if matches!(
                    request.kind,
                    OperationKind::ScanUnassigned { .. } | OperationKind::AssignAddress { .. }
                ) && self.commissioning.is_none()
                {
                    self.commissioning = self.begin_commissioning(request)?;
                } else if request.kind == OperationKind::RestartNode && self.restart.is_none() {
                    self.restart = self.begin_restart(request)?;
                } else {
                    self.operations
                        .fail_active(OperationsError::ExecutorUnavailable)
                        .map_err(ProductionChannelStepError::Operations)?;
                }
            }
            ChannelCommandDecision::None
            | ChannelCommandDecision::Existing { .. }
            | ChannelCommandDecision::Rejected { .. } => {}
        }
        self.advance_automatic_configuration()?;
        if let Some(execution) = self.maintenance.take() {
            self.maintenance = self.drive_maintenance(execution)?;
            return Ok(());
        }
        if let Some(execution) = self.resume.take() {
            self.resume = self.drive_resume(execution)?;
            return Ok(());
        }
        if let Some(execution) = self.configuration.take() {
            self.configuration = self.drive_configuration(execution)?;
            return Ok(());
        }
        if let Some(execution) = self.commissioning.take() {
            self.commissioning = self.drive_commissioning(execution)?;
            return Ok(());
        }
        if let Some(execution) = self.restart.take() {
            self.restart = self.drive_restart(execution)?;
            return Ok(());
        }
        let started = Instant::now();
        let event = self
            .recording
            .step()
            .map_err(ProductionChannelStepError::Recording)?;
        self.metrics.observe_step(&event, started.elapsed());
        Ok(())
    }

    fn shutdown(self) -> Self::Shutdown {
        if self.operations.mode() == ChannelOperatingMode::Maintenance {
            return finalize_quiesced_channel(self.recording);
        }
        let node_ids = self.nodes.iter().map(|node| node.id).collect::<Vec<_>>();
        controlled_shutdown_channel(self.recording, &node_ids, self.shutdown_policy)
    }
}

fn finalize_quiesced_channel<P, C, T>(
    recording: ProductionChannelLoop<P, C, T>,
) -> ControlledChannelShutdownReport
where
    P: Read + Write,
    C: MonotonicClock,
    T: TimeObservationSubmitter,
{
    let mut outcome = if recording.is_quiescent() {
        Ok(())
    } else {
        Err(ControlledChannelShutdownError::MaintenanceNotQuiescent)
    };
    let (_endpoint, _measurements, persistence, _time_observations, _telemetry_observations) =
        recording.into_parts();
    let persistence = match persistence.shutdown() {
        Ok(mut shutdown) => {
            if let Err(error) = shutdown.sink.finish_all_streams() {
                remember_first_shutdown_error(
                    &mut outcome,
                    ControlledChannelShutdownError::CaptureFinalization(error),
                );
            }
            Ok(shutdown)
        }
        Err(error) => Err(error),
    };
    ControlledChannelShutdownReport {
        outcome,
        persistence,
    }
}

#[derive(Debug)]
pub enum ProductionChannelStepError {
    Recording(ThreadedChannelRecordingLoopError<CaptureRepositoryError>),
    Operations(OperationsError),
    Maintenance(ChannelMaintenanceError),
}

impl From<OperationsError> for ProductionChannelStepError {
    fn from(value: OperationsError) -> Self {
        Self::Operations(value)
    }
}

impl From<ChannelMaintenanceError> for ProductionChannelStepError {
    fn from(value: ChannelMaintenanceError) -> Self {
        Self::Maintenance(value)
    }
}

impl From<MeasurementCoordinatorError> for ProductionChannelStepError {
    fn from(value: MeasurementCoordinatorError) -> Self {
        Self::Maintenance(ChannelMaintenanceError::Coordinator(value))
    }
}

impl From<ChannelModeError> for ProductionChannelStepError {
    fn from(value: ChannelModeError) -> Self {
        Self::Maintenance(ChannelMaintenanceError::Mode(value))
    }
}

impl From<ResumeRecordingError> for ProductionChannelStepError {
    fn from(value: ResumeRecordingError) -> Self {
        Self::Maintenance(ChannelMaintenanceError::ResumeMode(value))
    }
}

impl From<ResumeVerificationError> for ProductionChannelStepError {
    fn from(value: ResumeVerificationError) -> Self {
        Self::Maintenance(ChannelMaintenanceError::ResumeVerification(value))
    }
}

impl From<SafeStateGroupError> for ProductionChannelStepError {
    fn from(value: SafeStateGroupError) -> Self {
        Self::Maintenance(ChannelMaintenanceError::SafeStateGroup(value))
    }
}

impl fmt::Display for ProductionChannelStepError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "production channel step error: {self:?}")
    }
}

impl std::error::Error for ProductionChannelStepError {}

#[derive(Debug)]
pub enum ProductionChannelTaskBuildError {
    Shutdown(ControlledChannelShutdownError),
    Maintenance(ChannelMaintenanceError),
    Measurement(MeasurementCoordinatorError),
}

impl From<ControlledChannelShutdownError> for ProductionChannelTaskBuildError {
    fn from(value: ControlledChannelShutdownError) -> Self {
        Self::Shutdown(value)
    }
}

impl From<ChannelMaintenanceError> for ProductionChannelTaskBuildError {
    fn from(value: ChannelMaintenanceError) -> Self {
        Self::Maintenance(value)
    }
}

impl fmt::Display for ProductionChannelTaskBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "production channel task build error: {self:?}")
    }
}

impl std::error::Error for ProductionChannelTaskBuildError {}

#[derive(Debug)]
pub enum ChannelMaintenanceError {
    ZeroBarrierDeadline,
    ZeroDrainDeadline,
    ZeroServiceOperationDeadline,
    ZeroResumeNodeDeadline,
    EmptyChannel,
    DeadlineOverflow,
    MissingSafeStateGroup,
    SafeStatePolicy(crate::safe_state::SafeStateError),
    SafeStateGroup(SafeStateGroupError),
    Mode(ChannelModeError),
    ResumeMode(ResumeRecordingError),
    ResumeVerification(ResumeVerificationError),
    Coordinator(MeasurementCoordinatorError),
    CaptureProfile(CaptureProfileError),
    CaptureRecovery(ChannelRecoveryError),
    CaptureRollover(CaptureRolloverError),
    TimeProfile(crate::time_profile::TimeProfileError),
    TimeRolloverDisconnected,
    TelemetryProfile(crate::telemetry_profile::TelemetryProfileError),
    TelemetryRolloverDisconnected,
}

impl fmt::Display for ChannelMaintenanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "channel maintenance error: {self:?}")
    }
}

impl std::error::Error for ChannelMaintenanceError {}

impl From<ResumeVerificationError> for ChannelMaintenanceError {
    fn from(value: ResumeVerificationError) -> Self {
        Self::ResumeVerification(value)
    }
}

pub struct ControlledChannelShutdownReport {
    pub outcome: Result<(), ControlledChannelShutdownError>,
    pub persistence: Result<
        PersistenceShutdown<CaptureRepository, CaptureRepositoryError>,
        PersistenceExecutorError,
    >,
}

pub fn controlled_shutdown_channel<P, C, T>(
    recording: ProductionChannelLoop<P, C, T>,
    nodes: &[DiscoveryTargetId],
    policy: ControlledChannelShutdownPolicy,
) -> ControlledChannelShutdownReport
where
    P: Read + Write,
    C: MonotonicClock,
    T: TimeObservationSubmitter,
{
    let (recording, mut outcome) = match policy.validate() {
        Ok(policy) => run_controlled_shutdown(recording, nodes, policy),
        Err(error) => (recording, Err(error)),
    };
    let (_endpoint, _measurements, persistence, _time_observations, _telemetry_observations) =
        recording.into_parts();
    let persistence = match persistence.shutdown() {
        Ok(mut shutdown) => {
            if let Err(error) = shutdown.sink.finish_all_streams() {
                remember_first_shutdown_error(
                    &mut outcome,
                    ControlledChannelShutdownError::CaptureFinalization(error),
                );
            }
            Ok(shutdown)
        }
        Err(error) => Err(error),
    };
    ControlledChannelShutdownReport {
        outcome,
        persistence,
    }
}

fn run_controlled_shutdown<P, C, T>(
    mut recording: ProductionChannelLoop<P, C, T>,
    nodes: &[DiscoveryTargetId],
    policy: ControlledChannelShutdownPolicy,
) -> (
    ProductionChannelLoop<P, C, T>,
    Result<(), ControlledChannelShutdownError>,
)
where
    P: Read + Write,
    C: MonotonicClock,
    T: TimeObservationSubmitter,
{
    recording.begin_quiesce();
    let mut outcome = Ok(());
    let barrier_deadline = Instant::now() + policy.barrier_deadline;
    while !recording.is_quiescent() {
        if Instant::now() >= barrier_deadline {
            remember_first_shutdown_error(
                &mut outcome,
                ControlledChannelShutdownError::BarrierDeadline,
            );
            break;
        }
        if let Err(error) = recording.step() {
            remember_first_shutdown_error(
                &mut outcome,
                ControlledChannelShutdownError::BarrierLoop(error),
            );
            break;
        }
    }

    // Stop needs exclusive access to the endpoint while retaining the same
    // coordinator, persistence executor and Time submitter.
    let (mut endpoint, measurements, persistence, time_observations, telemetry_observations) =
        recording.into_parts();
    let stop_result = stop_recording_nodes(&mut endpoint, nodes, policy.node);
    let mut recording = ThreadedChannelRecordingLoop::new_with_time_observations(
        endpoint,
        measurements,
        persistence,
        time_observations,
    )
    .with_telemetry_observations(telemetry_observations);
    if let Err(error) = stop_result {
        remember_first_shutdown_error(&mut outcome, ControlledChannelShutdownError::Stop(error));
        return (recording, outcome);
    }

    if let Err(error) = recording.begin_shutdown_drain() {
        remember_first_shutdown_error(
            &mut outcome,
            ControlledChannelShutdownError::DrainCoordinator(error),
        );
        return (recording, outcome);
    }
    let drain_deadline = Instant::now() + policy.drain_deadline;
    while !recording.is_shutdown_drain_complete() {
        if Instant::now() >= drain_deadline {
            remember_first_shutdown_error(
                &mut outcome,
                ControlledChannelShutdownError::DrainDeadline,
            );
            return (recording, outcome);
        }
        if let Err(error) = recording.step() {
            remember_first_shutdown_error(
                &mut outcome,
                ControlledChannelShutdownError::DrainLoop(error),
            );
            return (recording, outcome);
        }
    }
    if let Err(error) = recording.finish_shutdown_drain() {
        remember_first_shutdown_error(
            &mut outcome,
            ControlledChannelShutdownError::DrainCoordinator(error),
        );
    }
    (recording, outcome)
}

fn remember_first_shutdown_error(
    outcome: &mut Result<(), ControlledChannelShutdownError>,
    error: ControlledChannelShutdownError,
) {
    if outcome.is_ok() {
        *outcome = Err(error);
    }
}

#[derive(Debug)]
pub enum ControlledChannelShutdownError {
    ZeroBarrierDeadline,
    ZeroDrainDeadline,
    BarrierDeadline,
    DrainDeadline,
    BarrierLoop(ThreadedChannelRecordingLoopError<CaptureRepositoryError>),
    DrainLoop(ThreadedChannelRecordingLoopError<CaptureRepositoryError>),
    DrainCoordinator(MeasurementCoordinatorError),
    Stop(RecordingShutdownError),
    CaptureFinalization(CaptureRepositoryError),
    MaintenanceNotQuiescent,
}

impl fmt::Display for ControlledChannelShutdownError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "controlled channel shutdown error: {self:?}")
    }
}

impl std::error::Error for ControlledChannelShutdownError {}

/// Performs the irreversible ordering constraints for one physical channel:
/// persistent port ownership, Capture replay, cursor reconciliation, lifecycle
/// activation, scheduler assembly, and only then recording-loop construction.
pub fn compose_channel_recording<P, C, T>(
    bootstrapped: BootstrappedChannel<P, C>,
    startup: &ChannelStartupPlan,
    capture: &CaptureInstallationProfile,
    policy: ChannelCompositionPolicy,
    work_id_seed: u64,
    time_observations: T,
    telemetry_observations: Box<dyn TelemetryObservationSubmitter>,
) -> Result<ProductionChannelLoop<P, C, T>, ChannelWorkerError>
where
    P: Read + Write,
    C: MonotonicClock,
    T: TimeObservationSubmitter,
{
    validate_bootstrap(startup, bootstrapped.nodes())?;
    let (mut endpoint, bootstrap_nodes) = bootstrapped.into_parts();
    let mut streams = Vec::with_capacity(bootstrap_nodes.len());
    for node in &bootstrap_nodes {
        append_configuration_snapshot(
            capture.root(),
            ConfigurationSnapshot {
                hardware_id: node.identity.hardware_id,
                boot_epoch: node.identity.boot_epoch,
                observed: node.configuration,
                effective_sample_seq: node.stream.produced.get(),
            },
        )
        .map_err(ChannelWorkerError::ConfigurationHistory)?;
        let origin = capture
            .inspect_stream_origin(
                startup.channel_name(),
                node.identity.hardware_id,
                node.identity.boot_epoch,
            )
            .map_err(ChannelWorkerError::CaptureProfile)?;
        streams.push(
            prepare_capture_stream(*node, origin)
                .map_err(ChannelWorkerError::PrepareCapture)?
                .stream,
        );
    }

    let runtime = capture
        .runtime_config(startup.channel_name(), streams)
        .map_err(ChannelWorkerError::CaptureProfile)?
        .start()
        .map_err(ChannelWorkerError::CaptureRuntime)?;
    let (recovered_streams, executor) = runtime.into_parts();

    let mut recovered_nodes = match reconcile_channel_streams(&bootstrap_nodes, &recovered_streams)
    {
        Ok(nodes) => nodes,
        Err(source) => {
            let shutdown = executor.shutdown().err();
            return Err(ChannelWorkerError::Reconcile { source, shutdown });
        }
    };
    let takeover_required = startup_takeover_required(&bootstrap_nodes, &recovered_nodes);
    let activation_targets = startup
        .nodes()
        .iter()
        .map(|node| DiscoveryTargetId {
            channel_id: node.discovered.channel_id,
            node_address: node.discovered.node_address,
        })
        .collect::<Vec<_>>();

    if let Err(source) =
        activate_recording_nodes(&mut endpoint, &mut recovered_nodes, policy.activation)
    {
        let rollback = stop_recording_nodes(
            &mut endpoint,
            &activation_targets,
            RecordingShutdownPolicy {
                node_deadline: policy.activation.node_deadline,
            },
        )
        .err();
        let shutdown = executor.shutdown().err();
        return Err(ChannelWorkerError::Activation {
            source,
            rollback,
            shutdown,
        });
    }

    let now = endpoint.now();
    let coordinator = match assemble_recording_coordinator(
        startup,
        recovered_nodes,
        policy.assembly,
        now,
        work_id_seed,
    ) {
        Ok(value) => value,
        Err(source) => {
            let rollback = stop_recording_nodes(
                &mut endpoint,
                &activation_targets,
                RecordingShutdownPolicy {
                    node_deadline: policy.activation.node_deadline,
                },
            )
            .err();
            let shutdown = executor.shutdown().err();
            return Err(ChannelWorkerError::Assembly {
                source,
                rollback,
                shutdown,
            });
        }
    };

    let recording = ThreadedChannelRecordingLoop::new_with_time_observations(
        endpoint,
        coordinator,
        executor,
        time_observations,
    )
    .with_telemetry_observations(telemetry_observations);
    if !takeover_required {
        return Ok(recording);
    }

    // A process restart can discover a node that kept recording while the
    // host was down. Stop production first and drain the finite retained tail
    // through the recovered durable sessions. Starting ordinary scheduling
    // directly from a full journal can otherwise keep Measurement permanently
    // urgent and starve TimeSync even when the channel has ample steady-state
    // capacity.
    let (recording, takeover) =
        run_controlled_shutdown(recording, &activation_targets, policy.takeover);
    if let Err(source) = takeover {
        let (_endpoint, _measurements, persistence, _time_observations, _telemetry_observations) =
            recording.into_parts();
        return Err(ChannelWorkerError::TakeoverDrain {
            source,
            shutdown: persistence.shutdown().err(),
        });
    }

    let (mut endpoint, measurements, persistence, time_observations, telemetry_observations) =
        recording.into_parts();
    if let Err(source) =
        start_recording_nodes(&mut endpoint, &activation_targets, policy.activation)
    {
        let rollback = stop_recording_nodes(
            &mut endpoint,
            &activation_targets,
            RecordingShutdownPolicy {
                node_deadline: policy.activation.node_deadline,
            },
        )
        .err();
        return Err(ChannelWorkerError::TakeoverActivation {
            source,
            rollback,
            shutdown: persistence.shutdown().err(),
        });
    }
    let mut recording = ThreadedChannelRecordingLoop::new_with_time_observations(
        endpoint,
        measurements,
        persistence,
        time_observations,
    )
    .with_telemetry_observations(telemetry_observations);
    if let Err(source) = recording.resume_scheduling() {
        let (_endpoint, _measurements, persistence, _time_observations, _telemetry_observations) =
            recording.into_parts();
        return Err(ChannelWorkerError::TakeoverResume {
            source,
            shutdown: persistence.shutdown().err(),
        });
    }
    Ok(recording)
}

/// A startup drain is required whenever acquisition was already active or
/// durable Capture trails the node's observed producer cursor. The latter can
/// happen when a previous process stopped the node but did not finish draining
/// its journal. Starting acquisition before that finite tail is drained can
/// keep a full journal permanently ahead of the host: every pull then records
/// another small overflow gap and TimeSync never gets a schedulable slot.
fn startup_takeover_required(
    bootstrap_nodes: &[NodeBootstrapEvidence],
    recovered_nodes: &[crate::channel_recovery::RecoveredNode],
) -> bool {
    bootstrap_nodes.iter().any(|node| {
        node.runtime.state == RuntimeState::Recording
            || recovered_nodes.iter().any(|recovered| {
                recovered.id() == node.id
                    && recovered.session().cursors().received() < node.stream.produced
            })
    })
}

fn validate_bootstrap(
    startup: &ChannelStartupPlan,
    nodes: &[NodeBootstrapEvidence],
) -> Result<(), ChannelWorkerError> {
    if nodes.len() != startup.nodes().len() {
        return Err(ChannelWorkerError::BootstrapNodeCountMismatch {
            planned: startup.nodes().len(),
            actual: nodes.len(),
        });
    }
    for planned in startup.nodes() {
        let node = nodes
            .iter()
            .find(|node| {
                node.id.channel_id == planned.discovered.channel_id
                    && node.id.node_address == planned.discovered.node_address
            })
            .ok_or(ChannelWorkerError::MissingBootstrapNode {
                channel_id: planned.discovered.channel_id,
                node_address: planned.discovered.node_address,
            })?;
        if node.identity.hardware_id != planned.discovered.hardware_id
            || node.identity.boot_epoch != planned.discovered.boot_epoch
        {
            return Err(ChannelWorkerError::BootstrapIdentityMismatch {
                channel_id: planned.discovered.channel_id,
                node_address: planned.discovered.node_address,
            });
        }
    }
    Ok(())
}

#[derive(Debug)]
pub enum ChannelWorkerError {
    BootstrapNodeCountMismatch {
        planned: usize,
        actual: usize,
    },
    MissingBootstrapNode {
        channel_id: u8,
        node_address: u8,
    },
    BootstrapIdentityMismatch {
        channel_id: u8,
        node_address: u8,
    },
    CaptureProfile(CaptureProfileError),
    PrepareCapture(ChannelRecoveryError),
    CaptureRuntime(CaptureRuntimeError),
    ConfigurationHistory(ConfigurationHistoryError),
    Reconcile {
        source: ChannelRecoveryError,
        shutdown: Option<PersistenceExecutorError>,
    },
    Activation {
        source: RecordingActivationError,
        rollback: Option<RecordingShutdownError>,
        shutdown: Option<PersistenceExecutorError>,
    },
    Assembly {
        source: ChannelAssemblyError,
        rollback: Option<RecordingShutdownError>,
        shutdown: Option<PersistenceExecutorError>,
    },
    TakeoverDrain {
        source: ControlledChannelShutdownError,
        shutdown: Option<PersistenceExecutorError>,
    },
    TakeoverActivation {
        source: RecordingActivationError,
        rollback: Option<RecordingShutdownError>,
        shutdown: Option<PersistenceExecutorError>,
    },
    TakeoverResume {
        source: MeasurementCoordinatorError,
        shutdown: Option<PersistenceExecutorError>,
    },
}

impl fmt::Display for ChannelWorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "channel worker startup error: {self:?}")
    }
}

impl std::error::Error for ChannelWorkerError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use sensor_core::{BootEpoch, HardwareId, SampleAxes, SampleAxis, SequenceCursor};
    use sensor_protocol::{
        decode_lifecycle_request, encode_lifecycle_response, encode_pull_response, FirmwareVersion,
        Frame, FrameRole, NodeCapabilities, NodeIdentity, ObservabilityProfile, OutputRateCode,
        PullResponse, RuntimeOperation, RuntimeResult, RuntimeResultCode, RuntimeState,
        SampleChunk, SampleChunkHeader, SensorStatusCode, ServiceId, StreamDecoder, StreamState,
        TimingEvidence, CAPABILITIES_SCHEMA_VERSION, HOST_ADDRESS, LIFECYCLE_SERVICE_ID,
        LOCAL_PROTOCOL_VERSION, MAX_PAYLOAD_BYTES, MAX_WIRE_FRAME_BYTES, TIMING_VALID,
    };
    use serde_json::json;

    use crate::capture_segments::{capture_segment_paths, validate_capture_segment};
    use crate::capture_window::{CaptureWindowPolicy, WindowSealReason};
    use crate::channel_admission::AdmissionPolicy;
    use crate::channel_agent::{ChannelAgent, ChannelPolicy};
    use crate::channel_bootstrap::NodeBootstrapEvidence;
    use crate::channel_endpoint::{ChannelEndpoint, MonotonicClock, SystemMonotonicClock};
    use crate::channel_port::ChannelPort;
    use crate::channel_runtime::TimeSyncScheduleConfig;
    use crate::channel_scheduler::ChannelSchedulerPolicy;
    use crate::channel_startup::{build_channel_startup_plans, ChannelStartupPolicy};
    use crate::discovery::{
        DiscoveryTargetId, InstallationDiscoveryPlan, REQUIRED_RECORDING_FEATURES,
    };
    use crate::installation_topology::InstallationTopologyProfile;
    use crate::persistence_executor::GroupCommitPolicy;
    use crate::time_persistence::RejectingTimeObservationSubmitter;
    use crate::transaction::DurationMicros;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    const HARDWARE: HardwareId = HardwareId::from_bytes([1, 1, 2, 3, 4, 5, 6, 7]);
    const EPOCH: BootEpoch = BootEpoch::new(101);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sensor-host-channel-worker-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct StartBus {
        reads: VecDeque<Vec<u8>>,
        decoder: StreamDecoder,
        starts: usize,
        stops: Arc<AtomicUsize>,
        pulls: usize,
    }

    impl Read for StartBus {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let Some(bytes) = self.reads.pop_front() else {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no response"));
            };
            let count = bytes.len().min(output.len());
            output[..count].copy_from_slice(&bytes[..count]);
            Ok(count)
        }
    }

    impl Write for StartBus {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            for byte in bytes {
                let Some(decoded) = self.decoder.push(*byte) else {
                    continue;
                };
                let request = decoded.map_err(io::Error::other)?;
                let payload = if request.service == LIFECYCLE_SERVICE_ID {
                    let command =
                        decode_lifecycle_request(request.message, request.payload()).unwrap();
                    let state = match command.operation {
                        RuntimeOperation::Start => {
                            self.starts += 1;
                            RuntimeState::Recording
                        }
                        RuntimeOperation::Stop => {
                            self.stops.fetch_add(1, Ordering::SeqCst);
                            RuntimeState::Idle
                        }
                        _ => return Err(io::Error::other("unexpected lifecycle operation")),
                    };
                    encode_lifecycle_response(RuntimeResult {
                        operation_id: command.operation_id,
                        code: RuntimeResultCode::Ok,
                        state,
                        observability: ObservabilityProfile::Production,
                        sensor_status: SensorStatusCode::Ok,
                    })
                    .unwrap()
                    .to_vec()
                } else if request.service == ServiceId::Measurement as u8 {
                    self.pulls += 1;
                    let mut payload = [0_u8; MAX_PAYLOAD_BYTES];
                    let response = if self.pulls == 1 {
                        let axis = SampleAxis::new(17).unwrap();
                        PullResponse::SampleChunk(SampleChunk {
                            header: SampleChunkHeader {
                                boot_epoch: EPOCH,
                                retained_from: SequenceCursor::new(140),
                                produced: SequenceCursor::new(141),
                                first: SequenceCursor::new(140),
                                sample_count: 1,
                            },
                            config_revision: 1,
                            timing: TimingEvidence {
                                segment_id: 1,
                                first_device_time_us: 1_000,
                                last_device_time_us: 1_000,
                                max_fit_residual_us: 0,
                                quality_flags: TIMING_VALID,
                            },
                            samples: vec![SampleAxes::new(axis, axis, axis)],
                        })
                    } else {
                        PullResponse::NoData(StreamState {
                            boot_epoch: EPOCH,
                            retained_from: SequenceCursor::new(141),
                            produced: SequenceCursor::new(141),
                            released: SequenceCursor::new(141),
                            capacity_samples: 2048,
                            overwritten_samples: 80,
                        })
                    };
                    let length = encode_pull_response(&response, &mut payload).unwrap();
                    payload[..length].to_vec()
                } else {
                    return Err(io::Error::other("unexpected service"));
                };
                let response = Frame::new(
                    FrameRole::Response,
                    HOST_ADDRESS,
                    request.destination,
                    request.service,
                    request.message,
                    request.transaction_id,
                    &payload,
                )
                .unwrap();
                let mut wire = [0_u8; MAX_WIRE_FRAME_BYTES];
                let length = response.encode(&mut wire);
                self.reads.push_back(wire[..length].to_vec());
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn duration(value: u64) -> DurationMicros {
        DurationMicros::new(value).unwrap()
    }

    fn identity() -> NodeIdentity {
        NodeIdentity {
            hardware_id: HARDWARE,
            boot_epoch: EPOCH,
            node_address: 1,
            board_revision: 2,
            firmware: FirmwareVersion {
                major: 0,
                minor: 5,
                patch: 6,
            },
            protocol: LOCAL_PROTOCOL_VERSION,
        }
    }

    fn capabilities() -> NodeCapabilities {
        NodeCapabilities {
            schema_version: CAPABILITIES_SCHEMA_VERSION,
            service_flags: ServiceId::Identity.capability_bit()
                | ServiceId::Measurement.capability_bit()
                | ServiceId::Configuration.capability_bit()
                | ServiceId::Lifecycle.capability_bit()
                | ServiceId::Time.capability_bit(),
            feature_flags: REQUIRED_RECORDING_FEATURES,
            max_payload_bytes: MAX_PAYLOAD_BYTES as u16,
            journal_capacity_samples: 2048,
            output_rate_mask: OutputRateCode::Hz125.capability_bit(),
            max_sample_chunk_samples: 48,
        }
    }

    fn observed_configuration() -> sensor_protocol::ObservedConfiguration {
        sensor_protocol::ObservedConfiguration {
            revision: 1,
            desired: sensor_protocol::MeasurementConfiguration {
                output_rate: OutputRateCode::Hz125,
                range_g: 2,
                high_pass_corner: 0,
                filter_profile: sensor_protocol::DecimationFilterProfile::Balanced,
                fifo_watermark: 30,
                offset_x: 0,
                offset_y: 0,
                offset_z: 0,
            },
            sensor_odr: sensor_protocol::Adxl355Odr::Hz125,
            decimation_factor: 1,
        }
    }

    #[test]
    fn only_mutating_measurement_configuration_requires_quiesce() {
        let desired = observed_configuration().desired;

        assert!(!configuration_requires_quiesce(
            &OperationKind::GetConfiguration
        ));
        assert!(!configuration_requires_quiesce(
            &OperationKind::ValidateConfiguration {
                configuration: desired,
            }
        ));
        assert!(configuration_requires_quiesce(
            &OperationKind::ApplyConfiguration {
                expected_revision: 1,
                configuration: desired,
            }
        ));
        assert!(configuration_requires_quiesce(
            &OperationKind::ResetConfiguration {
                expected_revision: 1,
            }
        ));
    }

    fn topology() -> InstallationTopologyProfile {
        InstallationTopologyProfile::from_document(&json!({
            "system":{"name":"test"},
            "channels":[{
                "name":"line-a", "channel_id":1, "enabled":true,
                "transport":{"kind":"serial-rs485", "device_id":"adapter", "endpoint":"/dev/test", "baud_rate":115200},
                "nodes":[{"id":1,"hardware_id":"0101020304050607","board_revision":2,"enabled":true,"output_rate_hz":125}]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn startup_takeover_drains_a_stopped_node_when_capture_trails_producer() {
        let id = DiscoveryTargetId {
            channel_id: 1,
            node_address: 1,
        };
        let runtime = RuntimeResult {
            operation_id: 1,
            code: RuntimeResultCode::Ok,
            state: RuntimeState::Maintenance,
            observability: ObservabilityProfile::Production,
            sensor_status: SensorStatusCode::Ok,
        };
        let bootstrap = NodeBootstrapEvidence {
            id,
            identity: identity(),
            capabilities: capabilities(),
            configuration: observed_configuration(),
            runtime,
            stream: StreamState {
                boot_epoch: EPOCH,
                retained_from: SequenceCursor::new(120),
                produced: SequenceCursor::new(140),
                released: SequenceCursor::new(100),
                capacity_samples: 2048,
                overwritten_samples: 20,
            },
        };
        let recovered = crate::channel_recovery::RecoveredNode::from_parts(
            id,
            runtime,
            crate::stream_recovery::RecoveryContinuity::JournalGapRequired {
                first_missing: SequenceCursor::new(100),
                missing_count: 20,
            },
            crate::node_session::NodeSession::recovered(
                HARDWARE,
                EPOCH,
                SequenceCursor::new(100),
                SequenceCursor::new(100),
            )
            .unwrap(),
        );

        assert!(startup_takeover_required(&[bootstrap], &[recovered]));
    }

    #[test]
    fn startup_takeover_is_not_needed_for_a_stopped_caught_up_node() {
        let id = DiscoveryTargetId {
            channel_id: 1,
            node_address: 1,
        };
        let runtime = RuntimeResult {
            operation_id: 1,
            code: RuntimeResultCode::Ok,
            state: RuntimeState::Maintenance,
            observability: ObservabilityProfile::Production,
            sensor_status: SensorStatusCode::Ok,
        };
        let bootstrap = NodeBootstrapEvidence {
            id,
            identity: identity(),
            capabilities: capabilities(),
            configuration: observed_configuration(),
            runtime,
            stream: StreamState {
                boot_epoch: EPOCH,
                retained_from: SequenceCursor::new(140),
                produced: SequenceCursor::new(140),
                released: SequenceCursor::new(140),
                capacity_samples: 2048,
                overwritten_samples: 0,
            },
        };
        let recovered = crate::channel_recovery::RecoveredNode::from_parts(
            id,
            runtime,
            crate::stream_recovery::RecoveryContinuity::Contiguous,
            crate::node_session::NodeSession::recovered(
                HARDWARE,
                EPOCH,
                SequenceCursor::new(140),
                SequenceCursor::new(140),
            )
            .unwrap(),
        );

        assert!(!startup_takeover_required(&[bootstrap], &[recovered]));
    }

    #[test]
    fn takeover_stops_drains_and_restarts_a_node_found_recording() {
        let discovery = InstallationDiscoveryPlan::from_topology(&topology());
        let mut collector = discovery.collector();
        collector
            .accept_runtime(
                DiscoveryTargetId {
                    channel_id: 1,
                    node_address: 1,
                },
                identity(),
                capabilities(),
                observed_configuration(),
            )
            .unwrap();
        let inventory = collector.finish().unwrap();
        let startup = build_channel_startup_plans(
            &inventory,
            ChannelStartupPolicy {
                admission: AdmissionPolicy {
                    maximum_utilization_ppm: 900_000,
                    recovery_reserve_ppm: 100_000,
                },
                target_chunk_samples: 32,
                measurement_transaction_time: duration(36_900),
                measurement_slot: duration(40_000),
                maximum_service_interval: duration(500_000),
                retry_base_delay: duration(10_000),
                retry_max_delay: duration(1_000_000),
                retry_attempt_limit: 3,
                offline_probe_interval: duration(5_000_000),
                time_sync_schedule: TimeSyncScheduleConfig::new(
                    duration(30_000_000),
                    duration(4_000),
                    duration(250_000),
                )
                .unwrap(),
                time_sync_transaction_time: duration(4_000),
                telemetry_schedule: crate::channel_runtime::TelemetryScheduleConfig::new(
                    duration(600_000_000),
                    duration(20_000),
                    duration(5_000_000),
                )
                .unwrap(),
            },
        )
        .unwrap();

        let agent = ChannelAgent::new(
            10,
            20,
            duration(50),
            ChannelPolicy {
                transaction_timeout: duration(100_000),
                retry_delay: duration(1_000),
                measurement_attempts_per_slot: 2,
                max_pending_work_items: 2,
            },
        )
        .unwrap();
        let stops = Arc::new(AtomicUsize::new(0));
        let clock = SystemMonotonicClock::new();
        let endpoint = ChannelEndpoint::new(
            agent,
            ChannelPort::from_io(StartBus {
                reads: VecDeque::new(),
                decoder: StreamDecoder::new(),
                starts: 0,
                stops: Arc::clone(&stops),
                pulls: 0,
            }),
            clock.clone(),
        );
        let bootstrapped = BootstrappedChannel::from_parts(
            endpoint,
            vec![NodeBootstrapEvidence {
                id: DiscoveryTargetId {
                    channel_id: 1,
                    node_address: 1,
                },
                identity: identity(),
                capabilities: capabilities(),
                configuration: observed_configuration(),
                runtime: RuntimeResult {
                    operation_id: 1,
                    code: RuntimeResultCode::Ok,
                    state: RuntimeState::Recording,
                    observability: ObservabilityProfile::Production,
                    sensor_status: SensorStatusCode::Ok,
                },
                stream: StreamState {
                    boot_epoch: EPOCH,
                    retained_from: SequenceCursor::new(80),
                    produced: SequenceCursor::new(140),
                    released: SequenceCursor::new(90),
                    capacity_samples: 2048,
                    overwritten_samples: 80,
                },
            }],
        );
        let temp = TempDirectory::new();
        let capture = CaptureInstallationProfile::new(
            &temp.0,
            CaptureWindowPolicy::new(600_000_000).unwrap(),
            8,
            GroupCommitPolicy::new(4, Duration::from_millis(2)).unwrap(),
            "capture",
        )
        .unwrap();

        let recording = compose_channel_recording(
            bootstrapped,
            &startup[0],
            &capture,
            ChannelCompositionPolicy {
                activation: RecordingActivationPolicy {
                    node_deadline: Duration::from_secs(1),
                },
                takeover: ControlledChannelShutdownPolicy {
                    barrier_deadline: Duration::from_secs(1),
                    node: RecordingShutdownPolicy {
                        node_deadline: Duration::from_secs(1),
                    },
                    drain_deadline: Duration::from_secs(1),
                },
                assembly: RecordingAssemblyPolicy {
                    scheduler: ChannelSchedulerPolicy {
                        overflow_guard: duration(100_000),
                        max_slot: duration(50_000),
                    },
                    first_time_sync_delay: duration(1_000_000),
                    time_sync_phase_step: duration(100_000),
                },
            },
            100,
            RejectingTimeObservationSubmitter,
            Box::new(crate::telemetry_persistence::RejectingTelemetryObservationSubmitter),
        )
        .unwrap();

        let now = clock.now();
        assert_eq!(
            recording
                .measurements()
                .runtime()
                .snapshot(1, now)
                .unwrap()
                .received,
            SequenceCursor::new(141)
        );
        assert_eq!(stops.load(Ordering::SeqCst), 1);
        let origin = capture
            .inspect_stream_origin("line-a", HARDWARE, EPOCH)
            .unwrap()
            .unwrap();
        assert_eq!(origin.first_cursor, SequenceCursor::new(140));
        let report = controlled_shutdown_channel(
            recording,
            &[DiscoveryTargetId {
                channel_id: 1,
                node_address: 1,
            }],
            ControlledChannelShutdownPolicy {
                barrier_deadline: Duration::from_secs(1),
                node: RecordingShutdownPolicy {
                    node_deadline: Duration::from_secs(1),
                },
                drain_deadline: Duration::from_secs(1),
            },
        );
        assert!(report.outcome.is_ok());
        assert_eq!(stops.load(Ordering::SeqCst), 2);
        let shutdown = report.persistence.unwrap();
        assert_eq!(
            shutdown.sink.durable_cursors().next().unwrap().2,
            SequenceCursor::new(141)
        );
        let stream_directory = capture.stream_directory("line-a", HARDWARE, EPOCH).unwrap();
        let paths = capture_segment_paths(stream_directory, 0);
        let validation = validate_capture_segment(paths.wal, paths.manifest).unwrap();
        assert_eq!(validation.manifest.end_exclusive, SequenceCursor::new(141));
        assert_eq!(
            validation.manifest.seal_reason,
            WindowSealReason::ExplicitFinish
        );
    }
}
