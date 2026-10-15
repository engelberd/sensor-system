use core::fmt;
use std::io::{Read, Write};

use crate::capture_profile::CaptureInstallationProfile;
use crate::channel_activation::RecordingActivationPolicy;
use crate::channel_bootstrap::BootstrappedChannel;
use crate::channel_metrics::{ChannelMetricsReader, ChannelMetricsSnapshot};
use crate::channel_startup::{
    build_channel_startup_plans, ChannelStartupError, ChannelStartupPlan, ChannelStartupPolicy,
    RecordingAssemblyPolicy,
};
use crate::channel_supervisor::{
    ChannelJoinResult, ChannelSupervisorError, ChannelSupervisorSet, ChannelTaskSpec,
    SupervisedChannel, SupervisedChannelTask,
};
use crate::channel_worker::{
    compose_channel_recording, ChannelMaintenanceError, ChannelMaintenancePolicy,
    ChannelRolloverServices, ChannelWorkerError, ControlledChannelShutdownError,
    ControlledChannelShutdownPolicy, ProductionChannelTask,
};
use crate::discovery::DiscoveryInventory;
use crate::host_health::{
    evaluate_host_health, HostCoordinatorLifecycle, HostHealthSnapshot, ObservedChannelHealth,
    SharedStorageFault,
};
use crate::installation_bootstrap::{BootstrappedInstallation, BootstrappedInstallationChannel};
use crate::node_metrics::{NodeMetricsReader, NodeMetricsSnapshot};
use crate::operations::{
    ChannelOperationsController, ChannelOperationsHandle, ChannelOperationsPolicy,
    ChannelOperationsSnapshot, OperationsError,
};
use crate::storage_health::{
    StorageHealthObservation, StorageHealthProbe, StorageHealthProbeError,
};
use crate::time_health::TimePersistenceHealth;
use crate::time_persistence::{
    TimeObservationQueueSender, TimePersistenceExecutorError, TimePersistenceRecoveryDisposition,
    TimePersistenceShutdown,
};
use crate::time_profile::{TimePersistenceInstallationProfile, TimeProfileError};
use crate::time_rollover::{time_rollover_mailbox, TimeRolloverMailboxError, TimeRolloverServer};
use crate::time_runtime::{
    TimeRuntime, TimeRuntimeConfig, TimeRuntimeError, TimeRuntimeHealthError,
    TimeRuntimeRecoveryError,
};
use crate::time_wal::HostClockId;

/// One validated channel ready for recovery and activation. Pairing the live
/// endpoint with its admission plan here removes all later index-based joins.
pub struct PlannedHostChannel<P, C> {
    channel_id: u8,
    channel_name: String,
    startup: ChannelStartupPlan,
    bootstrapped: BootstrappedChannel<P, C>,
}

impl<P, C> PlannedHostChannel<P, C> {
    pub const fn channel_id(&self) -> u8 {
        self.channel_id
    }

    pub fn channel_name(&self) -> &str {
        &self.channel_name
    }

    pub const fn startup(&self) -> &ChannelStartupPlan {
        &self.startup
    }

    pub fn bootstrapped(&self) -> &BootstrappedChannel<P, C> {
        &self.bootstrapped
    }

    pub fn into_parts(self) -> (ChannelStartupPlan, BootstrappedChannel<P, C>) {
        (self.startup, self.bootstrapped)
    }
}

/// Side-effect-free final startup blueprint for the host runtime.
///
/// All channels have passed identity/capability checks and per-channel
/// admission, and the single global Time WAL topology is known. No Capture WAL
/// has been opened and no node has been started yet.
pub struct HostStartupBlueprint<P, C> {
    inventory: DiscoveryInventory,
    channels: Vec<PlannedHostChannel<P, C>>,
    time: TimeRuntimeConfig,
}

impl<P, C> HostStartupBlueprint<P, C> {
    pub const fn inventory(&self) -> &DiscoveryInventory {
        &self.inventory
    }

    pub fn channels(&self) -> &[PlannedHostChannel<P, C>] {
        &self.channels
    }

    pub const fn time(&self) -> &TimeRuntimeConfig {
        &self.time
    }

    pub fn into_parts(
        self,
    ) -> (
        DiscoveryInventory,
        Vec<PlannedHostChannel<P, C>>,
        TimeRuntimeConfig,
    ) {
        (self.inventory, self.channels, self.time)
    }
}

pub fn plan_host_startup<P, C>(
    bootstrapped: BootstrappedInstallation<P, C>,
    startup_policy: ChannelStartupPolicy,
    time_profile: &TimePersistenceInstallationProfile,
    host_clock_id: HostClockId,
) -> Result<HostStartupBlueprint<P, C>, HostCoordinatorError> {
    let (inventory, channels) = bootstrapped.into_parts();
    let startup_plans = build_channel_startup_plans(&inventory, startup_policy)
        .map_err(HostCoordinatorError::ChannelPlanning)?;
    let channels = pair_channels(channels, startup_plans)?;
    let time = time_profile
        .runtime_config(host_clock_id, inventory.time_streams())
        .map_err(HostCoordinatorError::TimeProfile)?;

    Ok(HostStartupBlueprint {
        inventory,
        channels,
        time,
    })
}

pub fn plan_linux_host_startup<P, C>(
    bootstrapped: BootstrappedInstallation<P, C>,
    startup_policy: ChannelStartupPolicy,
    time_profile: &TimePersistenceInstallationProfile,
) -> Result<HostStartupBlueprint<P, C>, HostCoordinatorError> {
    let (inventory, channels) = bootstrapped.into_parts();
    let startup_plans = build_channel_startup_plans(&inventory, startup_policy)
        .map_err(HostCoordinatorError::ChannelPlanning)?;
    let channels = pair_channels(channels, startup_plans)?;
    let time = time_profile
        .linux_runtime_config(inventory.time_streams())
        .map_err(HostCoordinatorError::TimeProfile)?;

    Ok(HostStartupBlueprint {
        inventory,
        channels,
        time,
    })
}

