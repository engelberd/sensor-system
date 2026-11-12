use core::fmt;

use crate::{ResponseStatus, ServiceId};

pub const DIAGNOSTICS_SERVICE_ID: u8 = ServiceId::Diagnostics as u8;
pub const GET_DIAGNOSTIC_INFO_MESSAGE_ID: u8 = 1;
pub const GET_FAULT_SNAPSHOT_MESSAGE_ID: u8 = 2;
pub const GET_PERSISTENT_DIAGNOSTIC_RECORD_MESSAGE_ID: u8 = 3;
pub const READ_DIAGNOSTIC_EVENTS_MESSAGE_ID: u8 = 4;

pub const DIAGNOSTIC_INFO_RESPONSE_BYTES: usize = 25;
pub const FAULT_SNAPSHOT_RESPONSE_BYTES: usize = 70;
pub const PERSISTENT_DIAGNOSTIC_RECORD_RESPONSE_BYTES: usize = 84;
pub const READ_DIAGNOSTIC_EVENTS_REQUEST_BYTES: usize = 5;
pub const DIAGNOSTIC_EVENTS_RESPONSE_HEADER_BYTES: usize = 12;
pub const DIAGNOSTIC_EVENT_WIRE_BYTES: usize = 28;
pub const MAX_DIAGNOSTIC_EVENTS_PER_RESPONSE: u8 = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticInfo {
    pub reset_cause: u8,
    pub live_usb_enabled: bool,
    pub stored_event_count: u16,
    pub event_capacity: u16,
    pub dropped_event_count: u32,
    pub first_event_id: u32,
    pub next_event_id: u32,
    pub last_error_event_id: u32,
    pub last_error_code: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FaultSnapshot {
    pub event_id: u32,
    pub time_ms: u32,
    pub event_code: u16,
    pub severity: u8,
    pub reset_cause: u8,
    pub sample_seq: u64,
    pub last_progress_ms: u32,
    pub fifo_no_data: u32,
    pub sensor_errors: u32,
    pub dropped_samples: u32,
    pub rx_overflow_count: u32,
    pub packet_overwrite_count: u32,
    pub debug_gpio_int1_edges: u32,
    pub debug_gpio_drdy_edges: u32,
    pub debug_config_snapshot: u32,
    pub debug_irq_snapshot: u32,
    pub arg0: i32,
    pub arg1: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistentDiagnosticRecord {
    pub generation: u32,
    pub boot_counter: u32,
    pub firmware_version: u32,
    pub event_id: u32,
    pub time_ms: u32,
    pub event_code: u16,
    pub severity: u8,
    pub repeat_count: u8,
    pub reset_cause: u8,
    pub sample_seq: u64,
    pub last_progress_ms: u32,
    pub fifo_no_data: u32,
    pub sensor_errors: u32,
    pub dropped_samples: u32,
    pub rx_overflow_count: u32,
    pub packet_overwrite_count: u32,
    pub debug_gpio_int1_edges: u32,
    pub debug_gpio_drdy_edges: u32,
    pub debug_config_snapshot: u32,
    pub debug_irq_snapshot: u32,
    pub arg0: i32,
    pub arg1: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticEvent {
    pub event_id: u32,
    pub time_ms: u32,
    pub event_code: u16,
    pub severity: u8,
    pub repeat_count: u8,
    pub sample_seq: u64,
    pub arg0: i32,
    pub arg1: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticEventPage {
    pub first_event_id: u32,
    pub next_event_id: u32,
    pub events: Vec<DiagnosticEvent>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticsWireError {
    InvalidLength,
    InvalidPresence,
    InvalidBoolean,
    InvalidEventCount,
    NonZeroReserved,
    Status(crate::ApplicationContractError),
    Remote(ResponseStatus),
}

impl fmt::Display for DiagnosticsWireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength => formatter.write_str("invalid Diagnostics payload length"),
            Self::InvalidPresence => formatter.write_str("invalid Diagnostics presence marker"),
            Self::InvalidBoolean => formatter.write_str("invalid Diagnostics boolean"),
            Self::InvalidEventCount => formatter.write_str("invalid Diagnostics event count"),
            Self::NonZeroReserved => formatter.write_str("Diagnostics reserved bytes are non-zero"),
            Self::Status(error) => write!(formatter, "invalid Diagnostics status: {error}"),
            Self::Remote(status) => write!(formatter, "Diagnostics remote error: {status:?}"),
        }
    }
}

impl std::error::Error for DiagnosticsWireError {}

pub fn encode_read_events_request(
    start_event_id: u32,
    maximum: u8,
) -> Result<[u8; READ_DIAGNOSTIC_EVENTS_REQUEST_BYTES], DiagnosticsWireError> {
    if maximum == 0 || maximum > MAX_DIAGNOSTIC_EVENTS_PER_RESPONSE {
        return Err(DiagnosticsWireError::InvalidEventCount);
    }
    let mut output = [0; READ_DIAGNOSTIC_EVENTS_REQUEST_BYTES];
    output[0..4].copy_from_slice(&start_event_id.to_le_bytes());
    output[4] = maximum;
    Ok(output)
}

pub fn decode_diagnostic_info(payload: &[u8]) -> Result<DiagnosticInfo, DiagnosticsWireError> {
    require_ok(payload)?;
    require_length(payload, DIAGNOSTIC_INFO_RESPONSE_BYTES)?;
    let live_usb_enabled = decode_bool(payload[2])?;
    Ok(DiagnosticInfo {
        reset_cause: payload[1],
        live_usb_enabled,
        stored_event_count: read_u16(payload, 3),
        event_capacity: read_u16(payload, 5),
        dropped_event_count: read_u32(payload, 7),
        first_event_id: read_u32(payload, 11),
        next_event_id: read_u32(payload, 15),
        last_error_event_id: read_u32(payload, 19),
        last_error_code: read_u16(payload, 23),
    })
}

pub fn decode_fault_snapshot(
    payload: &[u8],
) -> Result<Option<FaultSnapshot>, DiagnosticsWireError> {
    require_ok(payload)?;
    let present = decode_presence(payload)?;
    if !present {
        require_length(payload, 2)?;
        return Ok(None);
    }
    require_length(payload, FAULT_SNAPSHOT_RESPONSE_BYTES)?;
    Ok(Some(FaultSnapshot {
        event_id: read_u32(payload, 2),
        time_ms: read_u32(payload, 6),
        event_code: read_u16(payload, 10),
        severity: payload[12],
        reset_cause: payload[13],
        sample_seq: read_u64(payload, 14),
        last_progress_ms: read_u32(payload, 22),
        fifo_no_data: read_u32(payload, 26),
        sensor_errors: read_u32(payload, 30),
        dropped_samples: read_u32(payload, 34),
        rx_overflow_count: read_u32(payload, 38),
        packet_overwrite_count: read_u32(payload, 42),
        debug_gpio_int1_edges: read_u32(payload, 46),
        debug_gpio_drdy_edges: read_u32(payload, 50),
        debug_config_snapshot: read_u32(payload, 54),
        debug_irq_snapshot: read_u32(payload, 58),
        arg0: read_i32(payload, 62),
        arg1: read_i32(payload, 66),
    }))
}

pub fn decode_persistent_diagnostic_record(
    payload: &[u8],
) -> Result<Option<PersistentDiagnosticRecord>, DiagnosticsWireError> {
    require_ok(payload)?;
    let present = decode_presence(payload)?;
    if !present {
        require_length(payload, 2)?;
        return Ok(None);
    }
    require_length(payload, PERSISTENT_DIAGNOSTIC_RECORD_RESPONSE_BYTES)?;
    if payload[27] != 0 {
        return Err(DiagnosticsWireError::NonZeroReserved);
    }
    Ok(Some(PersistentDiagnosticRecord {
        generation: read_u32(payload, 2),
        boot_counter: read_u32(payload, 6),
        firmware_version: read_u32(payload, 10),
        event_id: read_u32(payload, 14),
        time_ms: read_u32(payload, 18),
        event_code: read_u16(payload, 22),
        severity: payload[24],
        repeat_count: payload[25],
        reset_cause: payload[26],
        sample_seq: read_u64(payload, 28),
        last_progress_ms: read_u32(payload, 36),
        fifo_no_data: read_u32(payload, 40),
        sensor_errors: read_u32(payload, 44),
        dropped_samples: read_u32(payload, 48),
        rx_overflow_count: read_u32(payload, 52),
        packet_overwrite_count: read_u32(payload, 56),
        debug_gpio_int1_edges: read_u32(payload, 60),
        debug_gpio_drdy_edges: read_u32(payload, 64),
        debug_config_snapshot: read_u32(payload, 68),
        debug_irq_snapshot: read_u32(payload, 72),
        arg0: read_i32(payload, 76),
        arg1: read_i32(payload, 80),
    }))
}

pub fn decode_diagnostic_events(
    payload: &[u8],
) -> Result<DiagnosticEventPage, DiagnosticsWireError> {
    require_ok(payload)?;
    if payload.len() < DIAGNOSTIC_EVENTS_RESPONSE_HEADER_BYTES {
        return Err(DiagnosticsWireError::InvalidLength);
    }
    let count = usize::from(payload[1]);
    if count > usize::from(MAX_DIAGNOSTIC_EVENTS_PER_RESPONSE) {
        return Err(DiagnosticsWireError::InvalidEventCount);
    }
    if payload[2] != 0 || payload[3] != 0 {
        return Err(DiagnosticsWireError::NonZeroReserved);
    }
    let expected = DIAGNOSTIC_EVENTS_RESPONSE_HEADER_BYTES + count * DIAGNOSTIC_EVENT_WIRE_BYTES;
    require_length(payload, expected)?;
    let mut events = Vec::with_capacity(count);
    for index in 0..count {
        let offset = DIAGNOSTIC_EVENTS_RESPONSE_HEADER_BYTES + index * DIAGNOSTIC_EVENT_WIRE_BYTES;
        events.push(DiagnosticEvent {
            event_id: read_u32(payload, offset),
            time_ms: read_u32(payload, offset + 4),
            event_code: read_u16(payload, offset + 8),
            severity: payload[offset + 10],
            repeat_count: payload[offset + 11],
            sample_seq: read_u64(payload, offset + 12),
            arg0: read_i32(payload, offset + 20),
            arg1: read_i32(payload, offset + 24),
        });
    }
    Ok(DiagnosticEventPage {
        first_event_id: read_u32(payload, 4),
        next_event_id: read_u32(payload, 8),
        events,
    })
}

fn require_ok(payload: &[u8]) -> Result<(), DiagnosticsWireError> {
    let Some(status) = payload.first().copied() else {
        return Err(DiagnosticsWireError::InvalidLength);
    };
    let status = ResponseStatus::try_from(status).map_err(DiagnosticsWireError::Status)?;
    if status != ResponseStatus::Ok {
        return Err(DiagnosticsWireError::Remote(status));
    }
    Ok(())
}

fn decode_presence(payload: &[u8]) -> Result<bool, DiagnosticsWireError> {
    match payload.get(1).copied() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        Some(_) => Err(DiagnosticsWireError::InvalidPresence),
        None => Err(DiagnosticsWireError::InvalidLength),
    }
}

fn decode_bool(value: u8) -> Result<bool, DiagnosticsWireError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(DiagnosticsWireError::InvalidBoolean),
    }
}

