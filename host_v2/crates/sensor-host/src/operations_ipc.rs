use core::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sensor_protocol::{DecimationFilterProfile, MeasurementConfiguration, OutputRateCode};
use serde_json::{json, Value};

use crate::operations::{
    ChannelOperatingMode, ChannelOperationsHandle, CommandSubmitError, OperationId, OperationKind,
    OperationPhase, OperationRequest, OperationResult, OperationTarget, OperationsError,
};

pub const OPERATIONS_IPC_SCHEMA_VERSION: u16 = 1;
pub const DEFAULT_OPERATIONS_SOCKET_PATH: &str = "/run/sensor-system/operations-v1.sock";
pub const MAX_OPERATIONS_REQUEST_BYTES: usize = 4 * 1024;
pub const MAX_OPERATIONS_RESPONSE_BYTES: usize = 4 * 1024;
pub const DEFAULT_REQUESTS_PER_POLL: usize = 4;

/// Bounded, non-blocking local adapter. It never owns channel state or RS485.
pub struct OperationsIpcServer {
    socket: UnixDatagram,
    path: PathBuf,
    handles: Vec<ChannelOperationsHandle>,
    requests_per_poll: usize,
}

impl OperationsIpcServer {
    pub fn bind(
        path: impl Into<PathBuf>,
        handles: Vec<ChannelOperationsHandle>,
    ) -> Result<Self, OperationsIpcError> {
        Self::bind_with_budget(path, handles, DEFAULT_REQUESTS_PER_POLL)
    }

