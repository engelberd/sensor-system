use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use sensor_host::channel_endpoint::{MonotonicClock, SystemMonotonicClock};
use sensor_host::channel_supervisor::ChannelWorkerPhase;
use sensor_host::discovery::InstallationDiscoveryPlan;
use sensor_host::discovery_executor::{execute_installation_discovery, DiscoveryExecutionPolicy};
use sensor_host::host_health::{HostHealthLevel, HostHealthReason, HostHealthSnapshot};
use sensor_host::host_process::{summarize_shutdown, HostProcessShutdownSummary, LinuxHostProcess};
use sensor_host::installation_topology::HostInstallationProfile;
use sensor_host::operations_ipc::OperationsIpcServer;
use sensor_host::runtime_status::{
    unix_time_ms, HostRuntimeObservation, RuntimeProcessPhase, RuntimeStatusIdentity,
    RuntimeStatusPublisher,
};
use sensor_host::termination_signal::TerminationSignalReceiver;
use sensor_host::time_wal::HostClockId;

fn main() -> ExitCode {
    match parse_arguments(std::env::args_os().skip(1)) {
        Ok(Command::Version) => {
            println!("{}", sensor_host::version_line());
            ExitCode::SUCCESS
        }
        Ok(Command::Validate {
            config,
            repository_root,
        }) => match validate(config, repository_root) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("sensor-host: configuration rejected: {error}");
                ExitCode::from(2)
            }
        },
        Ok(Command::Discover {
            config,
            repository_root,
        }) => match discover(config, repository_root) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("sensor-host: discovery failed: {error}");
                ExitCode::from(2)
            }
        },
        Ok(Command::Paths {
            config,
            repository_root,
            initialize,
        }) => match paths(config, repository_root, initialize) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("sensor-host: cannot resolve paths: {error}");
                ExitCode::from(2)
            }
        },
        Ok(Command::Run {
            config,
            repository_root,
            status_file,
            operations_socket,
        }) => match run(config, repository_root, status_file, operations_socket) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("sensor-host: run failed: {error}");
                ExitCode::from(error.exit_code())
            }
        },
        Ok(Command::Help) => {
            print_usage();
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("sensor-host: {error}");
            print_usage();
            ExitCode::from(2)
        }
    }
}

