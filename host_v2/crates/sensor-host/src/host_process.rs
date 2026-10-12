use core::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use crate::channel_endpoint::SystemMonotonicClock;
use crate::channel_metrics::ChannelMetricsSnapshot;
use crate::channel_supervisor::ChannelWorkerExit;
use crate::channel_worker::ControlledChannelShutdownReport;
use crate::discovery::DiscoveryInventory;
use crate::discovery::InstallationDiscoveryPlan;
use crate::host_coordinator::{
    plan_host_startup, start_host_runtime, HostCoordinatorError, HostCoordinatorRuntime,
    HostCoordinatorShutdown, HostRuntimeStartError, HostSupervisionError, HostSupervisionReport,
    ProductionHostChannelTask,
};
use crate::host_health::HostHealthSnapshot;
use crate::installation_bootstrap::{
    bootstrap_serial_installation, InstallationBootstrapError, SerialChannelBootstrapError,
};
use crate::installation_topology::HostInstallationProfile;
use crate::node_metrics::NodeMetricsSnapshot;
use crate::storage_health::{
    StorageHealthObservation, StorageHealthPolicy, StorageHealthProbe, StorageHealthProbeError,
};
use crate::time_health::TimePersistenceHealth;
use crate::time_wal::{HostClockId, HostClockIdError};

pub type LinuxSerialPort = Box<dyn serialport::SerialPort>;
pub type LinuxProductionChannelTask =
    ProductionHostChannelTask<LinuxSerialPort, SystemMonotonicClock>;
pub type LinuxHostProcessShutdown = HostCoordinatorShutdown<
    <LinuxProductionChannelTask as crate::channel_supervisor::SupervisedChannelTask>::StepError,
    <LinuxProductionChannelTask as crate::channel_supervisor::SupervisedChannelTask>::Shutdown,
>;

/// The production process owner after all startup phases have succeeded.
///
/// The binary owns this value exactly once. It cannot accidentally close the
/// shared Time WAL before channel workers because `shutdown` delegates to the
/// coordinator's ordered shutdown protocol.
pub struct LinuxHostProcess {
    runtime: HostCoordinatorRuntime<LinuxProductionChannelTask>,
    storage: StorageHealthProbe,
    supervision_interval: Duration,
    metrics_interval: Duration,
}

impl LinuxHostProcess {
    pub fn start(
        profile: &HostInstallationProfile,
        session_seed: u32,
        work_id_seed: u64,
    ) -> Result<Self, LinuxHostProcessStartError> {
        // This is a Linux process contract, not an incidental Time WAL detail.
        // Reject an unsupported host before touching storage or opening RS485.
        let host_clock_id = HostClockId::read_current_linux_boot_id()
            .map_err(LinuxHostProcessStartError::HostClock)?;
        Self::start_with_host_clock(profile, host_clock_id, session_seed, work_id_seed)
    }

    pub fn start_with_host_clock(
        profile: &HostInstallationProfile,
        host_clock_id: HostClockId,
        session_seed: u32,
        work_id_seed: u64,
    ) -> Result<Self, LinuxHostProcessStartError> {
        let storage = prepare_storage_health(
            profile.storage().capture().root(),
            profile.storage().minimum_free_bytes(),
        )?;
        let plan = InstallationDiscoveryPlan::from_topology(profile.topology());
        let bootstrapped = bootstrap_serial_installation(
            &plan,
            profile.runtime().serial_bootstrap(),
            session_seed,
        )
        .map_err(LinuxHostProcessStartError::Bootstrap)?;
        let startup_policy = profile
            .runtime()
            .channel_startup(profile.storage().time_sync())
            .map_err(LinuxHostProcessStartError::RuntimeProfile)?;
        let blueprint = plan_host_startup(
            bootstrapped,
            startup_policy,
            profile.storage().time_persistence(),
            host_clock_id,
        )
        .map_err(LinuxHostProcessStartError::Planning)?;
        let runtime = start_host_runtime(
            blueprint,
            profile.storage().capture(),
            profile.runtime().recording_activation(),
            profile.runtime().recording_assembly(),
            profile.runtime().controlled_shutdown(),
            work_id_seed,
        )
        .map_err(|error| LinuxHostProcessStartError::Runtime(Box::new(error)))?;

        Ok(Self {
            runtime,
            storage,
            supervision_interval: profile.runtime().supervision_interval(),
            metrics_interval: profile.runtime().metrics_interval(),
        })
    }

    pub const fn supervision_interval(&self) -> Duration {
        self.supervision_interval
    }

    pub const fn metrics_interval(&self) -> Duration {
        self.metrics_interval
    }

    pub fn channel_metrics(&self) -> Vec<ChannelMetricsSnapshot> {
        self.runtime.channel_metrics()
    }

    pub fn node_metrics(&self) -> Vec<NodeMetricsSnapshot> {
        self.runtime.node_metrics()
    }

    pub fn startup_channel_faults(&self) -> &[crate::host_coordinator::StartupChannelFault] {
        self.runtime.startup_channel_faults()
    }

    pub fn inventory(&self) -> &DiscoveryInventory {
        self.runtime.inventory()
    }

    pub fn health_snapshot(&self) -> HostHealthSnapshot {
        self.runtime.health_snapshot()
    }

    pub fn time_health(&self) -> &TimePersistenceHealth {
        self.runtime.time_health()
    }

    pub fn storage_observation(&self) -> Result<StorageHealthObservation, StorageHealthProbeError> {
        self.storage.observe()
    }

    pub fn storage_root(&self) -> &Path {
        self.storage.root()
    }

    pub const fn storage_minimum_available_bytes(&self) -> u64 {
        self.storage.minimum_available_bytes()
    }

    pub fn channel_count(&self) -> usize {
        self.runtime.channels().len()
    }