fn require_length(payload: &[u8], expected: usize) -> Result<(), DiagnosticsWireError> {
    if payload.len() == expected {
        Ok(())
    } else {
        Err(DiagnosticsWireError::InvalidLength)
    }
}

fn read_u16(input: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(input[offset..offset + 2].try_into().unwrap())
}

fn read_u32(input: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(input[offset..offset + 4].try_into().unwrap())
}

fn read_i32(input: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(input[offset..offset + 4].try_into().unwrap())
}

fn read_u64(input: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(input[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_info_and_absence() {
        let mut info = [0; DIAGNOSTIC_INFO_RESPONSE_BYTES];
        info[1] = 1;
        info[3..5].copy_from_slice(&2u16.to_le_bytes());
        info[5..7].copy_from_slice(&512u16.to_le_bytes());
        info[7..11].copy_from_slice(&3u32.to_le_bytes());
        info[11..15].copy_from_slice(&7u32.to_le_bytes());
        info[15..19].copy_from_slice(&9u32.to_le_bytes());
        info[19..23].copy_from_slice(&8u32.to_le_bytes());
        info[23..25].copy_from_slice(&36u16.to_le_bytes());
        assert_eq!(decode_diagnostic_info(&info).unwrap().last_error_code, 36);
        assert_eq!(decode_fault_snapshot(&[0, 0]), Ok(None));
        assert_eq!(decode_persistent_diagnostic_record(&[0, 0]), Ok(None));
    }

    #[test]
    fn rejects_noncanonical_payloads() {
        assert_eq!(
            decode_fault_snapshot(&[0, 2]),
            Err(DiagnosticsWireError::InvalidPresence)
        );
        assert_eq!(
            encode_read_events_request(1, 0),
            Err(DiagnosticsWireError::InvalidEventCount)
        );
        assert_eq!(
            decode_diagnostic_info(&[ResponseStatus::Busy as u8]),
            Err(DiagnosticsWireError::Remote(ResponseStatus::Busy))
        );
    }

    #[test]
    fn decodes_event_page() {
        let mut payload =
            vec![0; DIAGNOSTIC_EVENTS_RESPONSE_HEADER_BYTES + DIAGNOSTIC_EVENT_WIRE_BYTES];
        payload[1] = 1;
        payload[4..8].copy_from_slice(&7u32.to_le_bytes());
        payload[8..12].copy_from_slice(&9u32.to_le_bytes());
        payload[12..16].copy_from_slice(&8u32.to_le_bytes());
        payload[20..22].copy_from_slice(&36u16.to_le_bytes());
        payload[22] = 3;
        payload[23] = 1;
        payload[24..32].copy_from_slice(&60u64.to_le_bytes());
        payload[32..36].copy_from_slice(&3i32.to_le_bytes());
        payload[36..40].copy_from_slice(&(-4i32).to_le_bytes());
        let page = decode_diagnostic_events(&payload).unwrap();
        assert_eq!(page.first_event_id, 7);
        assert_eq!(page.next_event_id, 9);
        assert_eq!(page.events[0].arg1, -4);
    }
}
