use core::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::operations::{
    OperationKind, OperationPhase, OperationRecord, OperationResult, OperationTarget,
    OperationsAuditSink, OperationsAuditWriteError,
};

const MAGIC: [u8; 4] = *b"OPA1";
const SCHEMA_VERSION: u8 = 1;
const HEADER_BYTES: usize = 16;
const MAX_PAYLOAD_BYTES: usize = 8192;

/// Per-channel append-only audit. Each phase transition is synced before the
/// controller may continue. Incomplete final records are truncated on open;
/// corruption of a complete record fails closed.
pub struct OperationsAuditWal {
    file: File,
    path: PathBuf,
    channel_id: u8,
    next_sequence: u64,
}

impl OperationsAuditWal {
    pub fn open(root: &Path, channel_id: u8) -> Result<Self, OperationsAuditError> {
        if channel_id == 0 {
            return Err(OperationsAuditError::InvalidChannel);
        }
        let directory = root.join(".operations-audit-v1");
        fs::create_dir_all(&directory).map_err(OperationsAuditError::Io)?;
        let path = directory.join(format!("channel-{channel_id:02}.wal"));
        let existed = path.exists();
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .map_err(OperationsAuditError::Io)?;
        if !existed {
            File::open(&directory)
                .and_then(|value| value.sync_all())
                .map_err(OperationsAuditError::Io)?;
        }
        let next_sequence = recover_and_count(&mut file, channel_id)?;
        Ok(Self {
            file,
            path,
            channel_id,
            next_sequence,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub const fn event_count(&self) -> u64 {
        self.next_sequence
    }

    fn append_record(&mut self, record: &OperationRecord) -> Result<(), OperationsAuditError> {
        if record.request.target.channel_id() != self.channel_id {
            return Err(OperationsAuditError::WrongChannel);
        }
        let payload = event_json(self.next_sequence, record);
        let bytes = serde_json::to_vec(&payload).map_err(OperationsAuditError::Serialize)?;
        if bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(OperationsAuditError::PayloadTooLarge(bytes.len()));
        }
        let mut header = [0_u8; HEADER_BYTES];
        header[..4].copy_from_slice(&MAGIC);
        header[4] = SCHEMA_VERSION;
        header[8..12].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
        header[12..16].copy_from_slice(&sensor_protocol::crc32c(&bytes).to_le_bytes());
        self.file
            .write_all(&header)
            .map_err(OperationsAuditError::Io)?;
        self.file
            .write_all(&bytes)
            .map_err(OperationsAuditError::Io)?;
        self.file.sync_data().map_err(OperationsAuditError::Io)?;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(OperationsAuditError::SequenceOverflow)?;
        Ok(())
    }
}

impl OperationsAuditSink for OperationsAuditWal {
    fn append(&mut self, record: &OperationRecord) -> Result<(), OperationsAuditWriteError> {
        self.append_record(record)
            .map_err(|_| OperationsAuditWriteError)
    }
}

fn recover_and_count(file: &mut File, channel_id: u8) -> Result<u64, OperationsAuditError> {
    file.seek(SeekFrom::Start(0))
        .map_err(OperationsAuditError::Io)?;
    let length = file.metadata().map_err(OperationsAuditError::Io)?.len();
    let mut offset = 0_u64;
    let mut count = 0_u64;
    while offset < length {
        let remaining = length - offset;
        if remaining < HEADER_BYTES as u64 {
            truncate_tail(file, offset)?;
            break;
        }
        let mut header = [0_u8; HEADER_BYTES];
        file.read_exact(&mut header)
            .map_err(OperationsAuditError::Io)?;
        if header[..4] != MAGIC || header[4] != SCHEMA_VERSION || header[5..8] != [0; 3] {
            return Err(OperationsAuditError::InvalidRecord);
        }
        let payload_len = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
        if payload_len == 0 || payload_len > MAX_PAYLOAD_BYTES {
            return Err(OperationsAuditError::InvalidRecord);
        }
        let record_len = HEADER_BYTES as u64 + payload_len as u64;
        if remaining < record_len {
            truncate_tail(file, offset)?;
            break;
        }
        let mut payload = vec![0_u8; payload_len];
        file.read_exact(&mut payload)
            .map_err(OperationsAuditError::Io)?;
        let expected = u32::from_le_bytes(header[12..16].try_into().unwrap());
        if sensor_protocol::crc32c(&payload) != expected {
            return Err(OperationsAuditError::ChecksumMismatch);
        }
        let value: Value =
            serde_json::from_slice(&payload).map_err(OperationsAuditError::Deserialize)?;
        if value.get("schema_version").and_then(Value::as_u64) != Some(1)
            || value.get("channel_id").and_then(Value::as_u64) != Some(u64::from(channel_id))
            || value.get("event_sequence").and_then(Value::as_u64) != Some(count)
        {
            return Err(OperationsAuditError::InvalidRecord);
        }
        offset += record_len;
        count = count
            .checked_add(1)
            .ok_or(OperationsAuditError::SequenceOverflow)?;
    }
    file.seek(SeekFrom::End(0))
        .map_err(OperationsAuditError::Io)?;
    Ok(count)
}

fn truncate_tail(file: &mut File, offset: u64) -> Result<(), OperationsAuditError> {
    file.set_len(offset).map_err(OperationsAuditError::Io)?;
    file.sync_data().map_err(OperationsAuditError::Io)
}

fn event_json(sequence: u64, record: &OperationRecord) -> Value {
    let (channel_id, node_address) = match record.request.target {
        OperationTarget::Channel { channel_id } => (channel_id, None),
        OperationTarget::Node {
            channel_id,
            node_address,
        } => (channel_id, Some(node_address)),
    };
    json!({
        "schema_version": 1,
        "event_sequence": sequence,
        "unix_time_ms": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
        "channel_id": channel_id,
        "node_address": node_address,
        "operation_id": record.request.operation_id.get(),
        "operation": operation_json(&record.request.kind),
        "phase": phase_label(record.phase),
        "result": record.result.as_ref().map(result_json),
        "error": record.error.map(|value| value.to_string())
    })
}

fn operation_json(kind: &OperationKind) -> Value {
    match kind {
        OperationKind::ReadStatus => json!({"kind": "read_status"}),
        OperationKind::EnterMaintenance => json!({"kind": "enter_maintenance"}),
        OperationKind::ResumeRecording => json!({"kind": "resume_recording"}),
        OperationKind::GetConfiguration => json!({"kind": "get_configuration"}),
        OperationKind::ValidateConfiguration { configuration } => {
            json!({"kind": "validate_configuration", "configuration": configuration_json(*configuration)})
        }
        OperationKind::ApplyConfiguration {
            expected_revision,
            configuration,
        } => {
            json!({"kind": "apply_configuration", "expected_revision": expected_revision, "configuration": configuration_json(*configuration)})
        }
        OperationKind::ResetConfiguration { expected_revision } => {
            json!({"kind": "reset_configuration", "expected_revision": expected_revision})
        }
        OperationKind::SetTelemetryInterval { interval_seconds } => {
            json!({"kind": "set_telemetry_interval", "telemetry_interval_seconds": interval_seconds})
        }
        OperationKind::RestartNode => json!({"kind": "restart_node"}),
        OperationKind::ScanUnassigned {
            round_nonce,
            slot_count,
        } => {
            json!({"kind": "scan_unassigned", "round_nonce": round_nonce, "slot_count": slot_count})
        }
        OperationKind::AssignAddress {
            round_nonce,
            hardware_id,
            node_address,
        } => {
            json!({"kind": "assign_address", "round_nonce": round_nonce, "hardware_id": hardware_id_hex(*hardware_id), "new_node_address": node_address})
        }
    }
}

fn result_json(result: &OperationResult) -> Value {
    match result {
        OperationResult::ConfigurationObserved(value) => {
            json!({"kind": "configuration_observed", "revision": value.revision})
        }
        OperationResult::ConfigurationValidated(value) => {
            json!({"kind": "configuration_validated", "code": value.code as u8, "field": value.field as u8})
        }
        OperationResult::ConfigurationApplied(value) => {
            json!({"kind": "configuration_applied", "wire_operation_id": value.operation_id, "code": value.code as u8, "revision": value.observed.revision, "effective_sample_seq": value.effective_sample_seq})
        }
        OperationResult::CommissioningScan {
            round_nonce,
            slot_count,
            discovered,
        } => {
            json!({"kind": "commissioning_scan", "round_nonce": round_nonce, "slot_count": slot_count, "hardware_ids": discovered.iter().map(|value| hardware_id_hex(value.hardware_id)).collect::<Vec<_>>() })
        }
        OperationResult::AddressAssigned(value) => {
            json!({"kind": "address_assigned", "wire_operation_id": value.operation_id, "hardware_id": hardware_id_hex(value.hardware_id), "node_address": value.node_address})
        }
        OperationResult::NodeRestartRequested { node_address } => {
            json!({"kind": "node_restart_requested", "node_address": node_address})
        }
        OperationResult::TelemetryIntervalApplied {
            node_address,
            interval_seconds,
        } => {
            json!({"kind": "telemetry_interval_applied", "node_address": node_address, "telemetry_interval_seconds": interval_seconds})
        }
    }
}

fn configuration_json(value: sensor_protocol::MeasurementConfiguration) -> Value {
    json!({"output_rate_code": value.output_rate as u8, "range_g": value.range_g, "high_pass_corner": value.high_pass_corner, "filter_profile": value.filter_profile as u8, "fifo_watermark": value.fifo_watermark, "offset_x": value.offset_x, "offset_y": value.offset_y, "offset_z": value.offset_z})
}

fn phase_label(phase: OperationPhase) -> &'static str {
    match phase {
        OperationPhase::Accepted => "accepted",
        OperationPhase::Running => "running",
        OperationPhase::Succeeded => "succeeded",
        OperationPhase::Rejected => "rejected",
        OperationPhase::Failed => "failed",
    }
}

fn hardware_id_hex(value: sensor_core::HardwareId) -> String {
    use core::fmt::Write as _;
    let mut output = String::with_capacity(16);
    for byte in value.bytes() {
        write!(&mut output, "{byte:02x}").unwrap();
    }
    output
}

#[derive(Debug)]
pub enum OperationsAuditError {
    InvalidChannel,
    WrongChannel,
    InvalidRecord,
    ChecksumMismatch,
    SequenceOverflow,
    PayloadTooLarge(usize),
    Io(io::Error),
    Serialize(serde_json::Error),
    Deserialize(serde_json::Error),
}

impl fmt::Display for OperationsAuditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "operations audit error: {self:?}")
    }
}
impl std::error::Error for OperationsAuditError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::{OperationId, OperationRequest};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "operations-audit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }
    fn record(phase: OperationPhase) -> OperationRecord {
        OperationRecord {
            request: OperationRequest {
                operation_id: OperationId::new(7).unwrap(),
                target: OperationTarget::Channel { channel_id: 2 },
                kind: OperationKind::EnterMaintenance,
            },
            phase,
            result: None,
            error: None,
        }
    }

    #[test]
    fn durable_transitions_reopen_with_monotonic_sequence() {
        let root = root();
        let mut wal = OperationsAuditWal::open(&root, 2).unwrap();
        wal.append_record(&record(OperationPhase::Accepted))
            .unwrap();
        wal.append_record(&record(OperationPhase::Running)).unwrap();
        assert_eq!(OperationsAuditWal::open(&root, 2).unwrap().event_count(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_truncates_only_an_incomplete_tail() {
        let root = root();
        let mut wal = OperationsAuditWal::open(&root, 2).unwrap();
        wal.append_record(&record(OperationPhase::Accepted))
            .unwrap();
        let path = wal.path().to_owned();
        drop(wal);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"OPA")
            .unwrap();
        let recovered = OperationsAuditWal::open(&root, 2).unwrap();
        assert_eq!(recovered.event_count(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
