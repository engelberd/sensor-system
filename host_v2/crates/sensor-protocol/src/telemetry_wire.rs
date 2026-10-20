use core::fmt;

use crate::{Adxl355TemperatureCode, Adxl355TemperatureCodeError, ResponseStatus, ServiceId};

pub const TELEMETRY_SERVICE_ID: u8 = ServiceId::Telemetry as u8;
pub const GET_TELEMETRY_SNAPSHOT_MESSAGE_ID: u8 = 1;
pub const TELEMETRY_SCHEMA_VERSION: u8 = 1;
pub const TELEMETRY_SNAPSHOT_RESPONSE_BYTES: usize = 14;
pub const ADXL355_TEMPERATURE_FIELD: u16 = 1 << 0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TelemetrySnapshot {
    pub captured_device_time_us: u64,
    pub adxl355_temperature: Adxl355TemperatureCode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryWireError {
    InvalidLength,
    UnknownStatus(u8),
    Remote(ResponseStatus),
    UnsupportedSchema(u8),
    InvalidFields(u16),
    ZeroCapturedTime,
    InvalidTemperature(Adxl355TemperatureCodeError),
}

impl fmt::Display for TelemetryWireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength => formatter.write_str("invalid Telemetry payload length"),
            Self::UnknownStatus(value) => write!(formatter, "unknown Telemetry status {value}"),
            Self::Remote(status) => write!(formatter, "Telemetry remote error: {status:?}"),
            Self::UnsupportedSchema(schema) => {
                write!(formatter, "unsupported Telemetry schema {schema}")
            }
            Self::InvalidFields(fields) => {
                write!(formatter, "invalid Telemetry fields {fields:#06x}")
            }
            Self::ZeroCapturedTime => formatter.write_str("Telemetry captured time is zero"),
            Self::InvalidTemperature(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TelemetryWireError {}

pub fn decode_telemetry_snapshot(payload: &[u8]) -> Result<TelemetrySnapshot, TelemetryWireError> {
    let Some(status_byte) = payload.first().copied() else {
        return Err(TelemetryWireError::InvalidLength);
    };
    let status = ResponseStatus::try_from(status_byte)
        .map_err(|_| TelemetryWireError::UnknownStatus(status_byte))?;
    if status != ResponseStatus::Ok {
        if payload.len() != 1 {
            return Err(TelemetryWireError::InvalidLength);
        }
        return Err(TelemetryWireError::Remote(status));
    }
    if payload.len() != TELEMETRY_SNAPSHOT_RESPONSE_BYTES {
        return Err(TelemetryWireError::InvalidLength);
    }
    if payload[1] != TELEMETRY_SCHEMA_VERSION {
        return Err(TelemetryWireError::UnsupportedSchema(payload[1]));
    }
    let captured_device_time_us = u64::from_le_bytes(payload[2..10].try_into().unwrap());
    if captured_device_time_us == 0 {
        return Err(TelemetryWireError::ZeroCapturedTime);
    }
    let fields = u16::from_le_bytes(payload[10..12].try_into().unwrap());
    if fields != ADXL355_TEMPERATURE_FIELD {
        return Err(TelemetryWireError::InvalidFields(fields));
    }
    let raw = u16::from_le_bytes(payload[12..14].try_into().unwrap());
    let adxl355_temperature =
        Adxl355TemperatureCode::new(raw).map_err(TelemetryWireError::InvalidTemperature)?;
    Ok(TelemetrySnapshot {
        captured_device_time_us,
        adxl355_temperature,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_schema_one_snapshot() {
        let mut payload = [0u8; TELEMETRY_SNAPSHOT_RESPONSE_BYTES];
        payload[0] = ResponseStatus::Ok as u8;
        payload[1] = TELEMETRY_SCHEMA_VERSION;
        payload[2..10].copy_from_slice(&123_456u64.to_le_bytes());
        payload[10..12].copy_from_slice(&ADXL355_TEMPERATURE_FIELD.to_le_bytes());
        payload[12..14].copy_from_slice(&1885u16.to_le_bytes());
        let decoded = decode_telemetry_snapshot(&payload).unwrap();
        assert_eq!(decoded.captured_device_time_us, 123_456);
        assert_eq!(decoded.adxl355_temperature.get(), 1885);
        assert_eq!(decoded.adxl355_temperature.celsius(), 25.0);
    }

    #[test]
    fn rejects_impossible_temperature_and_non_exact_error() {
        let mut payload = [0u8; TELEMETRY_SNAPSHOT_RESPONSE_BYTES];
        payload[0] = ResponseStatus::Ok as u8;
        payload[1] = TELEMETRY_SCHEMA_VERSION;
        payload[2..10].copy_from_slice(&1u64.to_le_bytes());
        payload[10..12].copy_from_slice(&ADXL355_TEMPERATURE_FIELD.to_le_bytes());
        assert!(matches!(
            decode_telemetry_snapshot(&payload),
            Err(TelemetryWireError::InvalidTemperature(_))
        ));
        assert_eq!(
            decode_telemetry_snapshot(&[ResponseStatus::Busy as u8]),
            Err(TelemetryWireError::Remote(ResponseStatus::Busy))
        );
        assert_eq!(
            decode_telemetry_snapshot(&[ResponseStatus::Busy as u8, 0]),
            Err(TelemetryWireError::InvalidLength)
        );
    }
}