fn run(
    config: PathBuf,
    repository_root: PathBuf,
    status_file: PathBuf,
    operations_socket: PathBuf,
) -> Result<(), RunCommandError> {
    // Install before any worker is created. The blocked mask is inherited by
    // every channel thread, so SIGINT/SIGTERM has one synchronous owner.
    let signals = TerminationSignalReceiver::install().map_err(RunCommandError::Signal)?;
    let profile =
        HostInstallationProfile::load(config, repository_root).map_err(RunCommandError::Profile)?;
    let system_name = profile.system_name().to_owned();
    let clock = SystemMonotonicClock::try_new().map_err(RunCommandError::Clock)?;
    let host_boot_id = HostClockId::read_current_linux_boot_id().map_err(|error| {
        RunCommandError::Start(
            sensor_host::host_process::LinuxHostProcessStartError::HostClock(error),
        )
    })?;
    let session_seed = fresh_session_seed();
    let work_id_seed = fresh_work_id_seed();
    let identity = RuntimeStatusIdentity {
        system_name: system_name.clone(),
        config_fingerprint: profile.config_fingerprint(),
        process_id: std::process::id(),
        process_instance_id: format!("{session_seed:08x}-{work_id_seed:016x}"),
        host_boot_id,
        started_unix_ms: unix_time_ms().map_err(RunCommandError::Status)?,
        started_monotonic_us: clock.now().get(),
    };
    let mut status =
        RuntimeStatusPublisher::new(status_file, identity).map_err(RunCommandError::Status)?;
    status
        .publish(
            RuntimeProcessPhase::Starting,
            clock.now().get(),
            None,
            None,
            None,
        )
        .map_err(RunCommandError::Status)?;
    let mut process = match LinuxHostProcess::start_with_host_clock(
        &profile,
        host_boot_id,
        session_seed,
        work_id_seed,
    ) {
        Ok(process) => process,
        Err(error) => {
            let message = error.to_string();
            let _ = status.publish(
                RuntimeProcessPhase::Faulted,
                clock.now().get(),
                None,
                Some(&message),
                Some(false),
            );
            return Err(RunCommandError::Start(error));
        }
    };
    let operations =
        match OperationsIpcServer::bind(operations_socket, process.channel_operations()) {
            Ok(server) => server,
            Err(error) => {
                let message = error.to_string();
                let _ = status.publish(
                    RuntimeProcessPhase::Faulted,
                    clock.now().get(),
                    Some(&observe_runtime(&process)),
                    Some(&message),
                    Some(false),
                );
                let _ = process.request_stop();
                let _ = process.shutdown();
                return Err(RunCommandError::Operations(error));
            }
        };
    let initial_observation = observe_runtime(&process);
    let mut cause = None;
    if let Err(error) = status.publish(
        runtime_phase(&initial_observation),
        clock.now().get(),
        Some(&initial_observation),
        None,
        None,
    ) {
        cause = Some(error.to_string());
        let _ = process.request_stop();
    }
    println!(
        "host running: system={} channels={} startup_faults={} supervision_interval_ms={} metrics_interval_ms={}",
        system_name,
        process.channel_count(),
        process.startup_channel_faults().len(),
        process.supervision_interval().as_millis(),
        process.metrics_interval().as_millis()
    );
    for fault in process.startup_channel_faults() {
        eprintln!(
            "channel unavailable at startup: id={} name={} error={}",
            fault.channel_id, fault.channel_name, fault.detail
        );
    }

    let mut next_metrics = Instant::now() + process.metrics_interval();
    while cause.is_none() {
        if let Err(error) = operations.poll() {
            cause = Some(error.to_string());
            let _ = process.request_stop();
            break;
        }
        match signals.try_receive() {
            Ok(Some(signal)) => {
                println!("termination requested: signal={signal:?}");
                if let Err(error) = process.request_stop() {
                    cause = Some(format!("failed to request controlled stop: {error}"));
                }
                break;
            }
            Ok(None) => {}
            Err(error) => {
                cause = Some(error.to_string());
                let _ = process.request_stop();
                break;
            }
        }

        if process.all_channels_finished() {
            cause = Some("all channel workers ended before a termination request".to_owned());
            let _ = process.request_stop();
            break;
        }

        if let Err(error) = process.supervise_once() {
            cause = Some(error.to_string());
            // Global supervision faults already request stop. Calling this
            // again is idempotent and also covers future error variants.
            let _ = process.request_stop();
            break;
        }
        if Instant::now() >= next_metrics {
            print_runtime_metrics(&process);
            let observation = observe_runtime(&process);
            if let Err(error) = status.publish(
                runtime_phase(&observation),
                clock.now().get(),
                Some(&observation),
                None,
                None,
            ) {
                cause = Some(error.to_string());
                let _ = process.request_stop();
                break;
            }
            next_metrics = Instant::now() + process.metrics_interval();
        }
        std::thread::sleep(process.supervision_interval());
    }

    // Always leave one final snapshot, including for short commissioning runs
    // that end before the periodic interval.
    print_runtime_metrics(&process);
    let stopping_observation = observe_runtime(&process);
    if let Err(error) = status.publish(
        RuntimeProcessPhase::Stopping,
        clock.now().get(),
        Some(&stopping_observation),
        cause.as_deref(),
        None,
    ) {
        if cause.is_none() {
            cause = Some(error.to_string());
        }
    }
    let summary = summarize_shutdown(process.shutdown());
    print_shutdown_summary(&summary);
    let clean = cause.is_none() && summary.clean();
    let mut terminal_observation = stopping_observation;
    apply_shutdown_summary(&mut terminal_observation.health, &summary);
    let terminal_error = if clean {
        None
    } else {
        Some(
            cause
                .clone()
                .unwrap_or_else(|| format!("controlled shutdown was not clean: {summary:?}")),
        )
    };
    status
        .publish(
            if clean {
                RuntimeProcessPhase::Stopped
            } else {
                RuntimeProcessPhase::Faulted
            },
            clock.now().get(),
            Some(&terminal_observation),
            terminal_error.as_deref(),
            Some(clean),
        )
        .map_err(RunCommandError::Status)?;
    if clean {
        Ok(())
    } else {
        Err(RunCommandError::Execution { cause, summary })
    }
}

