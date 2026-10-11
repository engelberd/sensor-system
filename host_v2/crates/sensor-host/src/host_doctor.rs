use core::fmt;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use nix::unistd::{access, AccessFlags};
use serde_json::{json, Value};

use crate::capture_export::validate_capture_chain;
use crate::channel_endpoint::{MonotonicClock, SystemMonotonicClock};
use crate::discovery::InstallationDiscoveryPlan;
use crate::discovery_executor::{execute_installation_discovery, DiscoveryExecutionPolicy};
use crate::host_health::SharedStorageFault;
use crate::installation_topology::HostInstallationProfile;
use crate::storage_health::{StorageHealthObservation, StorageHealthPolicy, StorageHealthProbe};
use crate::time_wal::{HostClockId, TimeWalReader};

pub const DOCTOR_REPORT_SCHEMA_VERSION: u16 = 1;
pub const DEFAULT_STATUS_MAX_AGE: Duration = Duration::from_secs(30);
const MAX_STATUS_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DoctorMode {
    Online,
    Offline,
    Deep,
}

impl DoctorMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Offline => "offline",
            Self::Deep => "deep",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DoctorSeverity {
    Ok,
    Warning,
    Error,
}

impl DoctorSeverity {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoctorFinding {
    pub severity: DoctorSeverity,
    pub code: &'static str,
    pub message: String,
}

impl DoctorFinding {
    fn new(severity: DoctorSeverity, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity,
            code,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoctorReport {
    pub mode: DoctorMode,
    pub system_name: Option<String>,
    pub findings: Vec<DoctorFinding>,
}

impl DoctorReport {
    pub fn worst_severity(&self) -> DoctorSeverity {
        self.findings
            .iter()
            .map(|finding| finding.severity)
            .max()
            .unwrap_or(DoctorSeverity::Error)
    }

    pub fn exit_code(&self) -> u8 {
        match self.worst_severity() {
            DoctorSeverity::Ok => 0,
            DoctorSeverity::Warning => 1,
            DoctorSeverity::Error => 2,
        }
    }

    pub fn to_json(&self) -> Value {
        let mut totals = BTreeMap::new();
        for severity in [
            DoctorSeverity::Ok,
            DoctorSeverity::Warning,
            DoctorSeverity::Error,
        ] {
            totals.insert(
                severity.label(),
                self.findings
                    .iter()
                    .filter(|finding| finding.severity == severity)
                    .count(),
            );
        }
        json!({
            "schema_version": DOCTOR_REPORT_SCHEMA_VERSION,
            "mode": self.mode.label(),
            "system_name": self.system_name,
            "result": self.worst_severity().label(),
            "exit_code": self.exit_code(),
            "summary": totals,
            "findings": self.findings.iter().map(|finding| json!({
                "severity": finding.severity.label(),
                "code": finding.code,
                "message": finding.message
            })).collect::<Vec<_>>()
        })
    }

    pub fn render_text(&self) -> String {
        let mut output = String::new();
        for finding in &self.findings {
            output.push_str(&format!(
                "[{:<7}] {:<28} {}\n",
                finding.severity.label().to_ascii_uppercase(),
                finding.code,
                finding.message
            ));
        }
        let ok = self
            .findings
            .iter()
            .filter(|finding| finding.severity == DoctorSeverity::Ok)
            .count();
        let warnings = self
            .findings
            .iter()
            .filter(|finding| finding.severity == DoctorSeverity::Warning)
            .count();
        let errors = self
            .findings
            .iter()
            .filter(|finding| finding.severity == DoctorSeverity::Error)
            .count();
        output.push_str(&format!(
            "Summary: {ok} ok, {warnings} warnings, {errors} errors\n"
        ));
        output
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoctorOptions {
    pub mode: DoctorMode,
    pub status_path: PathBuf,
    pub service_name: String,
    pub status_max_age: Duration,
}

impl Default for DoctorOptions {
    fn default() -> Self {
        Self {
            mode: DoctorMode::Online,
            status_path: PathBuf::from(crate::runtime_status::DEFAULT_RUNTIME_STATUS_PATH),
            service_name: "sensor-host-v2.service".to_owned(),
            status_max_age: DEFAULT_STATUS_MAX_AGE,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServiceState {
    Active,
    Inactive,
    Missing,
    Unavailable,
}

pub fn run_doctor(
    config_path: &Path,
    repository_root: &Path,
    options: &DoctorOptions,
) -> DoctorReport {
    let mut report = DoctorReport {
        mode: options.mode,
        system_name: None,
        findings: Vec::new(),
    };
    let profile = match HostInstallationProfile::load(config_path, repository_root) {
        Ok(profile) => profile,
        Err(error) => {
            report.findings.push(DoctorFinding::new(
                DoctorSeverity::Error,
                "config.invalid",
                format!("{}: {error}", config_path.display()),
            ));
            return report;
        }
    };
    report.system_name = Some(profile.system_name().to_owned());
    report.findings.push(DoctorFinding::new(
        DoctorSeverity::Ok,
        "config.valid",
        format!(
            "system={} fingerprint=fnv1a64:{:016x}",
            profile.system_name(),
            profile.config_fingerprint()
        ),
    ));

    inspect_topology(&profile, &mut report.findings);
    inspect_storage(&profile, &mut report.findings);

    if options.mode == DoctorMode::Offline {
        report.findings.push(DoctorFinding::new(
            DoctorSeverity::Ok,
            "runtime.offline",
            "runtime, systemd and clock checks intentionally skipped",
        ));
        return report;
    }

    let service_state = inspect_service(&options.service_name, &mut report.findings);
    inspect_clock(&mut report.findings);
    inspect_status(
        &profile,
        &options.status_path,
        options.status_max_age,
        service_state,
        &mut report.findings,
    );

    if options.mode == DoctorMode::Deep {
        inspect_deep(&profile, service_state, &mut report.findings);
    }
    report
}

fn inspect_topology(profile: &HostInstallationProfile, findings: &mut Vec<DoctorFinding>) {
    let mut enabled_channels = 0_usize;
    let mut enabled_nodes = 0_usize;
    for channel in profile.topology().channels() {
        if !channel.enabled() {
            findings.push(DoctorFinding::new(
                DoctorSeverity::Ok,
                "channel.disabled",
                format!("{} is disabled by configuration", channel.name()),
            ));
            continue;
        }
        enabled_channels += 1;
        enabled_nodes += channel.enabled_nodes().count();
        let endpoint = channel.transport().endpoint();
        match fs::symlink_metadata(endpoint) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    findings.push(DoctorFinding::new(
                        DoctorSeverity::Ok,
                        "port.stable-alias",
                        format!("{}: {}", channel.name(), endpoint.display()),
                    ));
                } else {
                    findings.push(DoctorFinding::new(
                        DoctorSeverity::Warning,
                        "port.unstable-path",
                        format!(
                            "{}: {} is not a symlink-backed stable alias",
                            channel.name(),
                            endpoint.display()
                        ),
                    ));
                }
                match fs::metadata(endpoint) {
                    Ok(target) if target.file_type().is_char_device() => {}
                    Ok(_) => findings.push(DoctorFinding::new(
                        DoctorSeverity::Error,
                        "port.not-character-device",
                        format!("{}: {}", channel.name(), endpoint.display()),
                    )),
                    Err(error) => findings.push(DoctorFinding::new(
                        DoctorSeverity::Error,
                        "port.target",
                        format!("{}: {}: {error}", channel.name(), endpoint.display()),
                    )),
                }
                match access(endpoint, AccessFlags::R_OK | AccessFlags::W_OK) {
                    Ok(()) => findings.push(DoctorFinding::new(
                        DoctorSeverity::Ok,
                        "port.permissions",
                        format!("{}: read/write access", channel.name()),
                    )),
                    Err(error) => findings.push(DoctorFinding::new(
                        DoctorSeverity::Error,
                        "port.permissions",
                        format!("{}: {}: {error}", channel.name(), endpoint.display()),
                    )),
                }
            }
            Err(error) => findings.push(DoctorFinding::new(
                DoctorSeverity::Error,
                "port.missing",
                format!("{}: {}: {error}", channel.name(), endpoint.display()),
            )),
        }
    }
    findings.push(DoctorFinding::new(
        DoctorSeverity::Ok,
        "topology.summary",
        format!("enabled_channels={enabled_channels} enabled_nodes={enabled_nodes}"),
    ));
}

fn inspect_storage(profile: &HostInstallationProfile, findings: &mut Vec<DoctorFinding>) {
    let storage = profile.storage();
    let paths = [
        ("capture", storage.capture().root()),
        ("checkpoint", storage.checkpoint_root()),
        ("time", storage.time_persistence().root()),
    ];
    for (label, path) in paths {
        match fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => match access(
                path,
                AccessFlags::R_OK | AccessFlags::W_OK | AccessFlags::X_OK,
            ) {
                Ok(()) => findings.push(DoctorFinding::new(
                    DoctorSeverity::Ok,
                    "storage.path-ready",
                    format!("{label}: {}", path.display()),
                )),
                Err(error) => findings.push(DoctorFinding::new(
                    DoctorSeverity::Error,
                    "storage.permissions",
                    format!("{label}: {}: {error}", path.display()),
                )),
            },
            Ok(_) => findings.push(DoctorFinding::new(
                DoctorSeverity::Error,
                "storage.not-directory",
                format!("{label}: {}", path.display()),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                findings.push(DoctorFinding::new(
                    DoctorSeverity::Warning,
                    "storage.path-missing",
                    format!("{label}: {}", path.display()),
                ));
            }
            Err(error) => findings.push(DoctorFinding::new(
                DoctorSeverity::Error,
                "storage.metadata",
                format!("{label}: {}: {error}", path.display()),
            )),
        }
    }

    let probe = match StorageHealthProbe::new(
        storage.capture().root(),
        StorageHealthPolicy {
            minimum_available_bytes: storage.minimum_free_bytes(),
        },
    ) {
        Ok(probe) => probe,
        Err(error) => {
            findings.push(DoctorFinding::new(
                DoctorSeverity::Error,
                "storage.probe",
                error.to_string(),
            ));
            return;
        }
    };
    match probe.observe() {
        Ok(StorageHealthObservation::Healthy(healthy)) => findings.push(DoctorFinding::new(
            DoctorSeverity::Ok,
            "storage.reserve",
            format!(
                "available_bytes={} required_bytes={}",
                healthy.available_bytes,
                probe.minimum_available_bytes()
            ),
        )),
        Ok(StorageHealthObservation::Fault(SharedStorageFault::ReserveBreached {
            available_bytes,
            required_bytes,
        })) => findings.push(DoctorFinding::new(
            DoctorSeverity::Error,
            "storage.reserve",
            format!("available_bytes={available_bytes} required_bytes={required_bytes}"),
        )),
        Ok(StorageHealthObservation::Fault(fault)) => findings.push(DoctorFinding::new(
            DoctorSeverity::Error,
            "storage.health",
            format!("{fault:?}"),
        )),
        Err(error) => findings.push(DoctorFinding::new(
            DoctorSeverity::Error,
            "storage.probe",
            error.to_string(),
        )),
    }
    match nix::sys::statfs::statfs(storage.capture().root()) {
        Ok(statistics) => findings.push(DoctorFinding::new(
            DoctorSeverity::Ok,
            "storage.filesystem",
            format!("type={}", statistics.filesystem_type_name()),
        )),
        Err(error) => findings.push(DoctorFinding::new(
            DoctorSeverity::Error,
            "storage.filesystem",
            error.to_string(),
        )),
    }
}

fn inspect_service(service_name: &str, findings: &mut Vec<DoctorFinding>) -> ServiceState {
    let output = Command::new("systemctl")
        .args([
            "show",
            service_name,
            "--property=LoadState",
            "--property=ActiveState",
            "--value",
        ])
        .output();
    let Ok(output) = output else {
        findings.push(DoctorFinding::new(
            DoctorSeverity::Warning,
            "service.unavailable",
            "systemctl is unavailable",
        ));
        return ServiceState::Unavailable;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let values = stdout.lines().map(str::trim).collect::<Vec<_>>();
    let load = values.first().copied().unwrap_or_default();
    let active = values.get(1).copied().unwrap_or_default();
    if load == "not-found" {
        findings.push(DoctorFinding::new(
            DoctorSeverity::Warning,
            "service.missing",
            format!("{service_name} is not installed"),
        ));
        ServiceState::Missing
    } else if active == "active" {
        findings.push(DoctorFinding::new(
            DoctorSeverity::Ok,
            "service.active",
            service_name,
        ));
        ServiceState::Active
    } else if output.status.success() || !load.is_empty() {
        findings.push(DoctorFinding::new(
            DoctorSeverity::Warning,
            "service.inactive",
            format!("{service_name}: load={load} active={active}"),
        ));
        ServiceState::Inactive
    } else {
        findings.push(DoctorFinding::new(
            DoctorSeverity::Warning,
            "service.unavailable",
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
        ServiceState::Unavailable
    }
}

fn inspect_clock(findings: &mut Vec<DoctorFinding>) {
    let output = Command::new("timedatectl")
        .args(["show", "--property=NTPSynchronized", "--value"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let synchronized = String::from_utf8_lossy(&output.stdout).trim() == "yes";
            findings.push(DoctorFinding::new(
                if synchronized {
                    DoctorSeverity::Ok
                } else {
                    DoctorSeverity::Warning
                },
                "clock.synchronized",
                if synchronized {
                    "system clock is synchronized"
                } else {
                    "system clock is not synchronized"
                },
            ));
        }
        _ => findings.push(DoctorFinding::new(
            DoctorSeverity::Warning,
            "clock.unavailable",
            "cannot query timedatectl",
        )),
    }
}

fn inspect_status(
    profile: &HostInstallationProfile,
    status_path: &Path,
    max_age: Duration,
    service_state: ServiceState,
    findings: &mut Vec<DoctorFinding>,
) {
    let value = match read_status(status_path) {
        Ok(value) => value,
        Err(error) => {
            findings.push(DoctorFinding::new(
                if service_state == ServiceState::Active {
                    DoctorSeverity::Error
                } else {
                    DoctorSeverity::Warning
                },
                "status.unavailable",
                format!("{}: {error}", status_path.display()),
            ));
            return;
        }
    };
    if let Err(error) = inspect_status_document(profile, &value, max_age, findings) {
        findings.push(DoctorFinding::new(
            DoctorSeverity::Error,
            "status.invalid",
            error,
        ));
    }
}

fn read_status(path: &Path) -> Result<Value, DoctorStatusError> {
    let metadata = fs::metadata(path).map_err(DoctorStatusError::Io)?;
    if metadata.len() > MAX_STATUS_BYTES {
        return Err(DoctorStatusError::TooLarge(metadata.len()));
    }
    let bytes = fs::read(path).map_err(DoctorStatusError::Io)?;
    serde_json::from_slice(&bytes).map_err(DoctorStatusError::Json)
}

fn inspect_status_document(
    profile: &HostInstallationProfile,
    status: &Value,
    max_age: Duration,
    findings: &mut Vec<DoctorFinding>,
) -> Result<(), String> {
    let object = status
        .as_object()
        .ok_or_else(|| "status root is not an object".to_owned())?;
    let schema = required_u64(object.get("schema_version"), "schema_version")?;
    if schema != u64::from(crate::runtime_status::RUNTIME_STATUS_SCHEMA_VERSION) {
        return Err(format!("unsupported schema_version={schema}"));
    }
    let system_name = required_str(object.get("system_name"), "system_name")?;
    if system_name != profile.system_name() {
        return Err(format!(
            "system_name mismatch: status={system_name} config={}",
            profile.system_name()
        ));
    }
    let config = required_object(object.get("config"), "config")?;
    let fingerprint = required_str(config.get("fingerprint"), "config.fingerprint")?;
    let expected_fingerprint = format!("{:016x}", profile.config_fingerprint());
    if fingerprint != expected_fingerprint {
        return Err(format!(
            "config fingerprint mismatch: status={fingerprint} config={expected_fingerprint}"
        ));
    }
    let software = required_object(object.get("software"), "software")?;
    let host_version = required_str(software.get("host_version"), "software.host_version")?;
    let framing = required_u64(
        software.get("framing_protocol"),
        "software.framing_protocol",
    )?;
    let application = required_u64(
        software.get("application_protocol"),
        "software.application_protocol",
    )?;
    let local_protocol = sensor_protocol::LOCAL_PROTOCOL_VERSION;
    if host_version != crate::HOST_SOFTWARE_VERSION
        || framing != u64::from(local_protocol.framing)
        || application != u64::from(local_protocol.application)
    {
        return Err(format!(
            "software mismatch: status={host_version}/{framing}/{application} local={}/{}/{}",
            crate::HOST_SOFTWARE_VERSION,
            local_protocol.framing,
            local_protocol.application
        ));
    }
    let process = required_object(object.get("process"), "process")?;
    let status_boot = required_str(process.get("host_boot_id"), "process.host_boot_id")?;
    let current_boot =
        HostClockId::read_current_linux_boot_id().map_err(|error| error.to_string())?;
    let current_boot_hex = bytes_hex(&current_boot.bytes());
    if status_boot != current_boot_hex {
        return Err(format!(
            "host boot mismatch: status={status_boot} current={current_boot_hex}"
        ));
    }
    let updated_us = required_u64(
        process.get("updated_monotonic_us"),
        "process.updated_monotonic_us",
    )?;
    let now_us = SystemMonotonicClock::try_new()
        .map_err(|error| error.to_string())?
        .now()
        .get();
    if updated_us > now_us {
        return Err("status monotonic timestamp is in the future".to_owned());
    }
    let age_us = now_us - updated_us;
    if age_us > duration_us(max_age) {
        findings.push(DoctorFinding::new(
            DoctorSeverity::Error,
            "status.stale",
            format!(
                "age_ms={} maximum_ms={}",
                age_us / 1_000,
                max_age.as_millis()
            ),
        ));
    } else {
        findings.push(DoctorFinding::new(
            DoctorSeverity::Ok,
            "status.fresh",
            format!("age_ms={}", age_us / 1_000),
        ));
    }

    let phase = required_str(process.get("phase"), "process.phase")?;
    findings.push(DoctorFinding::new(
        match phase {
            "recording" => DoctorSeverity::Ok,
            "degraded" | "starting" | "stopping" | "stopped" => DoctorSeverity::Warning,
            "faulted" => DoctorSeverity::Error,
            _ => return Err(format!("unknown process.phase={phase}")),
        },
        "status.process",
        format!("phase={phase}"),
    ));

    let health = required_object(object.get("health"), "health")?;
    let level = required_str(health.get("level"), "health.level")?;
    findings.push(DoctorFinding::new(
        match level {
            "healthy" => DoctorSeverity::Ok,
            "degraded" => DoctorSeverity::Warning,
            "stop_required" => DoctorSeverity::Error,
            _ => return Err(format!("unknown health.level={level}")),
        },
        "status.health",
        format!("level={level}"),
    ));

    let status_channels = required_array(object.get("channels"), "channels")?;
    for channel in profile.topology().enabled_channels() {
        let status_channel = status_channels.iter().find(|candidate| {
            candidate.get("channel_id").and_then(Value::as_u64)
                == Some(u64::from(channel.channel_id()))
        });
        let Some(status_channel) = status_channel else {
            findings.push(DoctorFinding::new(
                DoctorSeverity::Error,
                "status.channel-missing",
                channel.name(),
            ));
            continue;
        };
        let channel_phase = status_channel
            .get("phase")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        findings.push(DoctorFinding::new(
            match channel_phase {
                "running" | "stopped" => DoctorSeverity::Ok,
                "starting" | "stop_requested" => DoctorSeverity::Warning,
                _ => DoctorSeverity::Error,
            },
            "status.channel",
            format!("{} phase={channel_phase}", channel.name()),
        ));
        if let Some(metrics) = status_channel.get("metrics").and_then(Value::as_object) {
            let concerning = [
                "decode_errors",
                "disconnects",
                "transaction_timeouts",
                "wire_attempt_failures",
                "persistence_backpressure",
                "time_sync_backpressure",
                "storage_failed_groups",
            ]
            .into_iter()
            .filter_map(|name| {
                let count = metrics
                    .get(name)
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                (count > 0).then(|| format!("{name}={count}"))
            })
            .collect::<Vec<_>>();
            if !concerning.is_empty() {
                findings.push(DoctorFinding::new(
                    DoctorSeverity::Warning,
                    "status.channel-counters",
                    format!("{}: {}", channel.name(), concerning.join(" ")),
                ));
            }
        }
    }

    let status_nodes = required_array(object.get("nodes"), "nodes")?;
    for channel in profile.topology().enabled_channels() {
        for node in channel.enabled_nodes() {
            let status_node = status_nodes.iter().find(|candidate| {
                candidate.get("channel_id").and_then(Value::as_u64)
                    == Some(u64::from(channel.channel_id()))
                    && candidate.get("node_address").and_then(Value::as_u64)
                        == Some(u64::from(node.node_address()))
            });
            let Some(status_node) = status_node else {
                findings.push(DoctorFinding::new(
                    DoctorSeverity::Error,
                    "status.node-missing",
                    format!("{}/node-{}", channel.name(), node.node_address()),
                ));
                continue;
            };
            let hardware = status_node
                .get("hardware_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let board = status_node
                .get("board_revision")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let output_rate = status_node
                .get("output_rate_hz")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let expected_hardware = bytes_hex(&node.hardware_id().bytes());
            if hardware != expected_hardware
                || board != u64::from(node.board_revision())
                || output_rate != node.output_rate().hertz_label()
            {
                findings.push(DoctorFinding::new(
                    DoctorSeverity::Error,
                    "status.node-mismatch",
                    format!(
                        "{}/node-{} identity or output rate differs from config",
                        channel.name(),
                        node.node_address()
                    ),
                ));
            } else {
                findings.push(DoctorFinding::new(
                    DoctorSeverity::Ok,
                    "status.node",
                    format!(
                        "{}/node-{} hardware={hardware} odr={output_rate}",
                        channel.name(),
                        node.node_address()
                    ),
                ));
            }
            match status_node.get("cursors").and_then(Value::as_object) {
                Some(cursors) => {
                    let released = cursors.get("released").and_then(Value::as_u64);
                    let durable = cursors.get("durable").and_then(Value::as_u64);
                    let received = cursors.get("received").and_then(Value::as_u64);
                    let produced = cursors.get("produced").and_then(Value::as_u64);
                    if !matches!(
                        (released, durable, received, produced),
                        (Some(released), Some(durable), Some(received), Some(produced))
                            if released <= durable && durable <= received && received <= produced
                    ) {
                        findings.push(DoctorFinding::new(
                            DoctorSeverity::Error,
                            "status.cursor-order",
                            format!("{}/node-{}", channel.name(), node.node_address()),
                        ));
                    }
                }
                None => findings.push(DoctorFinding::new(
                    DoctorSeverity::Error,
                    "status.cursors-missing",
                    format!("{}/node-{}", channel.name(), node.node_address()),
                )),
            }
            if phase == "recording" {
                for (field, code) in [
                    ("last_measurement_age_ms", "status.measurement-missing"),
                    ("last_time_sync_age_ms", "status.time-sync-missing"),
                ] {
                    if status_node.get(field).and_then(Value::as_u64).is_none() {
                        findings.push(DoctorFinding::new(
                            DoctorSeverity::Warning,
                            code,
                            format!("{}/node-{}", channel.name(), node.node_address()),
                        ));
                    }
                }
            }
        }
    }
    findings.push(DoctorFinding::new(
        DoctorSeverity::Ok,
        "status.contract",
        format!("schema={schema} system={system_name}"),
    ));
    Ok(())
}

fn inspect_deep(
    profile: &HostInstallationProfile,
    service_state: ServiceState,
    findings: &mut Vec<DoctorFinding>,
) {
    if service_state == ServiceState::Active {
        findings.push(DoctorFinding::new(
            DoctorSeverity::Error,
            "deep.recorder-active",
            "refusing deep checks while sensor-host-v2.service is active",
        ));
        return;
    }
    match occupied_configured_ports(profile) {
        Ok(owners) if !owners.is_empty() => {
            findings.push(DoctorFinding::new(
                DoctorSeverity::Error,
                "deep.port-busy",
                owners.join(", "),
            ));
            return;
        }
        Ok(_) => findings.push(DoctorFinding::new(
            DoctorSeverity::Ok,
            "deep.ports-free",
            "no configured RS485 endpoint is owned by another process",
        )),
        Err(error) => {
            findings.push(DoctorFinding::new(
                DoctorSeverity::Error,
                "deep.port-ownership",
                error,
            ));
            return;
        }
    }

    let plan = InstallationDiscoveryPlan::from_topology(profile.topology());
    match execute_installation_discovery(
        &plan,
        DiscoveryExecutionPolicy::production(),
        fresh_session_seed(),
    ) {
        Ok(inventory) => findings.push(DoctorFinding::new(
            DoctorSeverity::Ok,
            "deep.discovery",
            format!("validated_nodes={}", inventory.nodes().len()),
        )),
        Err(error) => findings.push(DoctorFinding::new(
            DoctorSeverity::Error,
            "deep.discovery",
            error.to_string(),
        )),
    }

    match durable_storage_probe(profile.storage().capture().root()) {
        Ok(()) => findings.push(DoctorFinding::new(
            DoctorSeverity::Ok,
            "deep.storage-write",
            "write, fsync, atomic rename and directory fsync succeeded",
        )),
        Err(error) => findings.push(DoctorFinding::new(
            DoctorSeverity::Error,
            "deep.storage-write",
            error,
        )),
    }
    inspect_capture_tree(profile.storage().capture().root(), findings);
    inspect_time_tree(profile.storage().time_persistence().root(), findings);
}

fn occupied_configured_ports(profile: &HostInstallationProfile) -> Result<Vec<String>, String> {
    let proc = Path::new("/proc");
    if !proc.is_dir() {
        return Err("/proc is unavailable; cannot prove exclusive port ownership".to_owned());
    }
    let endpoints = profile
        .topology()
        .enabled_channels()
        .map(|channel| {
            fs::canonicalize(channel.transport().endpoint())
                .map(|path| (channel.name().to_owned(), path))
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut owners = BTreeSet::new();
    for entry in fs::read_dir(proc).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let fd_dir = entry.path().join("fd");
        let Ok(fds) = fs::read_dir(fd_dir) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = fs::read_link(fd.path()) else {
                continue;
            };
            for (channel_name, endpoint) in &endpoints {
                if target == *endpoint {
                    owners.insert(format!("{channel_name}: pid={pid}"));
                }
            }
        }
    }
    Ok(owners.into_iter().collect())
}

fn durable_storage_probe(root: &Path) -> Result<(), String> {
    let nonce = fresh_session_seed();
    let temporary = root.join(format!(".doctor-{nonce:08x}.tmp"));
    let published = root.join(format!(".doctor-{nonce:08x}.published"));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(b"sensor-system-doctor-v1\n")
            .map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);
        fs::rename(&temporary, &published).map_err(|error| error.to_string())?;
        File::open(root)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| error.to_string())?;
        fs::remove_file(&published).map_err(|error| error.to_string())?;
        File::open(root)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| error.to_string())
    })();
    let _ = fs::remove_file(&temporary);
    let _ = fs::remove_file(&published);
    result
}

fn inspect_capture_tree(root: &Path, findings: &mut Vec<DoctorFinding>) {
    let streams = match discover_parent_directories(root, |name| {
        name.starts_with("window-") && name.ends_with(".manifest")
    }) {
        Ok(streams) => streams,
        Err(error) => {
            findings.push(DoctorFinding::new(
                DoctorSeverity::Error,
                "deep.capture",
                error,
            ));
            return;
        }
    };
    if streams.is_empty() {
        findings.push(DoctorFinding::new(
            DoctorSeverity::Warning,
            "deep.capture-empty",
            format!("no sealed Capture stream below {}", root.display()),
        ));
        return;
    }
    let mut windows = 0_u64;
    let mut samples = 0_u64;
    for stream in &streams {
        match validate_capture_chain(stream) {
            Ok(summary) => {
                windows = windows.saturating_add(summary.window_count);
                samples = samples.saturating_add(summary.sample_count);
            }
            Err(error) => {
                findings.push(DoctorFinding::new(
                    DoctorSeverity::Error,
                    "deep.capture",
                    format!("{}: {error}", stream.display()),
                ));
                return;
            }
        }
    }
    findings.push(DoctorFinding::new(
        DoctorSeverity::Ok,
        "deep.capture",
        format!(
            "streams={} windows={windows} samples={samples}",
            streams.len()
        ),
    ));
}

fn inspect_time_tree(root: &Path, findings: &mut Vec<DoctorFinding>) {
    let files = match discover_files(root, |name| name.ends_with(".wal")) {
        Ok(files) => files,
        Err(error) => {
            findings.push(DoctorFinding::new(
                DoctorSeverity::Error,
                "deep.time-wal",
                error,
            ));
            return;
        }
    };
    if files.is_empty() {
        findings.push(DoctorFinding::new(
            DoctorSeverity::Warning,
            "deep.time-wal-empty",
            format!("no Time WAL below {}", root.display()),
        ));
        return;
    }
    let mut records = 0_u64;
    for path in &files {
        let mut reader = match TimeWalReader::open(path) {
            Ok(reader) => reader,
            Err(error) => {
                findings.push(DoctorFinding::new(
                    DoctorSeverity::Error,
                    "deep.time-wal",
                    format!("{}: {error}", path.display()),
                ));
                return;
            }
        };
        loop {
            match reader.next_observation() {
                Ok(Some(_)) => records = records.saturating_add(1),
                Ok(None) => break,
                Err(error) => {
                    findings.push(DoctorFinding::new(
                        DoctorSeverity::Error,
                        "deep.time-wal",
                        format!("{}: {error}", path.display()),
                    ));
                    return;
                }
            }
        }
    }
    findings.push(DoctorFinding::new(
        DoctorSeverity::Ok,
        "deep.time-wal",
        format!("streams={} records={records}", files.len()),
    ));
}

fn discover_parent_directories(
    root: &Path,
    matches: impl Fn(&str) -> bool,
) -> Result<BTreeSet<PathBuf>, String> {
    let mut directories = BTreeSet::new();
    walk_files(root, |path, name| {
        if matches(name) {
            if let Some(parent) = path.parent() {
                directories.insert(parent.to_path_buf());
            }
        }
    })?;
    Ok(directories)
}

fn discover_files(root: &Path, matches: impl Fn(&str) -> bool) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    walk_files(root, |path, name| {
        if matches(name) {
            files.push(path.to_path_buf());
        }
    })?;
    files.sort();
    Ok(files)
}

fn walk_files(root: &Path, mut visit: impl FnMut(&Path, &str)) -> Result<(), String> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in
            fs::read_dir(&directory).map_err(|error| format!("{}: {error}", directory.display()))?
        {
            let entry = entry.map_err(|error| error.to_string())?;
            let file_type = entry.file_type().map_err(|error| error.to_string())?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                if let Some(name) = entry.file_name().to_str() {
                    visit(&entry.path(), name);
                }
            }
        }
    }
    Ok(())
}