    pub fn bind_with_budget(
        path: impl Into<PathBuf>,
        handles: Vec<ChannelOperationsHandle>,
        requests_per_poll: usize,
    ) -> Result<Self, OperationsIpcError> {
        if requests_per_poll == 0 {
            return Err(OperationsIpcError::ZeroPollBudget);
        }
        let path = path.into();
        let parent = path.parent().ok_or(OperationsIpcError::MissingParent)?;
        if parent.as_os_str().is_empty() {
            return Err(OperationsIpcError::MissingParent);
        }
        fs::create_dir_all(parent).map_err(OperationsIpcError::CreateDirectory)?;
        remove_stale_socket(&path)?;
        let socket = UnixDatagram::bind(&path).map_err(OperationsIpcError::Bind)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o660))
            .map_err(OperationsIpcError::ConfigurePermissions)?;
        socket
            .set_nonblocking(true)
            .map_err(OperationsIpcError::Configure)?;
        Ok(Self {
            socket,
            path,
            handles,
            requests_per_poll,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Handles at most the configured request budget and never waits for a client.
    pub fn poll(&self) -> Result<usize, OperationsIpcError> {
        let mut handled = 0;
        let mut buffer = [0_u8; MAX_OPERATIONS_REQUEST_BYTES + 1];
        while handled < self.requests_per_poll {
            let (size, peer) = match self.socket.recv_from(&mut buffer) {
                Ok(value) => value,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(OperationsIpcError::Receive(error)),
            };
            handled += 1;
            let response = if size > MAX_OPERATIONS_REQUEST_BYTES {
                error_response("request_too_large", "operations request exceeds 4096 bytes")
            } else {
                match serde_json::from_slice::<Value>(&buffer[..size]) {
                    Ok(request) => self.dispatch(&request),
                    Err(error) => error_response("invalid_json", &error.to_string()),
                }
            };
            let Some(peer_path) = peer.as_pathname() else {
                continue;
            };
            let bytes = serde_json::to_vec(&response).map_err(OperationsIpcError::Serialize)?;
            if bytes.len() > MAX_OPERATIONS_RESPONSE_BYTES {
                return Err(OperationsIpcError::ResponseTooLarge(bytes.len()));
            }
            match self.socket.send_to(&bytes, peer_path) {
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::NotFound
                            | io::ErrorKind::ConnectionRefused
                    ) => {}
                Err(error) => return Err(OperationsIpcError::Send(error)),
            }
        }
        Ok(handled)
    }

    fn dispatch(&self, request: &Value) -> Value {
        let Some(object) = request.as_object() else {
            return error_response("invalid_request", "request must be a JSON object");
        };
        if object.get("schema_version").and_then(Value::as_u64)
            != Some(u64::from(OPERATIONS_IPC_SCHEMA_VERSION))
        {
            return error_response("unsupported_schema", "expected operations schema version 1");
        }
        match object.get("action").and_then(Value::as_str) {
            Some("ping") => json!({
                "schema_version": OPERATIONS_IPC_SCHEMA_VERSION,
                "ok": true,
                "state": "ready"
            }),
            Some("status") => self.status_response(object),
            Some("submit") => self.submit_response(object),
            Some("get") => self.get_response(object),
            Some(_) => error_response("unknown_action", "unknown operations action"),
            None => error_response("invalid_request", "action is required"),
        }
    }

    fn status_response(&self, object: &serde_json::Map<String, Value>) -> Value {
        let channel_id = match u8_field(object, "channel_id") {
            Ok(value) => value,
            Err(response) => return response,
        };
        let Some(handle) = self.handle(channel_id) else {
            return error_response("unknown_channel", "channel is not active in this process");
        };
        match handle.snapshot() {
            Ok(snapshot) => json!({
                "schema_version": OPERATIONS_IPC_SCHEMA_VERSION,
                "ok": true,
                "channel_id": channel_id,
                "mode": mode_label(snapshot.mode),
                "active_operation_id": snapshot.active_operation.map(OperationId::get)
            }),
            Err(error) => operations_error_response(error),
        }
    }

    fn submit_response(&self, object: &serde_json::Map<String, Value>) -> Value {
        let channel_id = match u8_field(object, "channel_id") {
            Ok(value) => value,
            Err(response) => return response,
        };
        let operation_id = match operation_id_field(object) {
            Ok(value) => value,
            Err(response) => return response,
        };
        let operation_name = object.get("operation").and_then(Value::as_str);
        let kind = match operation_name {
            Some("enter_maintenance") => OperationKind::EnterMaintenance,
            Some("resume_recording") => OperationKind::ResumeRecording,
            Some("get_configuration") => OperationKind::GetConfiguration,
            Some("validate_configuration") => {
                let configuration = match configuration_field(object) {
                    Ok(value) => value,
                    Err(response) => return response,
                };
                OperationKind::ValidateConfiguration { configuration }
            }
            Some("apply_configuration") => {
                let configuration = match configuration_field(object) {
                    Ok(value) => value,
                    Err(response) => return response,
                };
                let expected_revision = match u32_field(object, "expected_revision") {
                    Ok(value) if value != 0 => value,
                    Ok(_) => {
                        return error_response(
                            "invalid_request",
                            "expected_revision must be non-zero",
                        )
                    }
                    Err(response) => return response,
                };
                OperationKind::ApplyConfiguration {
                    expected_revision,
                    configuration,
                }
            }
            Some("reset_configuration") => {
                let expected_revision = match u32_field(object, "expected_revision") {
                    Ok(value) if value != 0 => value,
                    Ok(_) => {
                        return error_response(
                            "invalid_request",
                            "expected_revision must be non-zero",
                        )
                    }
                    Err(response) => return response,
                };
                OperationKind::ResetConfiguration { expected_revision }
            }
            Some("set_telemetry_interval") => {
                let interval_seconds = match u32_field(object, "telemetry_interval_seconds") {
                    Ok(value) => value,
                    Err(response) => return response,
                };
                OperationKind::SetTelemetryInterval { interval_seconds }
            }
            Some("restart_node") => OperationKind::RestartNode,
            Some("scan_unassigned") => OperationKind::ScanUnassigned {
                round_nonce: match u64_field(object, "round_nonce") {
                    Ok(value) => value,
                    Err(response) => return response,
                },
                slot_count: match u16_field(object, "slot_count") {
                    Ok(value) => value,
                    Err(response) => return response,
                },
            },
            Some("assign_address") => OperationKind::AssignAddress {
                round_nonce: match u64_field(object, "round_nonce") {
                    Ok(value) => value,
                    Err(response) => return response,
                },
                hardware_id: match hardware_id_field(object, "hardware_id") {
                    Ok(value) => value,
                    Err(response) => return response,
                },
                node_address: match u8_field(object, "new_node_address") {
                    Ok(value) => value,
                    Err(response) => return response,
                },
            },
            Some(_) => {
                return error_response("unknown_operation", "operation is not exposed by IPC v1")
            }
            None => return error_response("invalid_request", "operation is required"),
        };
        let Some(handle) = self.handle(channel_id) else {
            return error_response("unknown_channel", "channel is not active in this process");
        };
        let target = if matches!(
            kind,
            OperationKind::GetConfiguration
                | OperationKind::ValidateConfiguration { .. }
                | OperationKind::ApplyConfiguration { .. }
                | OperationKind::ResetConfiguration { .. }
                | OperationKind::SetTelemetryInterval { .. }
                | OperationKind::RestartNode
        ) {
            let node_address = match u8_field(object, "node_address") {
                Ok(value) => value,
                Err(response) => return response,
            };
            OperationTarget::Node {
                channel_id,
                node_address,
            }
        } else {
            OperationTarget::Channel { channel_id }
        };
        let request = OperationRequest {
            operation_id,
            target,
            kind,
        };
        // Resolve a retry from the worker-owned ledger before touching the
        // mailbox. This also lets a client recover when the first response was
        // lost while the bounded queue is currently full.
        match handle.snapshot() {
            Ok(snapshot) => {
                if let Some(record) = snapshot
                    .records
                    .iter()
                    .find(|record| record.request.operation_id == operation_id)
                {
                    if record.request != request {
                        return operations_error_response(OperationsError::OperationIdConflict(
                            operation_id,
                        ));
                    }
                    return operation_response(
                        channel_id,
                        operation_id,
                        phase_label(record.phase),
                        Some(snapshot.mode),
                        record.result.clone(),
                        record.error,
                    );
                }
            }
            Err(error) => return operations_error_response(error),
        }
        match handle.try_submit(request) {
            Ok(()) => json!({
                "schema_version": OPERATIONS_IPC_SCHEMA_VERSION,
                "ok": true,
                "channel_id": channel_id,
                "operation_id": operation_id.get(),
                "phase": "queued"
            }),
            Err(CommandSubmitError::QueueFull) => {
                error_response("queue_full", "channel operations mailbox is full")
            }
            Err(CommandSubmitError::Disconnected) => error_response(
                "channel_disconnected",
                "channel operations path is disconnected",
            ),
            Err(CommandSubmitError::Invalid(error)) => operations_error_response(error),
        }
    }

    fn get_response(&self, object: &serde_json::Map<String, Value>) -> Value {
        let channel_id = match u8_field(object, "channel_id") {
            Ok(value) => value,
            Err(response) => return response,
        };
        let operation_id = match operation_id_field(object) {
            Ok(value) => value,
            Err(response) => return response,
        };
        let Some(handle) = self.handle(channel_id) else {
            return error_response("unknown_channel", "channel is not active in this process");
        };
        let snapshot = match handle.snapshot() {
            Ok(value) => value,
            Err(error) => return operations_error_response(error),
        };
        let Some(record) = snapshot
            .records
            .iter()
            .find(|record| record.request.operation_id == operation_id)
        else {
            return json!({
                "schema_version": OPERATIONS_IPC_SCHEMA_VERSION,
                "ok": true,
                "channel_id": channel_id,
                "operation_id": operation_id.get(),
                "phase": "queued",
                "mode": mode_label(snapshot.mode)
            });
        };
        operation_response(
            channel_id,
            operation_id,
            phase_label(record.phase),
            Some(snapshot.mode),
            record.result.clone(),
            record.error,
        )
    }

    fn handle(&self, channel_id: u8) -> Option<&ChannelOperationsHandle> {
        self.handles
            .iter()
            .find(|handle| handle.channel_id() == channel_id)
    }
}