fn apply_shutdown_summary(health: &mut HostHealthSnapshot, summary: &HostProcessShutdownSummary) {
    for channel in &mut health.channels {
        let Some(shutdown) = summary
            .channels
            .iter()
            .find(|shutdown| shutdown.channel_id == Some(channel.channel_id))
        else {
            channel.phase = ChannelWorkerPhase::Faulted;
            continue;
        };
        if let Some(completed_steps) = shutdown.completed_steps {
            channel.completed_steps = completed_steps;
        }
        channel.phase = if shutdown.clean() {
            ChannelWorkerPhase::Stopped
        } else {
            ChannelWorkerPhase::Faulted
        };
    }

    health.reasons.retain(|reason| {
        !matches!(
            reason,
            HostHealthReason::ChannelFaulted { .. } | HostHealthReason::ChannelTerminated { .. }
        )
    });
    for channel in &health.channels {
        if channel.phase == ChannelWorkerPhase::Faulted {
            health.reasons.push(HostHealthReason::ChannelFaulted {
                channel_id: channel.channel_id,
            });
        }
    }
    health.level = if health.reasons.iter().any(|reason| {
        matches!(
            reason,
            HostHealthReason::TimeIntegrityFault | HostHealthReason::SharedStorageFault(_)
        )
    }) {
        HostHealthLevel::StopRequired
    } else if health.reasons.is_empty() {
        HostHealthLevel::Healthy
    } else {
        HostHealthLevel::Degraded
    };
}

fn observe_runtime(process: &LinuxHostProcess) -> HostRuntimeObservation {
    HostRuntimeObservation {
        health: process.health_snapshot(),
        channel_metrics: process.channel_metrics(),
        node_metrics: process.node_metrics(),
        nodes: process.inventory().nodes().to_vec(),
        time_streams: process.time_health().streams().to_vec(),
        storage_root: process.storage_root().to_path_buf(),
        storage_minimum_available_bytes: process.storage_minimum_available_bytes(),
        storage: process
            .storage_observation()
            .map_err(|error| error.to_string()),
    }
}

fn runtime_phase(observation: &HostRuntimeObservation) -> RuntimeProcessPhase {
    match observation.health.level {
        sensor_host::host_health::HostHealthLevel::Healthy => RuntimeProcessPhase::Recording,
        sensor_host::host_health::HostHealthLevel::Degraded => RuntimeProcessPhase::Degraded,
        sensor_host::host_health::HostHealthLevel::StopRequired => RuntimeProcessPhase::Faulted,
    }
}

fn print_runtime_metrics(process: &LinuxHostProcess) {
    for metrics in process.channel_metrics() {
        println!(
            "channel metrics: id={} tx={} decode_errors={} disconnects={} timeouts={} wire_failures={} late={} unrelated={} unsolicited={} persisted={} no_data={} measurement_retries={} recoveries={} persistence_submitted={} persistence_backpressure={} time_sync={} time_sync_retries={} time_sync_backpressure={} max_step_us={} storage_groups={} storage_records={} storage_failures={} storage_max_persist_us={} storage_max_group_records={} storage_queue_high_water={}",
            metrics.channel_id,
            metrics.transmissions,
            metrics.decode_errors,
            metrics.disconnects,
            metrics.transaction_timeouts,
            metrics.wire_attempt_failures,
            metrics.late_responses,
            metrics.unrelated_responses,
            metrics.unsolicited_responses,
            metrics.measurement_persisted,
            metrics.measurement_no_data,
            metrics.measurement_retries,
            metrics.measurement_recoveries,
            metrics.persistence_submitted,
            metrics.persistence_backpressure,
            metrics.time_sync_submitted,
            metrics.time_sync_retries,
            metrics.time_sync_backpressure,
            metrics.maximum_step_us,
            metrics.storage_committed_groups,
            metrics.storage_committed_records,
            metrics.storage_failed_groups,
            metrics.storage_maximum_persist_us,
            metrics.storage_maximum_group_records,
            metrics.storage_queue_high_water,
        );
    }
}