fn required_object<'a>(
    value: Option<&'a Value>,
    path: &str,
) -> Result<&'a serde_json::Map<String, Value>, String> {
    value
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{path} is missing or not an object"))
}

fn required_array<'a>(value: Option<&'a Value>, path: &str) -> Result<&'a Vec<Value>, String> {
    value
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{path} is missing or not an array"))
}

fn required_str<'a>(value: Option<&'a Value>, path: &str) -> Result<&'a str, String> {
    value
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{path} is missing or not a string"))
}

fn required_u64(value: Option<&Value>, path: &str) -> Result<u64, String> {
    value
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{path} is missing or not an unsigned integer"))
}

fn duration_us(duration: Duration) -> u64 {
    duration.as_micros().try_into().unwrap_or(u64::MAX)
}

fn fresh_session_seed() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    ((now as u32) ^ ((now >> 32) as u32) ^ std::process::id().rotate_left(11)).max(1)
}

fn bytes_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug)]
enum DoctorStatusError {
    Io(std::io::Error),
    Json(serde_json::Error),
    TooLarge(u64),
}

impl fmt::Display for DoctorStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Json(error) => error.fmt(formatter),
            Self::TooLarge(bytes) => write!(formatter, "status is too large: {bytes} bytes"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_exit_code_and_json_follow_worst_finding() {
        let report = DoctorReport {
            mode: DoctorMode::Online,
            system_name: Some("test".to_owned()),
            findings: vec![
                DoctorFinding::new(DoctorSeverity::Ok, "ok", "ready"),
                DoctorFinding::new(DoctorSeverity::Warning, "warn", "attention"),
            ],
        };
        assert_eq!(report.exit_code(), 1);
        assert_eq!(report.to_json()["result"], "warning");
        assert_eq!(report.to_json()["exit_code"], 1);
    }

    #[test]
    fn durable_probe_leaves_no_file_behind() {
        let root = std::env::temp_dir().join(format!(
            "sensor-doctor-storage-{}-{}",
            std::process::id(),
            fresh_session_seed()
        ));
        fs::create_dir(&root).unwrap();
        durable_storage_probe(&root).unwrap();
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn walkers_distinguish_capture_streams_and_time_files() {
        let root = std::env::temp_dir().join(format!(
            "sensor-doctor-walk-{}-{}",
            std::process::id(),
            fresh_session_seed()
        ));
        let stream = root.join("line-a/boot-1");
        fs::create_dir_all(&stream).unwrap();
        fs::write(stream.join("window-1.manifest"), b"x").unwrap();
        fs::write(stream.join("time.wal"), b"x").unwrap();
        assert_eq!(
            discover_parent_directories(&root, |name| name.ends_with(".manifest")).unwrap(),
            BTreeSet::from([stream.clone()])
        );
        assert_eq!(
            discover_files(&root, |name| name.ends_with(".wal")).unwrap(),
            vec![stream.join("time.wal")]
        );
        fs::remove_dir_all(root).unwrap();
    }
}