impl Drop for OperationsIpcServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn remove_stale_socket(path: &Path) -> Result<(), OperationsIpcError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(OperationsIpcError::Inspect(error)),
    };
    if !metadata.file_type().is_socket() {
        return Err(OperationsIpcError::PathOccupied);
    }
    let probe = UnixDatagram::unbound().map_err(OperationsIpcError::Probe)?;
    match probe.send_to(b"{\"schema_version\":1,\"action\":\"ping\"}", path) {
        Ok(_) => Err(OperationsIpcError::AlreadyRunning),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            fs::remove_file(path).map_err(OperationsIpcError::RemoveStale)
        }
        Err(error) => Err(OperationsIpcError::Probe(error)),
    }
}

fn u8_field(object: &serde_json::Map<String, Value>, key: &'static str) -> Result<u8, Value> {
    let Some(value) = object.get(key).and_then(Value::as_u64) else {
        return Err(error_response(
            "invalid_request",
            &format!("{key} must be an integer"),
        ));
    };
    u8::try_from(value)
        .map_err(|_| error_response("invalid_request", &format!("{key} is out of range")))
}

fn i16_field(object: &serde_json::Map<String, Value>, key: &'static str) -> Result<i16, Value> {
    let Some(value) = object.get(key).and_then(Value::as_i64) else {
        return Err(error_response(
            "invalid_request",
            &format!("{key} must be an integer"),
        ));
    };
    i16::try_from(value)
        .map_err(|_| error_response("invalid_request", &format!("{key} is out of range")))
}

fn u32_field(object: &serde_json::Map<String, Value>, key: &'static str) -> Result<u32, Value> {
    let Some(value) = object.get(key).and_then(Value::as_u64) else {
        return Err(error_response(
            "invalid_request",
            &format!("{key} must be an integer"),
        ));
    };
    u32::try_from(value)
        .map_err(|_| error_response("invalid_request", &format!("{key} is out of range")))
}