fn print_shutdown_summary(summary: &HostProcessShutdownSummary) {
    println!(
        "host shutdown: clean={} channels={} stop_request_failed={} time_shutdown_failed={}",
        summary.clean(),
        summary.channels.len(),
        summary.stop_request_failed,
        summary.time_shutdown_failed
    );
    for channel in &summary.channels {
        println!(
            "channel shutdown: id={:?} steps={:?} clean={} worker_fault={:?} join_error={:?} protocol_error={:?} persistence_error={:?}",
            channel.channel_id,
            channel.completed_steps,
            channel.clean(),
            channel.worker_fault,
            channel.join_error,
            channel.protocol_error,
            channel.persistence_error
        );
    }
}

fn discover(config: PathBuf, repository_root: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let profile = HostInstallationProfile::load(config, repository_root)?;
    let plan = InstallationDiscoveryPlan::from_topology(profile.topology());
    let inventory = execute_installation_discovery(
        &plan,
        DiscoveryExecutionPolicy::production(),
        fresh_session_seed(),
    )?;
    println!(
        "discovery complete: system={} active_channels={} active_nodes={}",
        profile.system_name(),
        plan.channels().len(),
        inventory.nodes().len()
    );
    for node in inventory.nodes() {
        let identity = node.identity();
        let capabilities = node.capabilities();
        let capture_stream = profile.storage().capture().stream_directory(
            node.channel_name(),
            identity.hardware_id,
            identity.boot_epoch,
        )?;
        println!(
            "node: channel={} name={} address={} hardware={} board={} firmware={}.{}.{} boot_epoch={} output_rate_hz={} journal={} max_chunk={} capture_stream={}",
            node.channel_id(),
            node.channel_name(),
            node.node_address(),
            hardware_id_hex(identity.hardware_id),
            identity.board_revision,
            identity.firmware.major,
            identity.firmware.minor,
            identity.firmware.patch,
            identity.boot_epoch.get(),
            node.output_rate().hertz_label(),
            capabilities.journal_capacity_samples,
            capabilities.max_sample_chunk_samples,
            capture_stream.display(),
        );
    }
    Ok(())
}

fn hardware_id_hex(hardware_id: sensor_core::HardwareId) -> String {
    use core::fmt::Write;
    let mut output = String::with_capacity(16);
    for byte in hardware_id.bytes() {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn fresh_session_seed() -> u32 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let folded = (now as u32) ^ ((now >> 32) as u32) ^ std::process::id().rotate_left(11);
    folded.max(1)
}

fn fresh_work_id_seed() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let folded =
        (now as u64) ^ ((now >> 64) as u64) ^ u64::from(std::process::id()).rotate_left(23);
    folded.max(1)
}

fn validate(config: PathBuf, repository_root: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let profile = HostInstallationProfile::load(config, repository_root)?;
    let discovery = InstallationDiscoveryPlan::from_topology(profile.topology());
    println!(
        "valid host profile: system={} configured_channels={} active_channels={} active_nodes={}",
        profile.system_name(),
        profile.topology().channels().len(),
        discovery.channels().len(),
        discovery.target_count()
    );
    println!(
        "storage: capture_root={} analysis_root={} analysis_enabled={} analysis_scan_interval_s={}",
        profile.storage().capture().root().display(),
        profile.storage().analysis().root().display(),
        profile.storage().analysis().enabled(),
        profile.storage().analysis().scan_interval_seconds(),
    );
    for channel in discovery.channels() {
        println!(
            "channel: id={} name={} transport_device={} endpoint={} active_nodes={}",
            channel.channel_id(),
            channel.channel_name(),
            channel.transport().device_id(),
            channel.transport().endpoint().display(),
            channel.targets().len()
        );
    }
    Ok(())
}