pub type ProductionHostChannelTask<P, C> = ProductionChannelTask<P, C, TimeObservationQueueSender>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostChannelRuntimePolicies {
    pub activation: RecordingActivationPolicy,
    pub assembly: RecordingAssemblyPolicy,
    pub shutdown: ControlledChannelShutdownPolicy,
    pub operations: ChannelOperationsPolicy,
    pub maintenance: ChannelMaintenancePolicy,
}

/// Owns the complete running host after startup has crossed the side-effect
/// boundary. The coordinator never handles samples; it supervises independent
/// channel workers and the one shared Time persistence service.
pub struct HostCoordinatorRuntime<T>
where
    T: SupervisedChannelTask,
{
    inventory: DiscoveryInventory,
    channels: ChannelSupervisorSet<T>,
    channel_metrics: Vec<ChannelMetricsReader>,
    node_metrics: Vec<NodeMetricsReader>,
    channel_operations: Vec<ChannelOperationsHandle>,
    startup_channel_faults: Vec<StartupChannelFault>,
    time: TimeRuntime,
    time_rollovers: TimeRolloverServer,
    lifecycle: HostCoordinatorLifecycle,
    shared_storage_fault: Option<SharedStorageFault>,
}

impl<T> HostCoordinatorRuntime<T>
where
    T: SupervisedChannelTask,
{
    pub const fn inventory(&self) -> &DiscoveryInventory {
        &self.inventory
    }

    pub fn channels(&self) -> &[SupervisedChannel<T>] {
        self.channels.channels()
    }

    pub fn channel_metrics(&self) -> Vec<ChannelMetricsSnapshot> {
        self.channel_metrics
            .iter()
            .map(ChannelMetricsReader::snapshot)
            .collect()
    }

    pub fn node_metrics(&self) -> Vec<NodeMetricsSnapshot> {
        self.node_metrics
            .iter()
            .map(NodeMetricsReader::snapshot)
            .collect()
    }

    pub fn channel_operations(&self) -> &[ChannelOperationsHandle] {
        &self.channel_operations
    }

    pub fn channel_operation_snapshots(
        &self,
    ) -> Result<Vec<ChannelOperationsSnapshot>, OperationsError> {
        self.channel_operations
            .iter()
            .map(ChannelOperationsHandle::snapshot)
            .collect()
    }

    pub fn startup_channel_faults(&self) -> &[StartupChannelFault] {
        &self.startup_channel_faults
    }

    pub const fn time_health(&self) -> &TimePersistenceHealth {
        self.time.health()
    }

    pub const fn lifecycle(&self) -> HostCoordinatorLifecycle {
        self.lifecycle
    }

    pub fn health_snapshot(&self) -> HostHealthSnapshot {
        let mut channels = self
            .channels
            .channels()
            .iter()
            .map(|channel| ObservedChannelHealth {
                snapshot: channel.snapshot(),
                thread_finished: channel.is_finished(),
            })
            .collect::<Vec<_>>();
        channels.extend(
            self.startup_channel_faults
                .iter()
                .map(|fault| ObservedChannelHealth {
                    snapshot: crate::channel_supervisor::ChannelWorkerSnapshot {
                        channel_id: fault.channel_id,
                        phase: crate::channel_supervisor::ChannelWorkerPhase::Faulted,
                        completed_steps: 0,
                    },
                    thread_finished: true,
                }),
        );
        evaluate_host_health(
            self.lifecycle,
            self.time.health().state(),
            self.shared_storage_fault,
            channels,
        )
    }

    /// Processes at most one Time persistence completion and therefore never
    /// turns coordinator health polling into an unbounded storage operation.
    pub fn poll_time_health(&mut self) -> Result<bool, TimeRuntimeHealthError> {
        self.time.poll_health()
    }

    /// Performs one bounded supervision decision. A failed Time append is
    /// recovered exactly once after its completion is observed. Failure to
    /// inspect or recover the shared Time service requests a global stop;
    /// faults isolated to one channel remain visible as degraded health.
    pub fn supervise_once(&mut self) -> Result<HostSupervisionReport, HostSupervisionError> {
        if self.lifecycle == HostCoordinatorLifecycle::StopRequested {
            return Ok(HostSupervisionReport {
                action: HostSupervisionAction::StopAlreadyRequested,
                health: self.health_snapshot(),
            });
        }

        if let Err(source) = self.time.poll_health() {
            let stop_request_error = self.request_stop().err();
            return Err(HostSupervisionError::TimeHealth {
                source,
                stop_request_error,
            });
        }

        let mut action = match self.time.health().state() {
            crate::time_health::TimePersistenceHealthState::RecoveryRequired => {
                match self.time.recover() {
                    Ok(disposition) => HostSupervisionAction::TimeRecoveryStarted(disposition),
                    Err(source) => {
                        let stop_request_error = self.request_stop().err();
                        return Err(HostSupervisionError::TimeRecovery {
                            source,
                            stop_request_error,
                        });
                    }
                }
            }
            crate::time_health::TimePersistenceHealthState::IntegrityFault => {
                let stop_request_error = self.request_stop().err();
                return Err(HostSupervisionError::TimeIntegrityFault { stop_request_error });
            }
            crate::time_health::TimePersistenceHealthState::Healthy
            | crate::time_health::TimePersistenceHealthState::Recovering => {
                HostSupervisionAction::Continue
            }
        };
        if action == HostSupervisionAction::Continue {
            if let Ok(Some(request)) = self.time_rollovers.try_receive() {
                let (previous, next, completion) = request.into_parts();
                let result = self.time.rollover_stream(previous, next);
                let succeeded = result.is_ok();
                let _ = completion.complete(result);
                action = HostSupervisionAction::TimeRolloverCompleted { succeeded };
            }
        }
        Ok(HostSupervisionReport {
            action,
            health: self.health_snapshot(),
        })
    }

    /// Adds the platform storage observation to one supervision turn. The
    /// reserve is checked before Time recovery so no new recovery write is
    /// attempted after the shared filesystem becomes unsafe.
    pub fn supervise_once_with_storage(
        &mut self,
        storage: &StorageHealthProbe,
    ) -> Result<HostSupervisionReport, HostSupervisionError> {
        if self.lifecycle == HostCoordinatorLifecycle::StopRequested {
            return self.supervise_once();
        }
        match storage.observe() {
            Ok(StorageHealthObservation::Healthy(_)) => self.supervise_once(),
            Ok(StorageHealthObservation::Fault(fault)) => {
                let stop_request_error = self.report_shared_storage_fault(fault).err();
                Err(HostSupervisionError::SharedStorage {
                    fault,
                    stop_request_error,
                })
            }
            Err(source) => {
                let stop_request_error = self
                    .report_shared_storage_fault(SharedStorageFault::ProbeFailed)
                    .err();
                Err(HostSupervisionError::StorageProbe {
                    source,
                    stop_request_error,
                })
            }
        }
    }

    /// Non-blocking and idempotent. Workers observe the bounded mailbox before
    /// their next channel step.
    pub fn request_stop(&mut self) -> Result<(), ChannelSupervisorError> {
        self.lifecycle = HostCoordinatorLifecycle::StopRequested;
        self.channels.request_stop_all()
    }

    /// Records the first fault of a resource shared by all Capture writers and
    /// requests a global controlled stop. Repeated reports are idempotent and
    /// cannot overwrite the original cause.
    pub fn report_shared_storage_fault(
        &mut self,
        fault: SharedStorageFault,
    ) -> Result<(), ChannelSupervisorError> {
        if self.shared_storage_fault.is_none() {
            self.shared_storage_fault = Some(fault);
        }
        self.request_stop()
    }

    /// Stops and joins every channel before stopping Time persistence. This
    /// ordering guarantees that no channel can retain or use a Time submitter
    /// after the shared service has been closed.
    pub fn shutdown(self) -> HostCoordinatorShutdown<T::StepError, T::Shutdown> {
        let stop_request = self.channels.request_stop_all();
        let channels = self.channels.join_all();
        let time = self.time.shutdown();
        HostCoordinatorShutdown {
            stop_request,
            channels,
            time,
        }
    }
}