fn u16_field(object: &serde_json::Map<String, Value>, key: &'static str) -> Result<u16, Value> {
    let value = u64_field(object, key)?;
    u16::try_from(value)
        .map_err(|_| error_response("invalid_request", &format!("{key} is out of range")))
}

fn u64_field(object: &serde_json::Map<String, Value>, key: &'static str) -> Result<u64, Value> {
    object.get(key).and_then(Value::as_u64).ok_or_else(|| {
        error_response(
            "invalid_request",
            &format!("{key} must be an unsigned integer"),
        )
    })
}

fn hardware_id_field(
    object: &serde_json::Map<String, Value>,
    key: &'static str,
) -> Result<sensor_core::HardwareId, Value> {
    let Some(value) = object.get(key).and_then(Value::as_str) else {
        return Err(error_response(
            "invalid_request",
            &format!("{key} must be a 16-digit hexadecimal string"),
        ));
    };
    if value.len() != 16 || !value.is_ascii() {
        return Err(error_response("invalid_request", "invalid hardware_id"));
    }
    let mut bytes = [0_u8; 8];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| error_response("invalid_request", "invalid hardware_id"))?;
    }
    if bytes == [0; 8] {
        return Err(error_response("invalid_request", "invalid hardware_id"));
    }
    Ok(sensor_core::HardwareId::from_bytes(bytes))
}

fn configuration_field(
    object: &serde_json::Map<String, Value>,
) -> Result<MeasurementConfiguration, Value> {
    let Some(configuration) = object.get("configuration").and_then(Value::as_object) else {
        return Err(error_response(
            "invalid_request",
            "configuration object is required",
        ));
    };
    let output_rate = match u8_field(configuration, "output_rate_code")? {
        0 => OutputRateCode::Hz4000,
        1 => OutputRateCode::Hz2000,
        2 => OutputRateCode::Hz1000,
        3 => OutputRateCode::Hz500,
        4 => OutputRateCode::Hz250,
        5 => OutputRateCode::Hz125,
        6 => OutputRateCode::Hz62_5,
        7 => OutputRateCode::Hz31_25,
        8 => OutputRateCode::Hz15_625,
        9 => OutputRateCode::Hz7_8125,
        10 => OutputRateCode::Hz3_90625,
        _ => {
            return Err(error_response(
                "invalid_request",
                "unknown output_rate_code",
            ))
        }
    };
    let filter_profile = match u8_field(configuration, "filter_profile")? {
        0 => DecimationFilterProfile::Light,
        1 => DecimationFilterProfile::Balanced,
        2 => DecimationFilterProfile::Aggressive,
        _ => return Err(error_response("invalid_request", "unknown filter_profile")),
    };
    let value = MeasurementConfiguration {
        output_rate,
        range_g: u8_field(configuration, "range_g")?,
        high_pass_corner: u8_field(configuration, "high_pass_corner")?,
        filter_profile,
        fifo_watermark: u8_field(configuration, "fifo_watermark")?,
        offset_x: i16_field(configuration, "offset_x")?,
        offset_y: i16_field(configuration, "offset_y")?,
        offset_z: i16_field(configuration, "offset_z")?,
    };
    value
        .validate()
        .map_err(|error| error_response("invalid_configuration", &error.to_string()))
}

fn operation_id_field(object: &serde_json::Map<String, Value>) -> Result<OperationId, Value> {
    let Some(value) = object.get("operation_id").and_then(Value::as_u64) else {
        return Err(error_response(
            "invalid_request",
            "operation_id must be an integer",
        ));
    };
    OperationId::new(value).map_err(operations_error_response)
}

fn mode_label(mode: ChannelOperatingMode) -> &'static str {
    match mode {
        ChannelOperatingMode::Recording => "recording",
        ChannelOperatingMode::EnteringMaintenance => "entering_maintenance",
        ChannelOperatingMode::Maintenance => "maintenance",
        ChannelOperatingMode::ResumingRecording => "resuming_recording",
        ChannelOperatingMode::Faulted => "faulted",
        ChannelOperatingMode::Offline => "offline",
    }
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

fn error_response(code: &str, message: &str) -> Value {
    json!({
        "schema_version": OPERATIONS_IPC_SCHEMA_VERSION,
        "ok": false,
        "error": {"code": code, "message": message}
    })
}

fn operation_response(
    channel_id: u8,
    operation_id: OperationId,
    phase: &str,
    mode: Option<ChannelOperatingMode>,
    result: Option<OperationResult>,
    error: Option<OperationsError>,
) -> Value {
    json!({
        "schema_version": OPERATIONS_IPC_SCHEMA_VERSION,
        "ok": true,
        "channel_id": channel_id,
        "operation_id": operation_id.get(),
        "phase": phase,
        "mode": mode.map(mode_label),
        "result": result.map(operation_result_json),
        "error": error.map(|value| value.to_string())
    })
}

