use core::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::channel_metrics::ChannelMetricsSnapshot;
use crate::channel_supervisor::ChannelWorkerPhase;
use crate::discovery::DiscoveredNode;
use crate::host_health::{
    HostCoordinatorLifecycle, HostHealthLevel, HostHealthReason, HostHealthSnapshot,
    SharedStorageFault,
};
use crate::node_metrics::NodeMetricsSnapshot;
use crate::storage_health::StorageHealthObservation;
use crate::time_health::{TimePersistenceHealthState, TimeStreamHealth};
use crate::time_wal::HostClockId;

pub const RUNTIME_STATUS_SCHEMA_VERSION: u16 = 1;
pub const DEFAULT_RUNTIME_STATUS_PATH: &str = "/run/sensor-system/host-v2-status.json";
const MAX_RUNTIME_STATUS_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeProcessPhase {
    Starting,
    Recording,
    Degraded,
    Stopping,
    Stopped,
    Faulted,
}

impl RuntimeProcessPhase {
    const fn label(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Recording => "recording",
            Self::Degraded => "degraded",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Faulted => "faulted",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeStatusIdentity {
    pub system_name: String,
    pub config_fingerprint: u64,
    pub process_id: u32,
    pub process_instance_id: String,
    pub host_boot_id: HostClockId,
    pub started_unix_ms: u64,
    pub started_monotonic_us: u64,
}

#[derive(Clone, Debug)]
pub struct HostRuntimeObservation {
    pub health: HostHealthSnapshot,
    pub channel_metrics: Vec<ChannelMetricsSnapshot>,
    pub node_metrics: Vec<NodeMetricsSnapshot>,
    pub nodes: Vec<DiscoveredNode>,
    pub time_streams: Vec<TimeStreamHealth>,
    pub storage_root: PathBuf,
    pub storage_minimum_available_bytes: u64,
    pub storage: Result<StorageHealthObservation, String>,
}

pub struct RuntimeStatusPublisher {
    path: PathBuf,
    temporary_path: PathBuf,
    identity: RuntimeStatusIdentity,
    next_sequence: u64,
}

impl RuntimeStatusPublisher {
    pub fn new(
        path: impl Into<PathBuf>,
        identity: RuntimeStatusIdentity,
    ) -> Result<Self, RuntimeStatusError> {
        let path = path.into();
        let parent = path.parent().ok_or(RuntimeStatusError::MissingParent)?;
        if parent.as_os_str().is_empty() {
            return Err(RuntimeStatusError::MissingParent);
        }
        fs::create_dir_all(parent).map_err(RuntimeStatusError::CreateDirectory)?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(RuntimeStatusError::InvalidFileName)?;
        let temporary_path =
            parent.join(format!(".{file_name}.{}.tmp", identity.process_instance_id));
        Ok(Self {
            path,
            temporary_path,
            identity,
            next_sequence: 1,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn publish(
        &mut self,
        phase: RuntimeProcessPhase,
        updated_monotonic_us: u64,
        observation: Option<&HostRuntimeObservation>,
        last_error: Option<&str>,
        clean_shutdown: Option<bool>,
    ) -> Result<(), RuntimeStatusError> {
        let sequence = self.next_sequence;
        let document = status_document(
            &self.identity,
            sequence,
            StatusPublication {
                phase,
                updated_unix_ms: unix_time_ms()?,
                updated_monotonic_us,
                observation,
                last_error,
                clean_shutdown,
            },
        );
        let mut bytes = serde_json::to_vec(&document).map_err(RuntimeStatusError::Serialize)?;
        bytes.push(b'\n');
        if bytes.len() > MAX_RUNTIME_STATUS_BYTES {
            return Err(RuntimeStatusError::DocumentTooLarge {
                actual: bytes.len(),
                maximum: MAX_RUNTIME_STATUS_BYTES,
            });
        }

        let write_result = self.write_atomically(&bytes);
        if write_result.is_err() {
            let _ = fs::remove_file(&self.temporary_path);
        }
        write_result?;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(RuntimeStatusError::SequenceOverflow)?;
        Ok(())
    }

    fn write_atomically(&self, bytes: &[u8]) -> Result<(), RuntimeStatusError> {
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        options.mode(0o640);
        let mut file = options
            .open(&self.temporary_path)
            .map_err(RuntimeStatusError::OpenTemporary)?;
        file.write_all(bytes)
            .map_err(RuntimeStatusError::WriteTemporary)?;
        file.flush().map_err(RuntimeStatusError::WriteTemporary)?;
        drop(file);
        fs::rename(&self.temporary_path, &self.path).map_err(RuntimeStatusError::Publish)
    }
}

struct StatusPublication<'a> {
    phase: RuntimeProcessPhase,
    updated_unix_ms: u64,
    updated_monotonic_us: u64,
    observation: Option<&'a HostRuntimeObservation>,
    last_error: Option<&'a str>,
    clean_shutdown: Option<bool>,
}

fn status_document(
    identity: &RuntimeStatusIdentity,
    sequence: u64,
    publication: StatusPublication<'_>,
) -> Value {
    let uptime_ms = publication
        .updated_monotonic_us
        .saturating_sub(identity.started_monotonic_us)
        / 1_000;
    let (health, channels, nodes, time_streams, storage) = match publication.observation {
        Some(observation) => (
            health_json(&observation.health),
            channels_json(observation),
            nodes_json(observation, publication.updated_monotonic_us),
            time_streams_json(&observation.time_streams),
            storage_json(observation),
        ),
        None => (Value::Null, Vec::new(), Vec::new(), Vec::new(), Value::Null),
    };
    json!({
        "schema_version": RUNTIME_STATUS_SCHEMA_VERSION,
        "sequence": sequence,
        "system_name": identity.system_name,
        "software": {
            "host_version": crate::HOST_SOFTWARE_VERSION,
            "framing_protocol": sensor_protocol::LOCAL_PROTOCOL_VERSION.framing,
            "application_protocol": sensor_protocol::LOCAL_PROTOCOL_VERSION.application
        },
        "config": {
            "fingerprint_algorithm": "fnv1a64",
            "fingerprint": format!("{:016x}", identity.config_fingerprint)
        },
        "process": {
            "pid": identity.process_id,
            "instance_id": identity.process_instance_id,
            "host_boot_id": identity.host_boot_id.to_string(),
            "phase": publication.phase.label(),
            "started_unix_ms": identity.started_unix_ms,
            "updated_unix_ms": publication.updated_unix_ms,
            "started_monotonic_us": identity.started_monotonic_us,
            "updated_monotonic_us": publication.updated_monotonic_us,
            "uptime_ms": uptime_ms,
            "clean_shutdown": publication.clean_shutdown
        },
        "health": health,
        "storage": storage,
        "channels": channels,
        "nodes": nodes,
        "time_streams": time_streams,
        "last_error": publication.last_error
    })
}

fn health_json(health: &HostHealthSnapshot) -> Value {
    json!({
        "level": host_health_label(health.level),
        "coordinator": coordinator_label(health.coordinator),
        "time_persistence": time_health_label(health.time),
        "reasons": health.reasons.iter().map(health_reason_json).collect::<Vec<_>>()
    })
}

fn channels_json(observation: &HostRuntimeObservation) -> Vec<Value> {
    observation
        .health
        .channels
        .iter()
        .map(|worker| {
            let metrics = observation
                .channel_metrics
                .iter()
                .find(|metrics| metrics.channel_id == worker.channel_id)
                .copied()
                .unwrap_or_default();
            let name = observation
                .nodes
                .iter()
                .find(|node| node.channel_id() == worker.channel_id)
                .map(DiscoveredNode::channel_name);
            json!({
                "channel_id": worker.channel_id,
                "channel_name": name,
                "phase": channel_phase_label(worker.phase),
                "completed_steps": worker.completed_steps,
                "metrics": channel_metrics_json(metrics)
            })
        })
        .collect()
}

fn nodes_json(observation: &HostRuntimeObservation, now_us: u64) -> Vec<Value> {
    observation
        .nodes
        .iter()
        .map(|node| {
            let identity = node.identity();
            let metrics = observation.node_metrics.iter().find(|metrics| {
                metrics.channel_id == node.channel_id()
                    && metrics.node_address == node.node_address()
            });
            let output_rate = metrics
                .and_then(|metrics| metrics.output_rate_code)
                .and_then(sensor_protocol::OutputRateCode::from_wire)
                .unwrap_or_else(|| node.output_rate());
            let telemetry = metrics.and_then(|metrics| metrics.latest_telemetry);
            let temperature_c = telemetry.map(|telemetry| {
                sensor_protocol::Adxl355TemperatureCode::new(
                    telemetry.adxl355_temperature_raw,
                )
                .expect("node metrics only accepts validated telemetry")
                .celsius()
            });
            json!({
                "channel_id": node.channel_id(),
                "channel_name": node.channel_name(),
                "node_address": node.node_address(),
                "hardware_id": hardware_id_hex(identity.hardware_id.bytes()),
                "boot_epoch": metrics
                    .and_then(|metrics| metrics.boot_epoch)
                    .unwrap_or_else(|| identity.boot_epoch.get()),
                "board_revision": identity.board_revision,
                "firmware": format!("{}.{}.{}", identity.firmware.major, identity.firmware.minor, identity.firmware.patch),
                "framing_protocol": identity.protocol.framing,
                "application_protocol": identity.protocol.application,
                "output_rate_hz": output_rate.hertz_label(),
                "output_rate_code": output_rate as u8,
                "configuration_revision": metrics.and_then(|metrics| metrics.configuration_revision),
                "output_rate_mask": node.capabilities().output_rate_mask,
                "service_flags": node.capabilities().service_flags,
                "feature_flags": node.capabilities().feature_flags,
                "cursors": metrics.map(|metrics| json!({
                    "received": metrics.received_cursor,
                    "durable": metrics.durable_cursor,
                    "released": metrics.released_cursor,
                    "produced": metrics.produced_cursor
                })),
                "last_measurement_monotonic_us": metrics.and_then(|metrics| metrics.last_measurement_monotonic_us),
                "last_measurement_age_ms": metrics.and_then(|metrics| age_ms(now_us, metrics.last_measurement_monotonic_us)),
                "last_time_sync_monotonic_us": metrics.and_then(|metrics| metrics.last_time_sync_monotonic_us),
                "last_time_sync_age_ms": metrics.and_then(|metrics| age_ms(now_us, metrics.last_time_sync_monotonic_us)),
                "last_telemetry_monotonic_us": telemetry.map(|telemetry| telemetry.observed_monotonic_us),
                "last_telemetry_age_ms": telemetry.and_then(|telemetry| age_ms(now_us, Some(telemetry.observed_monotonic_us))),
                "telemetry_device_time_us": telemetry.map(|telemetry| telemetry.captured_device_time_us),
                "temperature_raw": telemetry.map(|telemetry| telemetry.adxl355_temperature_raw),
                "temperature_c": temperature_c,
                "persisted_sample_count": metrics.map(|metrics| metrics.persisted_sample_count),
                "gap_count": metrics.map(|metrics| metrics.gap_count),
                "missing_sample_count": metrics.map(|metrics| metrics.missing_sample_count),
                "continuity_break_count": metrics.map(|metrics| metrics.continuity_break_count),
                "availability": metrics.map(|metrics| node_availability_label(metrics.availability)).unwrap_or("unknown"),
                "recovery_reason": metrics.and_then(|metrics| node_recovery_reason(metrics.availability)),
                "availability_failures": metrics.map(|metrics| metrics.availability.failures())
            })
        })
        .collect()
}

fn node_availability_label(availability: crate::channel_runtime::NodeAvailability) -> &'static str {
    use crate::channel_runtime::NodeAvailability;
    match availability {
        NodeAvailability::Online => "online",
        NodeAvailability::RetryBackoff { .. } => "degraded",
        NodeAvailability::Recovering { .. } => "recovering",
        NodeAvailability::Offline { .. } => "offline",
    }
}

fn node_recovery_reason(
    availability: crate::channel_runtime::NodeAvailability,
) -> Option<&'static str> {
    use crate::channel_runtime::{NodeAvailability, OfflineCause, RecoveryCause};
    let cause = match availability {
        NodeAvailability::Recovering { cause, .. }
        | NodeAvailability::Offline {
            cause: OfflineCause::Recovery(cause),
            ..
        } => cause,
        _ => return None,
    };
    Some(match cause {
        RecoveryCause::StreamChanged => "stream_changed",
        RecoveryCause::InvalidRelease => "invalid_release",
        RecoveryCause::InvalidResponse => "invalid_response",
        RecoveryCause::StreamRegressed => "stream_regressed",
    })
}

fn channel_metrics_json(metrics: ChannelMetricsSnapshot) -> Value {
    json!({
        "transmissions": metrics.transmissions,
        "decode_errors": metrics.decode_errors,
        "disconnects": metrics.disconnects,
        "transaction_timeouts": metrics.transaction_timeouts,
        "wire_attempt_failures": metrics.wire_attempt_failures,
        "late_responses": metrics.late_responses,
        "unrelated_responses": metrics.unrelated_responses,
        "unsolicited_responses": metrics.unsolicited_responses,
        "measurement_persisted": metrics.measurement_persisted,
        "measurement_no_data": metrics.measurement_no_data,
        "measurement_retries": metrics.measurement_retries,
        "measurement_recoveries": metrics.measurement_recoveries,
        "persistence_submitted": metrics.persistence_submitted,
        "persistence_backpressure": metrics.persistence_backpressure,
        "time_sync_submitted": metrics.time_sync_submitted,
        "time_sync_retries": metrics.time_sync_retries,
        "time_sync_backpressure": metrics.time_sync_backpressure,
        "telemetry_snapshots": metrics.telemetry_snapshots,
        "telemetry_retries": metrics.telemetry_retries,
        "telemetry_backpressure": metrics.telemetry_backpressure,
        "maximum_step_us": metrics.maximum_step_us,
        "storage_committed_groups": metrics.storage_committed_groups,
        "storage_committed_records": metrics.storage_committed_records,
        "storage_failed_groups": metrics.storage_failed_groups,
        "storage_maximum_persist_us": metrics.storage_maximum_persist_us,
        "storage_maximum_group_records": metrics.storage_maximum_group_records,
        "storage_queue_high_water": metrics.storage_queue_high_water
    })
}

fn time_streams_json(streams: &[TimeStreamHealth]) -> Vec<Value> {
    streams
        .iter()
        .map(|stream| {
            json!({
                "hardware_id": hardware_id_hex(stream.stream.hardware_id.bytes()),
                "node_address": stream.stream.node_address,
                "boot_epoch": stream.stream.boot_epoch.get(),
                "durable_record_count": stream.durable_record_count
            })
        })
        .collect()
}

fn storage_json(observation: &HostRuntimeObservation) -> Value {
    let root = observation.storage_root.display().to_string();
    match &observation.storage {
        Ok(StorageHealthObservation::Healthy(storage)) => json!({
            "state": "healthy",
            "root": root,
            "available_bytes": storage.available_bytes,
            "minimum_available_bytes": observation.storage_minimum_available_bytes
        }),
        Ok(StorageHealthObservation::Fault(fault)) => json!({
            "state": "faulted",
            "root": root,
            "minimum_available_bytes": observation.storage_minimum_available_bytes,
            "fault": storage_fault_json(*fault)
        }),
        Err(error) => json!({
            "state": "probe_failed",
            "root": root,
            "minimum_available_bytes": observation.storage_minimum_available_bytes,
            "error": error
        }),
    }
}

fn health_reason_json(reason: &HostHealthReason) -> Value {
    match reason {
        HostHealthReason::ChannelFaulted { channel_id } => {
            json!({"code":"channel_faulted", "channel_id":channel_id})
        }
        HostHealthReason::ChannelTerminated { channel_id } => {
            json!({"code":"channel_terminated", "channel_id":channel_id})
        }
        HostHealthReason::TimeRecoveryRequired => json!({"code":"time_recovery_required"}),
        HostHealthReason::TimeRecovering => json!({"code":"time_recovering"}),
        HostHealthReason::TimeIntegrityFault => json!({"code":"time_integrity_fault"}),
        HostHealthReason::SharedStorageFault(fault) => {
            json!({"code":"shared_storage_fault", "detail":storage_fault_json(*fault)})
        }
    }
}

fn storage_fault_json(fault: SharedStorageFault) -> Value {
    match fault {
        SharedStorageFault::ReserveBreached {
            available_bytes,
            required_bytes,
        } => json!({
            "code":"reserve_breached",
            "available_bytes":available_bytes,
            "required_bytes":required_bytes
        }),
        SharedStorageFault::ReadOnly => json!({"code":"read_only"}),
        SharedStorageFault::ProbeFailed => json!({"code":"probe_failed"}),
    }
}

const fn host_health_label(value: HostHealthLevel) -> &'static str {
    match value {
        HostHealthLevel::Healthy => "healthy",
        HostHealthLevel::Degraded => "degraded",
        HostHealthLevel::StopRequired => "stop_required",
    }
}

const fn coordinator_label(value: HostCoordinatorLifecycle) -> &'static str {
    match value {
        HostCoordinatorLifecycle::Running => "running",
        HostCoordinatorLifecycle::StopRequested => "stop_requested",
    }
}