pub struct HostCoordinatorShutdown<E, S> {
    pub stop_request: Result<(), ChannelSupervisorError>,
    pub channels: Vec<ChannelJoinResult<E, S>>,
    pub time: Result<TimePersistenceShutdown, TimePersistenceExecutorError>,
}

/// Crosses the host startup side-effect boundary exactly once: opens global
/// Time storage, composes each Capture/channel task, and then starts the ready
/// supervisor threads. A node lifecycle activation fault is isolated to its
/// physical channel. Configuration, Capture/recovery, scheduler and shared
/// Time failures remain fail-closed for the whole process.
pub fn start_host_runtime<P, C>(
    blueprint: HostStartupBlueprint<P, C>,
    capture: &CaptureInstallationProfile,
    time_profile: &TimePersistenceInstallationProfile,
    policies: HostChannelRuntimePolicies,
    work_id_seed: u64,
) -> Result<HostCoordinatorRuntime<ProductionHostChannelTask<P, C>>, HostRuntimeStartError>
where
    P: Read + Write + Send + 'static,
    C: crate::channel_endpoint::MonotonicClock + Send + 'static,
{
    if work_id_seed == 0 {
        return Err(HostRuntimeStartError::ZeroWorkIdSeed);
    }
    policies
        .shutdown
        .validate()
        .map_err(HostRuntimeStartError::ShutdownPolicy)?;
    policies
        .operations
        .validate()
        .map_err(HostRuntimeStartError::OperationsPolicy)?;
    policies
        .maintenance
        .validate()
        .map_err(HostRuntimeStartError::MaintenancePolicy)?;

    let (inventory, channels, time_config) = blueprint.into_parts();
    let time = time_config.start().map_err(HostRuntimeStartError::Time)?;
    let host_clock_id = time.host_clock_id();
    let (time_rollover, time_rollovers) = match time_rollover_mailbox(inventory.nodes().len()) {
        Ok(mailbox) => mailbox,
        Err(source) => {
            let time_shutdown_error = time.shutdown().err();
            return Err(HostRuntimeStartError::TimeRolloverMailbox {
                source,
                time_shutdown_error,
            });
        }
    };
    let mut prepared = Vec::with_capacity(channels.len());
    let mut startup_channel_faults = Vec::new();

    for planned in channels {
        let channel_id = planned.channel_id();
        let channel_name = planned.channel_name().to_owned();
        let time_observations = match time.submitter() {
            Ok(value) => value,
            Err(source) => {
                let cleanup = cleanup_prepared_channels(prepared, time);
                return Err(HostRuntimeStartError::TimeSubmitter {
                    channel_id,
                    source,
                    cleanup,
                });
            }
        };
        let (startup, bootstrapped) = planned.into_parts();
        let channel_seed = match work_id_seed.checked_add(u64::from(channel_id.saturating_sub(1))) {
            Some(value) => value,
            None => {
                drop(time_observations);
                let cleanup = cleanup_prepared_channels(prepared, time);
                return Err(HostRuntimeStartError::WorkIdSeedOverflow {
                    channel_id,
                    cleanup,
                });
            }
        };
        let recording = match compose_channel_recording(
            bootstrapped,
            &startup,
            capture,
            policies.activation,
            policies.assembly,
            channel_seed,
            time_observations,
        ) {
            Ok(value) => value,
            Err(source @ ChannelWorkerError::Activation { .. }) => {
                startup_channel_faults.push(StartupChannelFault {
                    channel_id,
                    channel_name,
                    detail: source.to_string(),
                });
                continue;
            }
            Err(source) => {
                let cleanup = cleanup_prepared_channels(prepared, time);
                return Err(HostRuntimeStartError::Channel {
                    channel_id,
                    source,
                    cleanup,
                });
            }
        };
        let (operations, operations_handle) =
            ChannelOperationsController::new(channel_id, policies.operations)
                .expect("operations policy was validated before channel composition");
        let task = ProductionChannelTask::new(
            recording,
            &startup,
            policies.shutdown,
            operations,
            policies.maintenance,
            ChannelRolloverServices::new(
                channel_name.clone(),
                capture.clone(),
                time_profile.clone(),
                host_clock_id,
                time_rollover.clone(),
            ),
        )
        .expect("channel task policies were validated before channel composition");
        prepared.push((channel_id, channel_name, task, operations_handle));
    }

    if prepared.is_empty() {
        let time_shutdown_error = time.shutdown().err();
        return Err(HostRuntimeStartError::NoRunnableChannels {
            faults: startup_channel_faults,
            time_shutdown_error,
        });
    }

    let mut channel_metrics = Vec::with_capacity(prepared.len());
    let mut node_metrics = Vec::with_capacity(inventory.nodes().len());
    let mut channel_operations = Vec::with_capacity(prepared.len());
    let specs = prepared
        .into_iter()
        .map(|(channel_id, channel_name, task, operations)| {
            channel_metrics.push(task.metrics_reader());
            node_metrics.extend(task.node_metrics_readers());
            channel_operations.push(operations);
            ChannelTaskSpec::new(channel_id, channel_name, task)
        })
        .collect();
    let channels = match ChannelSupervisorSet::start(specs) {
        Ok(value) => value,
        Err(source) => {
            // ChannelSupervisorSet owns cleanup of every supplied task on a
            // partial spawn. No Time submitters remain after it returns.
            let time_shutdown_error = time.shutdown().err();
            return Err(HostRuntimeStartError::Supervisor {
                source,
                time_shutdown_error,
            });
        }
    };

    Ok(HostCoordinatorRuntime {
        inventory,
        channels,
        channel_metrics,
        node_metrics,
        channel_operations,
        startup_channel_faults,
        time,
        time_rollovers,
        lifecycle: HostCoordinatorLifecycle::Running,
        shared_storage_fault: None,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartupChannelFault {
    pub channel_id: u8,
    pub channel_name: String,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostSupervisionAction {
    Continue,
    TimeRecoveryStarted(TimePersistenceRecoveryDisposition),
    TimeRolloverCompleted { succeeded: bool },
    StopAlreadyRequested,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostSupervisionReport {
    pub action: HostSupervisionAction,
    pub health: HostHealthSnapshot,
}

#[derive(Debug)]
pub enum HostSupervisionError {
    TimeHealth {
        source: TimeRuntimeHealthError,
        stop_request_error: Option<ChannelSupervisorError>,
    },
    TimeRecovery {
        source: TimeRuntimeRecoveryError,
        stop_request_error: Option<ChannelSupervisorError>,
    },
    TimeIntegrityFault {
        stop_request_error: Option<ChannelSupervisorError>,
    },
    SharedStorage {
        fault: SharedStorageFault,
        stop_request_error: Option<ChannelSupervisorError>,
    },
    StorageProbe {
        source: StorageHealthProbeError,
        stop_request_error: Option<ChannelSupervisorError>,
    },
}

impl fmt::Display for HostSupervisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "host supervision error: {self:?}")
    }
}

impl std::error::Error for HostSupervisionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TimeHealth { source, .. } => Some(source),
            Self::TimeRecovery { source, .. } => Some(source),
            Self::StorageProbe { source, .. } => Some(source),
            Self::TimeIntegrityFault { .. } | Self::SharedStorage { .. } => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct HostStartupCleanup {
    pub channel_shutdown_failures: usize,
    pub capture_shutdown_failures: usize,
    pub time_shutdown_error: Option<TimePersistenceExecutorError>,
}

fn cleanup_prepared_channels<P, C>(
    prepared: Vec<(
        u8,
        String,
        ProductionHostChannelTask<P, C>,
        ChannelOperationsHandle,
    )>,
    time: TimeRuntime,
) -> HostStartupCleanup
where
    P: Read + Write + Send + 'static,
    C: crate::channel_endpoint::MonotonicClock + Send + 'static,
{
    let mut cleanup = HostStartupCleanup::default();
    for (_, _, task, _operations) in prepared {
        let report = task.shutdown();
        cleanup.channel_shutdown_failures += usize::from(report.outcome.is_err());
        cleanup.capture_shutdown_failures += usize::from(report.persistence.is_err());
    }
    cleanup.time_shutdown_error = time.shutdown().err();
    cleanup
}

#[derive(Debug)]
pub enum HostRuntimeStartError {
    ZeroWorkIdSeed,
    ShutdownPolicy(ControlledChannelShutdownError),
    OperationsPolicy(OperationsError),
    MaintenancePolicy(ChannelMaintenanceError),
    Time(TimeRuntimeError),
    TimeRolloverMailbox {
        source: TimeRolloverMailboxError,
        time_shutdown_error: Option<TimePersistenceExecutorError>,
    },
    TimeSubmitter {
        channel_id: u8,
        source: TimePersistenceExecutorError,
        cleanup: HostStartupCleanup,
    },
    WorkIdSeedOverflow {
        channel_id: u8,
        cleanup: HostStartupCleanup,
    },
    Channel {
        channel_id: u8,
        source: ChannelWorkerError,
        cleanup: HostStartupCleanup,
    },
    NoRunnableChannels {
        faults: Vec<StartupChannelFault>,
        time_shutdown_error: Option<TimePersistenceExecutorError>,
    },
    Supervisor {
        source: ChannelSupervisorError,
        time_shutdown_error: Option<TimePersistenceExecutorError>,
    },
}

impl fmt::Display for HostRuntimeStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "host runtime startup error: {self:?}")
    }
}