fn operation_result_json(result: OperationResult) -> Value {
    match result {
        OperationResult::ConfigurationObserved(value) => json!({
            "kind": "configuration_observed",
            "observed": observed_configuration_json(value)
        }),
        OperationResult::ConfigurationValidated(value) => json!({
            "kind": "configuration_validated",
            "code": value.code as u8,
            "field": value.field as u8
        }),
        OperationResult::ConfigurationApplied(value) => json!({
            "kind": "configuration_applied",
            "wire_operation_id": value.operation_id,
            "code": value.code as u8,
            "observed": observed_configuration_json(value.observed),
            "effective_sample_seq": value.effective_sample_seq
        }),
        OperationResult::CommissioningScan {
            round_nonce,
            slot_count,
            discovered,
        } => json!({
            "kind": "commissioning_scan",
            "round_nonce": round_nonce,
            "slot_count": slot_count,
            "discovered": discovered.into_iter().map(|identity| json!({
                "hardware_id": hardware_id_hex(identity.hardware_id),
                "board_revision": identity.board_revision,
                "firmware": format!("{}.{}.{}", identity.firmware.major, identity.firmware.minor, identity.firmware.patch),
                "framing_version": identity.protocol.framing,
                "application_version": identity.protocol.application
            })).collect::<Vec<_>>()
        }),
        OperationResult::AddressAssigned(value) => json!({
            "kind": "address_assigned",
            "wire_operation_id": value.operation_id,
            "hardware_id": hardware_id_hex(value.hardware_id),
            "node_address": value.node_address
        }),
        OperationResult::NodeRestartRequested { node_address } => json!({
            "kind": "node_restart_requested",
            "node_address": node_address
        }),
        OperationResult::TelemetryIntervalApplied {
            node_address,
            interval_seconds,
        } => json!({
            "kind": "telemetry_interval_applied",
            "node_address": node_address,
            "telemetry_interval_seconds": interval_seconds
        }),
    }
}

fn hardware_id_hex(hardware_id: sensor_core::HardwareId) -> String {
    use core::fmt::Write as _;
    let mut output = String::with_capacity(16);
    for byte in hardware_id.bytes() {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn observed_configuration_json(value: sensor_protocol::ObservedConfiguration) -> Value {
    json!({
        "revision": value.revision,
        "configuration": measurement_configuration_json(value.desired),
        "sensor_odr_register": value.sensor_odr.register_value(),
        "decimation_factor": value.decimation_factor
    })
}

fn measurement_configuration_json(value: MeasurementConfiguration) -> Value {
    json!({
        "output_rate_code": value.output_rate as u8,
        "range_g": value.range_g,
        "high_pass_corner": value.high_pass_corner,
        "filter_profile": value.filter_profile as u8,
        "fifo_watermark": value.fifo_watermark,
        "offset_x": value.offset_x,
        "offset_y": value.offset_y,
        "offset_z": value.offset_z
    })
}

fn operations_error_response(error: OperationsError) -> Value {
    error_response("operation_rejected", &error.to_string())
}

#[derive(Debug)]
pub enum OperationsIpcError {
    MissingParent,
    ZeroPollBudget,
    CreateDirectory(io::Error),
    Inspect(io::Error),
    PathOccupied,
    AlreadyRunning,
    Probe(io::Error),
    RemoveStale(io::Error),
    Bind(io::Error),
    ConfigurePermissions(io::Error),
    Configure(io::Error),
    Receive(io::Error),
    Send(io::Error),
    Serialize(serde_json::Error),
    ResponseTooLarge(usize),
}

impl fmt::Display for OperationsIpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "operations IPC error: {self:?}")
    }
}

impl std::error::Error for OperationsIpcError {}

/// Small synchronous client used by CLI and future local adapters.
pub fn request(
    socket_path: &Path,
    request: &Value,
    timeout: Duration,
) -> Result<Value, OperationsIpcClientError> {
    let client_path = client_socket_path(socket_path);
    let client = UnixDatagram::bind(&client_path).map_err(OperationsIpcClientError::Bind)?;
    let cleanup = ClientSocketCleanup(client_path);
    fs::set_permissions(&cleanup.0, fs::Permissions::from_mode(0o600))
        .map_err(OperationsIpcClientError::ConfigurePermissions)?;
    client
        .set_read_timeout(Some(timeout))
        .map_err(OperationsIpcClientError::Configure)?;
    let bytes = serde_json::to_vec(request).map_err(OperationsIpcClientError::Serialize)?;
    if bytes.len() > MAX_OPERATIONS_REQUEST_BYTES {
        return Err(OperationsIpcClientError::RequestTooLarge(bytes.len()));
    }
    client
        .send_to(&bytes, socket_path)
        .map_err(OperationsIpcClientError::Send)?;
    let mut response = [0_u8; MAX_OPERATIONS_RESPONSE_BYTES + 1];
    let size = client
        .recv(&mut response)
        .map_err(OperationsIpcClientError::Receive)?;
    drop(cleanup);
    if size > MAX_OPERATIONS_RESPONSE_BYTES {
        return Err(OperationsIpcClientError::ResponseTooLarge(size));
    }
    serde_json::from_slice(&response[..size]).map_err(OperationsIpcClientError::Deserialize)
}