fn paths(
    config: PathBuf,
    repository_root: PathBuf,
    initialize: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let profile = HostInstallationProfile::load(&config, &repository_root)?;
    let storage = profile.storage();
    let entries = [
        ("repository", repository_root.as_path()),
        ("config", config.as_path()),
        ("capture", storage.capture().root()),
        ("checkpoint", storage.checkpoint_root()),
        ("time", storage.time_persistence().root()),
        ("analysis", storage.analysis().root()),
    ];
    if initialize {
        for (_, path) in &entries[2..] {
            std::fs::create_dir_all(path)?;
        }
    }
    for (name, path) in entries {
        let state = if path.exists() { "exists" } else { "missing" };
        println!("{name:12} {} [{state}]", path.display());
    }
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
enum Command {
    Version,
    Validate {
        config: PathBuf,
        repository_root: PathBuf,
    },
    Discover {
        config: PathBuf,
        repository_root: PathBuf,
    },
    Paths {
        config: PathBuf,
        repository_root: PathBuf,
        initialize: bool,
    },
    Run {
        config: PathBuf,
        repository_root: PathBuf,
        status_file: PathBuf,
        operations_socket: PathBuf,
    },
    Help,
}

fn parse_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<Command, &'static str> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    if arguments.as_slice() == [OsStr::new("--version")] {
        return Ok(Command::Version);
    }
    if arguments.as_slice() == [OsStr::new("--help")]
        || arguments.as_slice() == [OsStr::new("help")]
    {
        return Ok(Command::Help);
    }
    if arguments.first().is_some_and(|value| {
        value == "validate" || value == "discover" || value == "paths" || value == "run"
    }) {
        let operation = arguments[0].clone();
        let mut config = None;
        let mut repository_root = None;
        let mut status_file = None;
        let mut operations_socket = None;
        let mut initialize = false;
        let mut index = 1;
        while index < arguments.len() {
            let flag = &arguments[index];
            if flag == "--init" && operation == "paths" && !initialize {
                initialize = true;
                index += 1;
                continue;
            }
            let value = arguments
                .get(index + 1)
                .ok_or("operation option requires a value")?;
            match flag.to_str() {
                Some("--config") if config.is_none() => config = Some(PathBuf::from(value)),
                Some("--repository-root") if repository_root.is_none() => {
                    repository_root = Some(PathBuf::from(value));
                }
                Some("--status-file") if operation == "run" && status_file.is_none() => {
                    status_file = Some(PathBuf::from(value));
                }
                Some("--operations-socket")
                    if operation == "run" && operations_socket.is_none() =>
                {
                    operations_socket = Some(PathBuf::from(value));
                }
                _ => return Err("unknown or repeated operation option"),
            }
            index += 2;
        }
        let config = config.ok_or("operation requires --config")?;
        let repository_root = repository_root.ok_or("operation requires --repository-root")?;
        return match operation.to_str() {
            Some("validate") => Ok(Command::Validate {
                config,
                repository_root,
            }),
            Some("discover") => Ok(Command::Discover {
                config,
                repository_root,
            }),
            Some("paths") => Ok(Command::Paths {
                config,
                repository_root,
                initialize,
            }),
            Some("run") => Ok(Command::Run {
                config,
                repository_root,
                status_file: status_file.ok_or("run requires --status-file")?,
                operations_socket: operations_socket.ok_or("run requires --operations-socket")?,
            }),
            _ => unreachable!("operation was matched above"),
        };
    }
    Err("expected --version, --help, validate, discover, paths, or run")
}

fn print_usage() {
    eprintln!(
        "usage:\n  sensor-host --version\n  sensor-host validate --config <profile.json> --repository-root <path>\n  sensor-host discover --config <profile.json> --repository-root <path>\n  sensor-host paths --config <profile.json> --repository-root <path> [--init]\n  sensor-host run --config <profile.json> --repository-root <path> --status-file <path> --operations-socket <path>"
    );
}

#[derive(Debug)]
enum RunCommandError {
    Signal(sensor_host::termination_signal::TerminationSignalError),
    Clock(sensor_host::channel_endpoint::SystemMonotonicClockError),
    Profile(sensor_host::installation_topology::HostInstallationProfileError),
    Start(sensor_host::host_process::LinuxHostProcessStartError),
    Status(sensor_host::runtime_status::RuntimeStatusError),
    Operations(sensor_host::operations_ipc::OperationsIpcError),
    Execution {
        cause: Option<String>,
        summary: HostProcessShutdownSummary,
    },
}