impl std::error::Error for HostRuntimeStartError {}

fn pair_channels<P, C>(
    channels: Vec<BootstrappedInstallationChannel<P, C>>,
    mut plans: Vec<ChannelStartupPlan>,
) -> Result<Vec<PlannedHostChannel<P, C>>, HostCoordinatorError> {
    if channels.len() != plans.len() {
        return Err(HostCoordinatorError::ChannelCountMismatch {
            bootstrapped: channels.len(),
            planned: plans.len(),
        });
    }

    let mut paired = Vec::with_capacity(channels.len());
    for channel in channels {
        let position = plans
            .iter()
            .position(|plan| plan.channel_id() == channel.channel_id())
            .ok_or(HostCoordinatorError::MissingChannelPlan {
                channel_id: channel.channel_id(),
            })?;
        let startup = plans.swap_remove(position);
        if startup.channel_name() != channel.channel_name() {
            return Err(HostCoordinatorError::ChannelNameMismatch {
                channel_id: channel.channel_id(),
            });
        }
        paired.push(PlannedHostChannel {
            channel_id: channel.channel_id(),
            channel_name: channel.channel_name().to_owned(),
            startup,
            bootstrapped: channel.into_channel(),
        });
    }
    if let Some(plan) = plans.first() {
        return Err(HostCoordinatorError::UnexpectedChannelPlan {
            channel_id: plan.channel_id(),
        });
    }
    Ok(paired)
}