fn client_socket_path(_server_path: &Path) -> PathBuf {
    let file_name = format!(
        "op-{}-{:016x}.sock",
        std::process::id(),
        client_nonce() as u64
    );
    // A systemd service with PrivateTmp=true cannot reply to a pathname in the
    // caller's /tmp namespace. Linux clients therefore bind beside the server
    // socket. Other platforms retain a short /tmp path for sockaddr_un.
    #[cfg(target_os = "linux")]
    let directory = _server_path.parent().unwrap_or_else(|| Path::new("/tmp"));
    #[cfg(not(target_os = "linux"))]
    let directory = Path::new("/tmp");
    directory.join(file_name)
}

fn client_nonce() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

struct ClientSocketCleanup(PathBuf);

impl Drop for ClientSocketCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[derive(Debug)]
pub enum OperationsIpcClientError {
    Bind(io::Error),
    ConfigurePermissions(io::Error),
    Configure(io::Error),
    Serialize(serde_json::Error),
    RequestTooLarge(usize),
    Send(io::Error),
    Receive(io::Error),
    ResponseTooLarge(usize),
    Deserialize(serde_json::Error),
}

impl fmt::Display for OperationsIpcClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "operations IPC client error: {self:?}")
    }
}

impl std::error::Error for OperationsIpcClientError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::{
        ChannelCommandDecision, ChannelOperationsController, ChannelOperationsPolicy,
        OperationResult,
    };

    #[test]
    fn client_reply_socket_is_visible_to_a_private_tmp_server() {
        let server = Path::new("/run/sensor-system/operations-v1.sock");
        let client = client_socket_path(server);
        #[cfg(target_os = "linux")]
        assert_eq!(client.parent(), server.parent());
        #[cfg(not(target_os = "linux"))]
        assert_eq!(client.parent(), Some(Path::new("/tmp")));
    }
    use sensor_protocol::{Adxl355Odr, ObservedConfiguration};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    fn socket_path(label: &str) -> PathBuf {
        Path::new("/tmp").join(format!(
            "sensor-operations-ipc-{}-{}-{label}.sock",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn exchange(server: &OperationsIpcServer, path: &Path, value: Value) -> Value {
        let client_path = socket_path("client");
        let client = UnixDatagram::bind(&client_path).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        client
            .send_to(&serde_json::to_vec(&value).unwrap(), path)
            .unwrap();
        assert_eq!(server.poll().unwrap(), 1);
        let mut bytes = [0_u8; MAX_OPERATIONS_RESPONSE_BYTES + 1];
        let size = client.recv(&mut bytes).unwrap();
        let _ = fs::remove_file(client_path);
        serde_json::from_slice(&bytes[..size]).unwrap()
    }

    #[test]
    fn routes_request_to_bounded_channel_handle_and_reports_terminal_result() {
        let path = socket_path("server");
        let (mut controller, handle) = ChannelOperationsController::new(
            1,
            ChannelOperationsPolicy {
                mailbox_capacity: 2,
                ledger_capacity: 4,
            },
        )
        .unwrap();
        let server = OperationsIpcServer::bind(&path, vec![handle]).unwrap();
        let submitted = exchange(
            &server,
            &path,
            json!({
                "schema_version": 1,
                "action": "submit",
                "channel_id": 1,
                "operation_id": 42,
                "operation": "enter_maintenance"
            }),
        );
        assert_eq!(submitted["phase"], "queued");
        assert!(matches!(
            controller.poll_command().unwrap(),
            ChannelCommandDecision::Started(_)
        ));
        controller
            .set_mode(ChannelOperatingMode::Maintenance)
            .unwrap();
        controller.finish_active(true).unwrap();
        let result = exchange(
            &server,
            &path,
            json!({
                "schema_version": 1,
                "action": "get",
                "channel_id": 1,
                "operation_id": 42
            }),
        );
        assert_eq!(result["phase"], "succeeded");
        assert_eq!(result["mode"], "maintenance");

        let retry = exchange(
            &server,
            &path,
            json!({
                "schema_version": 1,
                "action": "submit",
                "channel_id": 1,
                "operation_id": 42,
                "operation": "enter_maintenance"
            }),
        );
        assert_eq!(retry["phase"], "succeeded");
        assert_eq!(
            controller.poll_command().unwrap(),
            ChannelCommandDecision::None
        );

        let conflict = exchange(
            &server,
            &path,
            json!({
                "schema_version": 1,
                "action": "submit",
                "channel_id": 1,
                "operation_id": 42,
                "operation": "resume_recording"
            }),
        );
        assert_eq!(conflict["ok"], false);
        assert_eq!(conflict["error"]["code"], "operation_rejected");
    }

    #[test]
    fn refuses_non_socket_path_and_second_live_owner() {
        let file = socket_path("file");
        fs::write(&file, b"occupied").unwrap();
        assert!(matches!(
            OperationsIpcServer::bind(&file, vec![]),
            Err(OperationsIpcError::PathOccupied)
        ));
        fs::remove_file(&file).unwrap();

        let path = socket_path("owned");
        let _server = OperationsIpcServer::bind(&path, vec![]).unwrap();
        assert!(matches!(
            OperationsIpcServer::bind(&path, vec![]),
            Err(OperationsIpcError::AlreadyRunning)
        ));
    }

    #[test]
    fn routes_node_configuration_and_serializes_typed_result() {
        let path = socket_path("configuration");
        let (mut controller, handle) = ChannelOperationsController::new(
            1,
            ChannelOperationsPolicy {
                mailbox_capacity: 2,
                ledger_capacity: 4,
            },
        )
        .unwrap();
        controller
            .set_mode(ChannelOperatingMode::Maintenance)
            .unwrap();
        let server = OperationsIpcServer::bind(&path, vec![handle]).unwrap();
        let submitted = exchange(
            &server,
            &path,
            json!({
                "schema_version": 1,
                "action": "submit",
                "channel_id": 1,
                "node_address": 7,
                "operation_id": 50,
                "operation": "get_configuration"
            }),
        );
        assert_eq!(submitted["phase"], "queued");
        let request = match controller.poll_command().unwrap() {
            ChannelCommandDecision::Started(request) => request,
            other => panic!("unexpected command decision: {other:?}"),
        };
        assert_eq!(
            request.target,
            OperationTarget::Node {
                channel_id: 1,
                node_address: 7
            }
        );
        let observed = ObservedConfiguration {
            revision: 9,
            desired: MeasurementConfiguration {
                output_rate: OutputRateCode::Hz125,
                range_g: 2,
                high_pass_corner: 0,
                filter_profile: DecimationFilterProfile::Balanced,
                fifo_watermark: 30,
                offset_x: 0,
                offset_y: 0,
                offset_z: 0,
            },
            sensor_odr: Adxl355Odr::Hz125,
            decimation_factor: 1,
        };
        controller
            .complete_active(OperationResult::ConfigurationObserved(observed))
            .unwrap();
        let result = exchange(
            &server,
            &path,
            json!({
                "schema_version": 1,
                "action": "get",
                "channel_id": 1,
                "operation_id": 50
            }),
        );
        assert_eq!(result["phase"], "succeeded");
        assert_eq!(result["result"]["kind"], "configuration_observed");
        assert_eq!(result["result"]["observed"]["revision"], 9);
        assert_eq!(
            result["result"]["observed"]["configuration"]["output_rate_code"],
            5
        );

        let submitted = exchange(
            &server,
            &path,
            json!({
                "schema_version": 1,
                "action": "submit",
                "channel_id": 1,
                "node_address": 7,
                "operation_id": 52,
                "operation": "validate_configuration",
                "configuration": {
                    "output_rate_code": 6,
                    "range_g": 2,
                    "high_pass_corner": 0,
                    "filter_profile": 1,
                    "fifo_watermark": 30,
                    "offset_x": -4,
                    "offset_y": 0,
                    "offset_z": 4
                }
            }),
        );
        assert_eq!(submitted["phase"], "queued");
        let request = match controller.poll_command().unwrap() {
            ChannelCommandDecision::Started(request) => request,
            other => panic!("unexpected command decision: {other:?}"),
        };
        assert!(matches!(
            request.kind,
            OperationKind::ValidateConfiguration { configuration }
                if configuration.output_rate == OutputRateCode::Hz62_5
                    && configuration.offset_x == -4
                    && configuration.offset_z == 4
        ));
        controller
            .complete_active(OperationResult::ConfigurationValidated(
                sensor_protocol::ConfigValidationResult {
                    code: sensor_protocol::ConfigValidationCode::Valid,
                    field: sensor_protocol::ConfigField::None,
                },
            ))
            .unwrap();
        let result = exchange(
            &server,
            &path,
            json!({
                "schema_version": 1,
                "action": "get",
                "channel_id": 1,
                "operation_id": 52
            }),
        );
        assert_eq!(result["result"]["kind"], "configuration_validated");
        assert_eq!(result["result"]["code"], 0);
        assert_eq!(result["result"]["field"], 0);
    }

    #[test]
    fn routes_mutating_configuration_with_required_revision() {
        let path = socket_path("apply-reset");
        let (mut controller, handle) = ChannelOperationsController::new(
            1,
            ChannelOperationsPolicy {
                mailbox_capacity: 2,
                ledger_capacity: 4,
            },
        )
        .unwrap();
        controller
            .set_mode(ChannelOperatingMode::Maintenance)
            .unwrap();
        let server = OperationsIpcServer::bind(&path, vec![handle]).unwrap();
        let response = exchange(
            &server,
            &path,
            json!({
                "schema_version": 1,
                "action": "submit",
                "channel_id": 1,
                "node_address": 1,
                "operation_id": 51,
                "operation": "apply_configuration",
                "expected_revision": 9,
                "configuration": {
                    "output_rate_code": 6,
                    "range_g": 2,
                    "high_pass_corner": 0,
                    "filter_profile": 1,
                    "fifo_watermark": 30,
                    "offset_x": 0,
                    "offset_y": 0,
                    "offset_z": 0
                }
            }),
        );
        assert_eq!(response["phase"], "queued");
        let request = match controller.poll_command().unwrap() {
            ChannelCommandDecision::Started(request) => request,
            other => panic!("unexpected command decision: {other:?}"),
        };
        assert!(matches!(
            request.kind,
            OperationKind::ApplyConfiguration {
                expected_revision: 9,
                configuration
            } if configuration.output_rate == OutputRateCode::Hz62_5
        ));
        controller
            .fail_active(crate::operations::OperationsError::ExecutorUnavailable)
            .unwrap();

        let response = exchange(
            &server,
            &path,
            json!({
                "schema_version": 1,
                "action": "submit",
                "channel_id": 1,
                "node_address": 1,
                "operation_id": 53,
                "operation": "reset_configuration",
                "expected_revision": 10
            }),
        );
        assert_eq!(response["phase"], "queued");
        let request = match controller.poll_command().unwrap() {
            ChannelCommandDecision::Started(request) => request,
            other => panic!("unexpected command decision: {other:?}"),
        };
        assert!(matches!(
            request.kind,
            OperationKind::ResetConfiguration {
                expected_revision: 10
            }
        ));
    }

    #[test]
    fn routes_restart_and_commissioning_requests() {
        let path = socket_path("restart-commissioning");
        let (mut controller, handle) = ChannelOperationsController::new(
            2,
            ChannelOperationsPolicy {
                mailbox_capacity: 2,
                ledger_capacity: 4,
            },
        )
        .unwrap();
        controller
            .set_mode(ChannelOperatingMode::Maintenance)
            .unwrap();
        let server = OperationsIpcServer::bind(&path, vec![handle]).unwrap();

        let submitted = exchange(
            &server,
            &path,
            json!({
                "schema_version": 1,
                "action": "submit",
                "channel_id": 2,
                "node_address": 1,
                "operation_id": 60,
                "operation": "restart_node"
            }),
        );
        assert_eq!(submitted["phase"], "queued");
        let request = match controller.poll_command().unwrap() {
            ChannelCommandDecision::Started(request) => request,
            other => panic!("unexpected command decision: {other:?}"),
        };
        assert_eq!(request.kind, OperationKind::RestartNode);
        controller
            .fail_active(OperationsError::ExecutorUnavailable)
            .unwrap();

        let submitted = exchange(
            &server,
            &path,
            json!({
                "schema_version": 1,
                "action": "submit",
                "channel_id": 2,
                "operation_id": 61,
                "operation": "scan_unassigned",
                "round_nonce": 1234,
                "slot_count": 16
            }),
        );
        assert_eq!(submitted["phase"], "queued");
        let request = match controller.poll_command().unwrap() {
            ChannelCommandDecision::Started(request) => request,
            other => panic!("unexpected command decision: {other:?}"),
        };
        assert_eq!(
            request.kind,
            OperationKind::ScanUnassigned {
                round_nonce: 1234,
                slot_count: 16
            }
        );
    }

    #[test]
    fn malformed_and_unknown_channel_requests_fail_without_touching_workers() {
        let path = socket_path("invalid");
        let server = OperationsIpcServer::bind(&path, vec![]).unwrap();
        let response = exchange(
            &server,
            &path,
            json!({
                "schema_version": 1,
                "action": "status",
                "channel_id": 7
            }),
        );
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "unknown_channel");
    }
}