const fn time_health_label(value: TimePersistenceHealthState) -> &'static str {
    match value {
        TimePersistenceHealthState::Healthy => "healthy",
        TimePersistenceHealthState::RecoveryRequired => "recovery_required",
        TimePersistenceHealthState::Recovering => "recovering",
        TimePersistenceHealthState::IntegrityFault => "integrity_fault",
    }
}

const fn channel_phase_label(value: ChannelWorkerPhase) -> &'static str {
    match value {
        ChannelWorkerPhase::Starting => "starting",
        ChannelWorkerPhase::Running => "running",
        ChannelWorkerPhase::StopRequested => "stop_requested",
        ChannelWorkerPhase::Faulted => "faulted",
        ChannelWorkerPhase::Stopped => "stopped",
    }
}

fn age_ms(now_us: u64, value: Option<u64>) -> Option<u64> {
    value.map(|value| now_us.saturating_sub(value) / 1_000)
}

fn hardware_id_hex(bytes: [u8; 8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn unix_time_ms() -> Result<u64, RuntimeStatusError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RuntimeStatusError::SystemTimeBeforeEpoch)?
        .as_millis();
    u64::try_from(millis).map_err(|_| RuntimeStatusError::TimeOverflow)
}

#[derive(Debug)]
pub enum RuntimeStatusError {
    MissingParent,
    InvalidFileName,
    CreateDirectory(io::Error),
    Serialize(serde_json::Error),
    DocumentTooLarge { actual: usize, maximum: usize },
    OpenTemporary(io::Error),
    WriteTemporary(io::Error),
    Publish(io::Error),
    SequenceOverflow,
    SystemTimeBeforeEpoch,
    TimeOverflow,
}