impl RunCommandError {
    /// Stable process codes used by the systemd restart contract.
    ///
    /// `75` (temporary failure) may be retried with a bounded start limit.
    /// Configuration and local I/O failures require operator intervention and
    /// are listed in `RestartPreventExitStatus` by the installed unit.
    const fn exit_code(&self) -> u8 {
        match self {
            Self::Profile(_) => 78, // EX_CONFIG
            Self::Status(_) => 73,  // EX_CANTCREAT
            Self::Operations(_) => 73,
            Self::Start(error) => match error {
                sensor_host::host_process::LinuxHostProcessStartError::Bootstrap(_) => 75,
                sensor_host::host_process::LinuxHostProcessStartError::StorageRoot(_)
                | sensor_host::host_process::LinuxHostProcessStartError::StorageProbe(_)
                | sensor_host::host_process::LinuxHostProcessStartError::StorageUnavailable(_) => {
                    73
                }
                sensor_host::host_process::LinuxHostProcessStartError::RuntimeProfile(_)
                | sensor_host::host_process::LinuxHostProcessStartError::Planning(_) => 78,
                sensor_host::host_process::LinuxHostProcessStartError::HostClock(_) => 69,
                // A composed runtime startup failure is deterministic for the
                // current node/storage state. Retrying it in a service loop
                // cannot repair that state and repeatedly touches every bus.
                // Faults after a successful start use `Execution` below and
                // remain restartable.
                sensor_host::host_process::LinuxHostProcessStartError::Runtime(_) => 78,
            },
            Self::Signal(_) | Self::Clock(_) => 70,
            Self::Execution { .. } => 1,
        }
    }
}

impl std::fmt::Display for RunCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Signal(error) => error.fmt(formatter),
            Self::Clock(error) => error.fmt(formatter),
            Self::Profile(error) => error.fmt(formatter),
            Self::Start(error) => error.fmt(formatter),
            Self::Status(error) => error.fmt(formatter),
            Self::Operations(error) => error.fmt(formatter),
            Self::Execution { cause, summary } => write!(
                formatter,
                "controlled shutdown was not clean: cause={cause:?}, summary={summary:?}"
            ),
        }
    }
}