#[derive(Debug)]
pub enum HostCoordinatorError {
    ChannelPlanning(ChannelStartupError),
    TimeProfile(TimeProfileError),
    ChannelCountMismatch { bootstrapped: usize, planned: usize },
    MissingChannelPlan { channel_id: u8 },
    UnexpectedChannelPlan { channel_id: u8 },
    ChannelNameMismatch { channel_id: u8 },
}

impl fmt::Display for HostCoordinatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "host startup planning error: {self:?}")
    }
}

impl std::error::Error for HostCoordinatorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ChannelPlanning(source) => Some(source),
            Self::TimeProfile(source) => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use sensor_core::{BootEpoch, HardwareId, SequenceCursor};
    use sensor_protocol::{
        FirmwareVersion, NodeCapabilities, NodeIdentity, ObservabilityProfile, OutputRateCode,
        RuntimeResult, RuntimeResultCode, RuntimeState, SensorStatusCode, ServiceId, StreamState,
        CAPABILITIES_SCHEMA_VERSION, LOCAL_PROTOCOL_VERSION, MAX_PAYLOAD_BYTES,
    };
    use serde_json::json;

    use crate::capture_window::CaptureWindowPolicy;
    use crate::channel_activation::RecordingShutdownPolicy;
    use crate::channel_admission::AdmissionPolicy;
    use crate::channel_agent::{ChannelAgent, ChannelPolicy};
    use crate::channel_bootstrap::NodeBootstrapEvidence;
    use crate::channel_endpoint::ChannelEndpoint;
    use crate::channel_port::ChannelPort;
    use crate::channel_runtime::TimeSyncScheduleConfig;
    use crate::channel_scheduler::ChannelSchedulerPolicy;
    use crate::discovery::{
        DiscoveryTargetId, InstallationDiscoveryPlan, REQUIRED_RECORDING_FEATURES,
    };
    use crate::installation_topology::InstallationTopologyProfile;
    use crate::operations::{
        OperationId, OperationKind, OperationPhase, OperationRequest, OperationResult,
        OperationTarget,
    };
    use crate::persistence_executor::GroupCommitPolicy;
    use crate::simulation::{SimulatedClock, SimulatedRs485Bus, SimulatedWireNode};
    use crate::storage_health::StorageHealthPolicy;
    use crate::transaction::DurationMicros;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sensor-host-coordinator-{}-{id}",
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

    fn duration(value: u64) -> DurationMicros {
        DurationMicros::new(value).unwrap()
    }

    fn identity(channel_id: u8) -> NodeIdentity {
        NodeIdentity {
            hardware_id: HardwareId::from_bytes([channel_id; 8]),
            boot_epoch: BootEpoch::new(u64::from(channel_id)),
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
                | ServiceId::Lifecycle.capability_bit()
                | ServiceId::Configuration.capability_bit()
                | ServiceId::Time.capability_bit(),
            feature_flags: REQUIRED_RECORDING_FEATURES,
            max_payload_bytes: MAX_PAYLOAD_BYTES as u16,
            journal_capacity_samples: 2048,
            output_rate_mask: OutputRateCode::Hz62_5.capability_bit()
                | OutputRateCode::Hz125.capability_bit()
                | OutputRateCode::Hz250.capability_bit(),
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

    fn topology(channel_count: u8) -> InstallationTopologyProfile {
        let channels = (1_u8..=channel_count)
            .map(|channel_id| {
                json!({
                    "name": format!("line-{channel_id:02}"),
                    "channel_id": channel_id,
                    "enabled": true,
                    "transport": {
                        "kind": "serial-rs485",
                        "device_id": format!("adapter-{channel_id}"),
                        "endpoint": format!("/dev/sim-{channel_id}"),
                        "baud_rate": 115200
                    },
                    "nodes": [{
                        "id": 1,
                        "hardware_id": format!("{channel_id:02x}").repeat(8),
                        "board_revision": 2,
                        "enabled": true,
                        "output_rate_hz": 125
                    }]
                })
            })
            .collect::<Vec<_>>();
        InstallationTopologyProfile::from_document(&json!({
            "system": {"name": "channel-matrix-simulation"},
            "channels": channels
        }))
        .unwrap()
    }

    fn startup_policy() -> ChannelStartupPolicy {
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
        }
    }

    fn run_channel_matrix_case(channel_count: u8) {
        let temp = TempDirectory::new();
        let discovery = InstallationDiscoveryPlan::from_topology(&topology(channel_count));
        let mut collector = discovery.collector();
        for channel_id in 1..=channel_count {
            collector
                .accept_runtime(
                    DiscoveryTargetId {
                        channel_id,
                        node_address: 1,
                    },
                    identity(channel_id),
                    capabilities(),
                    observed_configuration(),
                )
                .unwrap();
        }
        let inventory = collector.finish().unwrap();
        let startup = build_channel_startup_plans(&inventory, startup_policy()).unwrap();
        let time_profile =
            TimePersistenceInstallationProfile::new(temp.0.join("time"), 32, "time-storage")
                .unwrap();
        let time = time_profile
            .runtime_config(HostClockId::from_bytes([9; 16]), inventory.time_streams())
            .unwrap();

        let mut channels = Vec::new();
        for startup in startup {
            let channel_id = startup.channel_id();
            let clock = SimulatedClock::new(0);
            let mut simulated_node = SimulatedWireNode::new(
                1,
                BootEpoch::new(u64::from(channel_id)),
                2048,
                OutputRateCode::Hz125.sample_rate(),
            )
            .with_identity_contract(identity(channel_id), capabilities());
            if channel_count == 1 {
                simulated_node = simulated_node.with_reboot_on_maintenance(BootEpoch::new(101));
            }
            let mut bus = SimulatedRs485Bus::new(clock.clone(), vec![simulated_node]);
            bus.set_auto_advance_on_read(true);
            let agent = ChannelAgent::new(
                u32::from(channel_id),
                100 + u32::from(channel_id),
                duration(50),
                ChannelPolicy {
                    transaction_timeout: duration(100_000),
                    retry_delay: duration(1_000),
                    measurement_attempts_per_slot: 2,
                    max_pending_work_items: 2,
                },
            )
            .unwrap();
            let endpoint = ChannelEndpoint::new(agent, ChannelPort::from_io(bus), clock);
            let bootstrapped = BootstrappedChannel::from_parts(
                endpoint,
                vec![NodeBootstrapEvidence {
                    id: DiscoveryTargetId {
                        channel_id,
                        node_address: 1,
                    },
                    identity: identity(channel_id),
                    capabilities: capabilities(),
                    configuration: observed_configuration(),
                    runtime: RuntimeResult {
                        operation_id: 1,
                        code: RuntimeResultCode::Ok,
                        state: RuntimeState::Idle,
                        observability: ObservabilityProfile::Production,
                        sensor_status: SensorStatusCode::Ok,
                    },
                    stream: StreamState {
                        boot_epoch: BootEpoch::new(u64::from(channel_id)),
                        retained_from: SequenceCursor::new(0),
                        produced: SequenceCursor::new(0),
                        released: SequenceCursor::new(0),
                        capacity_samples: 2048,
                        overwritten_samples: 0,
                    },
                }],
            );
            channels.push(PlannedHostChannel {
                channel_id,
                channel_name: startup.channel_name().to_owned(),
                startup,
                bootstrapped,
            });
        }
        let blueprint = HostStartupBlueprint {
            inventory,
            channels,
            time,
        };
        let capture = CaptureInstallationProfile::new(
            temp.0.join("capture"),
            CaptureWindowPolicy::new(600_000_000).unwrap(),
            32,
            GroupCommitPolicy::new(8, Duration::from_millis(2)).unwrap(),
            "capture",
        )
        .unwrap();
        let mut runtime = start_host_runtime(
            blueprint,
            &capture,
            &time_profile,
            HostChannelRuntimePolicies {
                activation: RecordingActivationPolicy {
                    node_deadline: Duration::from_secs(1),
                },
                assembly: RecordingAssemblyPolicy {
                    scheduler: ChannelSchedulerPolicy {
                        overflow_guard: duration(100_000),
                        max_slot: duration(50_000),
                    },
                    first_time_sync_delay: duration(1_000_000),
                    time_sync_phase_step: duration(100_000),
                },
                shutdown: ControlledChannelShutdownPolicy {
                    barrier_deadline: Duration::from_secs(1),
                    node: RecordingShutdownPolicy {
                        node_deadline: Duration::from_secs(1),
                    },
                    drain_deadline: Duration::from_secs(1),
                },
                operations: ChannelOperationsPolicy {
                    mailbox_capacity: 4,
                    ledger_capacity: 16,
                },
                maintenance: ChannelMaintenancePolicy {
                    barrier_deadline: Duration::from_secs(1),
                    drain_deadline: Duration::from_secs(1),
                    service_operation_deadline: Duration::from_secs(1),
                    safe_state: crate::safe_state::SafeStatePolicy {
                        orderly_attempts: 2,
                        restart_attempts: 2,
                        restart_cycles: 1,
                    },
                    wire: crate::safe_state::SafeStateWirePolicy {
                        action_deadline: duration(100_000),
                        restart_settle_delay: duration(10_000),
                        restart_confirmation_deadline: duration(500_000),
                    },
                    resume_verification: crate::channel_resume::ResumeVerificationPolicy {
                        node_deadline: duration(500_000),
                    },
                },
            },
            1_000,
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        while runtime
            .channels()
            .iter()
            .any(|channel| channel.snapshot().completed_steps < 4)
            && Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert_eq!(runtime.channels().len(), usize::from(channel_count));
        assert_eq!(runtime.channel_metrics().len(), usize::from(channel_count));
        assert_eq!(
            runtime.channel_operations().len(),
            usize::from(channel_count)
        );
        assert!(runtime
            .channels()
            .iter()
            .all(|channel| channel.snapshot().completed_steps >= 4));
        let first_operations = runtime.channel_operations()[0].clone();
        first_operations
            .try_submit(OperationRequest {
                operation_id: OperationId::new(42).unwrap(),
                target: OperationTarget::Channel { channel_id: 1 },
                kind: OperationKind::ReadStatus,
            })
            .unwrap();
        let operation_deadline = Instant::now() + Duration::from_secs(1);
        while first_operations.snapshot().unwrap().records.is_empty()
            && Instant::now() < operation_deadline
        {
            std::thread::yield_now();
        }
        let operation_snapshot = first_operations.snapshot().unwrap();
        assert_eq!(operation_snapshot.records.len(), 1);
        assert_eq!(
            operation_snapshot.records[0].phase,
            OperationPhase::Succeeded
        );
        assert_eq!(operation_snapshot.records[0].error, None);
        if channel_count == 1 {
            first_operations
                .try_submit(OperationRequest {
                    operation_id: OperationId::new(43).unwrap(),
                    target: OperationTarget::Channel { channel_id: 1 },
                    kind: OperationKind::EnterMaintenance,
                })
                .unwrap();
            let maintenance_deadline = Instant::now() + Duration::from_secs(2);
            while {
                let snapshot = first_operations.snapshot().unwrap();
                (snapshot.mode != crate::operations::ChannelOperatingMode::Maintenance
                    || snapshot.records.len() < 2
                    || snapshot.records[1].phase != OperationPhase::Succeeded)
                    && Instant::now() < maintenance_deadline
            } {
                std::thread::yield_now();
            }
            let maintenance = first_operations.snapshot().unwrap();
            assert_eq!(
                maintenance.mode,
                crate::operations::ChannelOperatingMode::Maintenance,
                "operation={maintenance:?} worker={:?}",
                runtime.channels()[0].snapshot()
            );
            assert_eq!(maintenance.records[1].phase, OperationPhase::Succeeded);
            assert_eq!(maintenance.records[1].error, None);

            first_operations
                .try_submit(OperationRequest {
                    operation_id: OperationId::new(44).unwrap(),
                    target: OperationTarget::Node {
                        channel_id: 1,
                        node_address: 1,
                    },
                    kind: OperationKind::GetConfiguration,
                })
                .unwrap();
            let configuration_deadline = Instant::now() + Duration::from_secs(2);
            while {
                let snapshot = first_operations.snapshot().unwrap();
                (snapshot.records.len() < 3
                    || snapshot.records[2].phase != OperationPhase::Succeeded)
                    && Instant::now() < configuration_deadline
            } {
                std::thread::yield_now();
            }
            let observed = first_operations.snapshot().unwrap();
            assert!(matches!(
                observed.records[2].result,
                Some(OperationResult::ConfigurationObserved(value))
                    if value.revision == 1
                        && value.desired.output_rate == OutputRateCode::Hz125
            ));

            first_operations
                .try_submit(OperationRequest {
                    operation_id: OperationId::new(45).unwrap(),
                    target: OperationTarget::Node {
                        channel_id: 1,
                        node_address: 1,
                    },
                    kind: OperationKind::ValidateConfiguration {
                        configuration: sensor_protocol::MeasurementConfiguration {
                            output_rate: OutputRateCode::Hz125,
                            range_g: 2,
                            high_pass_corner: 0,
                            filter_profile: sensor_protocol::DecimationFilterProfile::Balanced,
                            fifo_watermark: 30,
                            offset_x: 0,
                            offset_y: 0,
                            offset_z: 0,
                        },
                    },
                })
                .unwrap();
            let validation_deadline = Instant::now() + Duration::from_secs(2);
            while {
                let snapshot = first_operations.snapshot().unwrap();
                (snapshot.records.len() < 4
                    || snapshot.records[3].phase != OperationPhase::Succeeded)
                    && Instant::now() < validation_deadline
            } {
                std::thread::yield_now();
            }
            let validated = first_operations.snapshot().unwrap();
            assert!(matches!(
                validated.records[3].result,
                Some(OperationResult::ConfigurationValidated(value))
                    if value.code == sensor_protocol::ConfigValidationCode::Valid
                        && value.field == sensor_protocol::ConfigField::None
            ));

            first_operations
                .try_submit(OperationRequest {
                    operation_id: OperationId::new(46).unwrap(),
                    target: OperationTarget::Node {
                        channel_id: 1,
                        node_address: 1,
                    },
                    kind: OperationKind::ApplyConfiguration {
                        expected_revision: 1,
                        configuration: sensor_protocol::MeasurementConfiguration {
                            output_rate: OutputRateCode::Hz62_5,
                            range_g: 2,
                            high_pass_corner: 0,
                            filter_profile: sensor_protocol::DecimationFilterProfile::Balanced,
                            fifo_watermark: 30,
                            offset_x: -4,
                            offset_y: 0,
                            offset_z: 4,
                        },
                    },
                })
                .unwrap();
            let apply_deadline = Instant::now() + Duration::from_secs(2);
            while {
                let snapshot = first_operations.snapshot().unwrap();
                (snapshot.records.len() < 5
                    || snapshot.records[4].phase != OperationPhase::Succeeded)
                    && Instant::now() < apply_deadline
            } {
                std::thread::yield_now();
            }
            let applied = first_operations.snapshot().unwrap();
            assert!(matches!(
                applied.records[4].result,
                Some(OperationResult::ConfigurationApplied(value))
                    if value.code == sensor_protocol::ConfigOperationCode::Applied
                        && value.observed.revision == 2
                        && value.observed.desired.output_rate == OutputRateCode::Hz62_5
                        && value.observed.desired.offset_x == -4
            ));

            first_operations
                .try_submit(OperationRequest {
                    operation_id: OperationId::new(47).unwrap(),
                    target: OperationTarget::Node {
                        channel_id: 1,
                        node_address: 1,
                    },
                    kind: OperationKind::ResetConfiguration {
                        expected_revision: 2,
                    },
                })
                .unwrap();
            let reset_deadline = Instant::now() + Duration::from_secs(2);
            while {
                let snapshot = first_operations.snapshot().unwrap();
                (snapshot.records.len() < 6
                    || snapshot.records[5].phase != OperationPhase::Succeeded)
                    && Instant::now() < reset_deadline
            } {
                std::thread::yield_now();
            }
            let reset = first_operations.snapshot().unwrap();
            assert!(matches!(
                reset.records[5].result,
                Some(OperationResult::ConfigurationApplied(value))
                    if value.code == sensor_protocol::ConfigOperationCode::Applied
                        && value.observed.revision == 3
                        && value.observed.desired.output_rate == OutputRateCode::Hz250
                        && value.observed.desired.offset_x == 0
            ));

            first_operations
                .try_submit(OperationRequest {
                    operation_id: OperationId::new(48).unwrap(),
                    target: OperationTarget::Channel { channel_id: 1 },
                    kind: OperationKind::ResumeRecording,
                })
                .unwrap();
            let resume_deadline = Instant::now() + Duration::from_secs(2);
            while {
                let snapshot = first_operations.snapshot().unwrap();
                (snapshot.mode != crate::operations::ChannelOperatingMode::Recording
                    || snapshot.records.len() < 7
                    || snapshot.records[6].phase != OperationPhase::Succeeded)
                    && Instant::now() < resume_deadline
            } {
                let _ = runtime.supervise_once();
                std::thread::yield_now();
            }
            let resumed = first_operations.snapshot().unwrap();
            assert_eq!(
                resumed.mode,
                crate::operations::ChannelOperatingMode::Recording,
                "operation={resumed:?} worker={:?}",
                runtime.channels()[0].snapshot()
            );
            assert_eq!(resumed.records[6].phase, OperationPhase::Succeeded);
            assert_eq!(resumed.records[6].error, None);
            assert!(runtime
                .time_health()
                .streams()
                .iter()
                .any(|stream| stream.stream.boot_epoch == BootEpoch::new(101)));
        }
        let supervision = runtime.supervise_once().unwrap();
        assert_eq!(supervision.action, HostSupervisionAction::Continue);
        assert_eq!(
            supervision.health.level,
            crate::host_health::HostHealthLevel::Healthy
        );
        let storage = StorageHealthProbe::new(
            &temp.0,
            StorageHealthPolicy {
                minimum_available_bytes: u64::MAX,
            },
        )
        .unwrap();
        assert!(matches!(
            runtime.supervise_once_with_storage(&storage),
            Err(HostSupervisionError::SharedStorage {
                fault: SharedStorageFault::ReserveBreached { .. },
                stop_request_error: None
            })
        ));
        assert_eq!(
            runtime.health_snapshot().level,
            crate::host_health::HostHealthLevel::StopRequired
        );
        runtime.request_stop().unwrap();
        let stopping = runtime.supervise_once().unwrap();
        assert_eq!(stopping.action, HostSupervisionAction::StopAlreadyRequested);
        assert_eq!(
            stopping.health.coordinator,
            HostCoordinatorLifecycle::StopRequested
        );

        let shutdown = runtime.shutdown();
        assert!(shutdown.stop_request.is_ok());
        assert_eq!(shutdown.channels.len(), usize::from(channel_count));
        for channel in &shutdown.channels {
            match &channel.as_ref().unwrap().1 {
                crate::channel_supervisor::ChannelWorkerExit::Stopped { shutdown, .. } => {
                    assert!(shutdown.outcome.is_ok());
                    assert!(shutdown.persistence.is_ok());
                    assert_eq!(
                        shutdown.persistence.as_ref().unwrap().sink.stream_count(),
                        1 + usize::from(channel_count == 1),
                        "the restarted node must retain both Capture epochs"
                    );
                }
                crate::channel_supervisor::ChannelWorkerExit::Faulted { .. } => {
                    panic!("simulated channel faulted")
                }
            }
        }
        assert_eq!(
            shutdown.time.as_ref().unwrap().repository.stream_count(),
            usize::from(channel_count) + usize::from(channel_count == 1),
            "all channels in the matrix case must share one global Time repository"
        );
        assert!(crate::host_process::summarize_shutdown(shutdown).clean());
    }

    #[test]
    fn one_four_and_eight_channels_share_time_and_shutdown_cleanly() {
        for channel_count in [1, 4, 8] {
            run_channel_matrix_case(channel_count);
        }
    }
}