    pub fn supervise_once(&mut self) -> Result<HostSupervisionReport, HostSupervisionError> {
        self.runtime.supervise_once_with_storage(&self.storage)
    }

    pub fn all_channels_finished(&self) -> bool {
        self.runtime
            .channels()
            .iter()
            .all(|channel| channel.is_finished())
    }

    pub fn request_stop(
        &mut self,
    ) -> Result<(), crate::channel_supervisor::ChannelSupervisorError> {
        self.runtime.request_stop()
    }

    pub fn shutdown(self) -> LinuxHostProcessShutdown {
        self.runtime.shutdown()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct HostProcessShutdownSummary {
    pub stop_request_failed: bool,
    pub channels: Vec<ChannelProcessShutdownSummary>,
    pub time_shutdown_failed: bool,
}

impl HostProcessShutdownSummary {
    pub fn clean(&self) -> bool {
        !self.stop_request_failed
            && !self.time_shutdown_failed
            && self
                .channels
                .iter()
                .all(ChannelProcessShutdownSummary::clean)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct ChannelProcessShutdownSummary {
    pub channel_id: Option<u8>,
    pub completed_steps: Option<u64>,
    pub worker_fault: Option<String>,
    pub join_error: Option<String>,
    pub protocol_error: Option<String>,
    pub persistence_error: Option<String>,
}

impl ChannelProcessShutdownSummary {
    pub fn clean(&self) -> bool {
        self.worker_fault.is_none()
            && self.join_error.is_none()
            && self.protocol_error.is_none()
            && self.persistence_error.is_none()
    }
}

pub fn summarize_shutdown<E>(
    shutdown: HostCoordinatorShutdown<E, ControlledChannelShutdownReport>,
) -> HostProcessShutdownSummary
where
    E: fmt::Debug,
{
    let channels = shutdown
        .channels
        .into_iter()
        .map(|result| match result {
            Ok((channel_id, exit)) => {
                let (completed_steps, worker_fault, report) = match exit {
                    ChannelWorkerExit::Stopped {
                        completed_steps,
                        shutdown,
                    } => (completed_steps, None, shutdown),
                    ChannelWorkerExit::Faulted {
                        completed_steps,
                        error,
                        shutdown,
                    } => (completed_steps, Some(format!("{error:?}")), shutdown),
                };
                ChannelProcessShutdownSummary {
                    channel_id: Some(channel_id),
                    completed_steps: Some(completed_steps),
                    worker_fault,
                    join_error: None,
                    protocol_error: report.outcome.err().map(|error| format!("{error:?}")),
                    persistence_error: report.persistence.err().map(|error| format!("{error:?}")),
                }
            }
            Err(error) => ChannelProcessShutdownSummary {
                channel_id: None,
                completed_steps: None,
                worker_fault: None,
                join_error: Some(format!("{error:?}")),
                protocol_error: None,
                persistence_error: None,
            },
        })
        .collect();
    HostProcessShutdownSummary {
        stop_request_failed: shutdown.stop_request.is_err(),
        channels,
        time_shutdown_failed: shutdown.time.is_err(),
    }
}

fn prepare_storage_health(
    capture_root: &Path,
    minimum_available_bytes: u64,
) -> Result<StorageHealthProbe, LinuxHostProcessStartError> {
    fs::create_dir_all(capture_root).map_err(LinuxHostProcessStartError::StorageRoot)?;
    let probe = StorageHealthProbe::new(
        capture_root,
        StorageHealthPolicy {
            minimum_available_bytes,
        },
    )
    .map_err(LinuxHostProcessStartError::StorageProbe)?;
    match probe
        .observe()
        .map_err(LinuxHostProcessStartError::StorageProbe)?
    {
        StorageHealthObservation::Healthy(_) => Ok(probe),
        StorageHealthObservation::Fault(fault) => {
            Err(LinuxHostProcessStartError::StorageUnavailable(fault))
        }
    }
}

#[derive(Debug)]
pub enum LinuxHostProcessStartError {
    HostClock(HostClockIdError),
    StorageRoot(io::Error),
    StorageProbe(StorageHealthProbeError),
    StorageUnavailable(crate::host_health::SharedStorageFault),
    Bootstrap(InstallationBootstrapError<SerialChannelBootstrapError>),
    RuntimeProfile(crate::runtime_profile::RuntimeProfileError),
    Planning(HostCoordinatorError),
    Runtime(Box<HostRuntimeStartError>),
}

impl fmt::Display for LinuxHostProcessStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "linux host process startup error: {self:?}")
    }
}

impl std::error::Error for LinuxHostProcessStartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HostClock(source) => Some(source),
            Self::StorageRoot(source) => Some(source),
            Self::StorageProbe(source) => Some(source),
            Self::Bootstrap(source) => Some(source),
            Self::RuntimeProfile(source) => Some(source),
            Self::Planning(source) => Some(source),
            Self::Runtime(source) => Some(source.as_ref()),
            Self::StorageUnavailable(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("sensor-host-process-{}-{id}", std::process::id()));
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn storage_preflight_creates_root_and_rejects_insufficient_reserve() {
        let temp = TempDirectory::new();
        let root = temp.0.join("capture");
        assert!(matches!(
            prepare_storage_health(&root, u64::MAX),
            Err(LinuxHostProcessStartError::StorageUnavailable(_))
        ));
        assert!(root.is_dir());
    }

    #[test]
    fn storage_preflight_returns_reusable_read_only_probe() {
        let temp = TempDirectory::new();
        let root = temp.0.join("capture");
        let probe = prepare_storage_health(&root, 1).unwrap();
        assert!(matches!(
            probe.observe().unwrap(),
            StorageHealthObservation::Healthy(_)
        ));
    }
}