impl fmt::Display for RuntimeStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "runtime status error: {self:?}")
    }
}

impl std::error::Error for RuntimeStatusError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_supervisor::ChannelWorkerSnapshot;
    use std::fs;

    fn identity() -> RuntimeStatusIdentity {
        RuntimeStatusIdentity {
            system_name: "test-host".to_owned(),
            config_fingerprint: 0x1234,
            process_id: 42,
            process_instance_id: "instance-1".to_owned(),
            host_boot_id: "00112233-4455-6677-8899-aabbccddeeff".parse().unwrap(),
            started_unix_ms: 1_000,
            started_monotonic_us: 2_000,
        }
    }

    #[test]
    fn atomically_replaces_a_complete_versioned_document() {
        let root = std::env::temp_dir().join(format!(
            "sensor-status-{}-{}",
            std::process::id(),
            unix_time_ms().unwrap()
        ));
        let path = root.join("status.json");
        let mut publisher = RuntimeStatusPublisher::new(&path, identity()).unwrap();
        publisher
            .publish(RuntimeProcessPhase::Starting, 2_500, None, None, None)
            .unwrap();
        let first: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(first["schema_version"], 1);
        assert_eq!(first["sequence"], 1);
        assert_eq!(first["process"]["phase"], "starting");
        assert_eq!(first["process"]["uptime_ms"], 0);

        publisher
            .publish(
                RuntimeProcessPhase::Faulted,
                5_500,
                None,
                Some("test fault"),
                Some(false),
            )
            .unwrap();
        let second: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(second["sequence"], 2);
        assert_eq!(second["process"]["phase"], "faulted");
        assert_eq!(second["process"]["uptime_ms"], 3);
        assert_eq!(second["last_error"], "test fault");
        assert!(!publisher.temporary_path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn health_labels_are_stable_contract_values() {
        let worker = ChannelWorkerSnapshot {
            channel_id: 1,
            phase: ChannelWorkerPhase::Running,
            completed_steps: 9,
        };
        let health = HostHealthSnapshot {
            level: HostHealthLevel::Degraded,
            coordinator: HostCoordinatorLifecycle::Running,
            time: TimePersistenceHealthState::Healthy,
            channels: vec![worker],
            reasons: vec![HostHealthReason::ChannelFaulted { channel_id: 1 }],
        };
        let value = health_json(&health);
        assert_eq!(value["level"], "degraded");
        assert_eq!(value["reasons"][0]["code"], "channel_faulted");
    }

    #[test]
    fn node_availability_labels_preserve_recovery_cause() {
        use crate::channel_runtime::{NodeAvailability, OfflineCause, RecoveryCause};

        let recovering = NodeAvailability::Recovering {
            cause: RecoveryCause::InvalidResponse,
            failures: 2,
        };
        assert_eq!(node_availability_label(recovering), "recovering");
        assert_eq!(node_recovery_reason(recovering), Some("invalid_response"));
        assert_eq!(recovering.failures(), 2);

        let offline = NodeAvailability::Offline {
            cause: OfflineCause::Recovery(RecoveryCause::StreamChanged),
            failures: 4,
        };
        assert_eq!(node_availability_label(offline), "offline");
        assert_eq!(node_recovery_reason(offline), Some("stream_changed"));
        assert_eq!(offline.failures(), 4);
    }
}