impl std::error::Error for RunCommandError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_explicit_validate_paths_in_any_order() {
        assert_eq!(
            parse_arguments(strings(&[
                "validate",
                "--repository-root",
                "/repo",
                "--config",
                "/etc/sensor.json"
            ])),
            Ok(Command::Validate {
                config: PathBuf::from("/etc/sensor.json"),
                repository_root: PathBuf::from("/repo"),
            })
        );
    }

    #[test]
    fn rejects_implicit_or_repeated_runtime_paths() {
        assert!(parse_arguments(strings(&["validate", "profile.json"])).is_err());
        assert!(parse_arguments(strings(&[
            "validate",
            "--config",
            "a.json",
            "--config",
            "b.json",
            "--repository-root",
            "/repo"
        ]))
        .is_err());
    }

    #[test]
    fn parses_read_only_discovery_without_implicit_paths() {
        assert_eq!(
            parse_arguments(strings(&[
                "discover",
                "--config",
                "/etc/sensor.json",
                "--repository-root",
                "/repo"
            ])),
            Ok(Command::Discover {
                config: PathBuf::from("/etc/sensor.json"),
                repository_root: PathBuf::from("/repo"),
            })
        );
        assert_ne!(fresh_session_seed(), 0);
        assert_ne!(fresh_work_id_seed(), 0);
    }

    #[test]
    fn parses_paths_with_optional_initialization() {
        assert_eq!(
            parse_arguments(strings(&[
                "paths",
                "--config",
                "/etc/sensor.json",
                "--repository-root",
                "/repo",
                "--init"
            ])),
            Ok(Command::Paths {
                config: PathBuf::from("/etc/sensor.json"),
                repository_root: PathBuf::from("/repo"),
                initialize: true,
            })
        );
        assert!(parse_arguments(strings(&[
            "validate",
            "--config",
            "/etc/sensor.json",
            "--repository-root",
            "/repo",
            "--init"
        ]))
        .is_err());
    }

    #[test]
    fn parses_production_run_with_explicit_paths() {
        assert_eq!(
            parse_arguments(strings(&[
                "run",
                "--config",
                "/etc/sensor.json",
                "--repository-root",
                "/repo",
                "--status-file",
                "/run/status.json",
                "--operations-socket",
                "/run/operations.sock"
            ])),
            Ok(Command::Run {
                config: PathBuf::from("/etc/sensor.json"),
                repository_root: PathBuf::from("/repo"),
                status_file: PathBuf::from("/run/status.json"),
                operations_socket: PathBuf::from("/run/operations.sock"),
            })
        );
    }

    #[test]
    fn parses_explicit_runtime_status_path() {
        assert_eq!(
            parse_arguments(strings(&[
                "run",
                "--config",
                "/etc/sensor.json",
                "--repository-root",
                "/repo",
                "--status-file",
                "/run/sensor-system/host-v2-status.json",
                "--operations-socket",
                "/run/sensor-system/operations-v1.sock"
            ])),
            Ok(Command::Run {
                config: PathBuf::from("/etc/sensor.json"),
                repository_root: PathBuf::from("/repo"),
                status_file: PathBuf::from("/run/sensor-system/host-v2-status.json"),
                operations_socket: PathBuf::from("/run/sensor-system/operations-v1.sock"),
            })
        );
    }

    #[test]
    fn rejects_run_without_a_status_destination() {
        assert!(parse_arguments(strings(&[
            "run",
            "--config",
            "/etc/sensor.json",
            "--repository-root",
            "/repo",
            "--operations-socket",
            "/run/operations.sock"
        ]))
        .is_err());
    }

    #[test]
    fn rejects_run_without_an_operations_destination() {
        assert!(parse_arguments(strings(&[
            "run",
            "--config",
            "/etc/sensor.json",
            "--repository-root",
            "/repo",
            "--status-file",
            "/run/status.json"
        ]))
        .is_err());
    }

    fn shutdown_channel(
        channel_id: Option<u8>,
        completed_steps: Option<u64>,
        worker_fault: Option<&str>,
    ) -> sensor_host::host_process::ChannelProcessShutdownSummary {
        sensor_host::host_process::ChannelProcessShutdownSummary {
            channel_id,
            completed_steps,
            worker_fault: worker_fault.map(str::to_owned),
            join_error: None,
            protocol_error: None,
            persistence_error: None,
        }
    }

    fn running_health() -> HostHealthSnapshot {
        HostHealthSnapshot {
            level: HostHealthLevel::Healthy,
            coordinator: sensor_host::host_health::HostCoordinatorLifecycle::StopRequested,
            time: sensor_host::time_health::TimePersistenceHealthState::Healthy,
            channels: vec![
                sensor_host::channel_supervisor::ChannelWorkerSnapshot {
                    channel_id: 1,
                    phase: ChannelWorkerPhase::Running,
                    completed_steps: 10,
                },
                sensor_host::channel_supervisor::ChannelWorkerSnapshot {
                    channel_id: 2,
                    phase: ChannelWorkerPhase::StopRequested,
                    completed_steps: 20,
                },
            ],
            reasons: Vec::new(),
        }
    }

    #[test]
    fn terminal_status_projects_clean_join_reports_as_stopped() {
        let mut health = running_health();
        let summary = HostProcessShutdownSummary {
            stop_request_failed: false,
            channels: vec![
                shutdown_channel(Some(1), Some(11), None),
                shutdown_channel(Some(2), Some(21), None),
            ],
            time_shutdown_failed: false,
        };

        apply_shutdown_summary(&mut health, &summary);

        assert_eq!(health.level, HostHealthLevel::Healthy);
        assert!(health.reasons.is_empty());
        assert_eq!(health.channels[0].phase, ChannelWorkerPhase::Stopped);
        assert_eq!(health.channels[0].completed_steps, 11);
        assert_eq!(health.channels[1].phase, ChannelWorkerPhase::Stopped);
        assert_eq!(health.channels[1].completed_steps, 21);
    }

    #[test]
    fn terminal_status_exposes_faulted_or_missing_join_reports() {
        let mut health = running_health();
        let summary = HostProcessShutdownSummary {
            stop_request_failed: false,
            channels: vec![shutdown_channel(Some(1), Some(12), Some("wire fault"))],
            time_shutdown_failed: false,
        };

        apply_shutdown_summary(&mut health, &summary);

        assert_eq!(health.level, HostHealthLevel::Degraded);
        assert_eq!(health.channels[0].phase, ChannelWorkerPhase::Faulted);
        assert_eq!(health.channels[0].completed_steps, 12);
        assert_eq!(health.channels[1].phase, ChannelWorkerPhase::Faulted);
        assert!(health
            .reasons
            .contains(&HostHealthReason::ChannelFaulted { channel_id: 1 }));
        assert!(health
            .reasons
            .contains(&HostHealthReason::ChannelFaulted { channel_id: 2 }));
    }
}
