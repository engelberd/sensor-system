use core::fmt;

use crate::{Adxl355Odr, OutputRateCode, ResponseStatus, ServiceId};

pub const CONFIGURATION_SERVICE_ID: u8 = ServiceId::Configuration as u8;
pub const GET_CONFIG_MESSAGE_ID: u8 = 1;
pub const VALIDATE_CONFIG_MESSAGE_ID: u8 = 2;
pub const APPLY_CONFIG_MESSAGE_ID: u8 = 3;
pub const RESET_CONFIG_MESSAGE_ID: u8 = 4;
pub const CONFIGURATION_SCHEMA_VERSION: u8 = 1;

pub const CONFIGURATION_VALUE_BYTES: usize = 12;
pub const OBSERVED_CONFIG_BYTES: usize = 18;
pub const GET_CONFIG_RESPONSE_BYTES: usize = 1 + OBSERVED_CONFIG_BYTES;
pub const VALIDATE_CONFIG_RESPONSE_BYTES: usize = 3;
pub const APPLY_CONFIG_REQUEST_BYTES: usize = 8 + CONFIGURATION_VALUE_BYTES;
pub const RESET_CONFIG_REQUEST_BYTES: usize = 8;
pub const CONFIG_OPERATION_RESPONSE_BYTES: usize = 1 + 4 + 1 + OBSERVED_CONFIG_BYTES + 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum DecimationFilterProfile {
    Light = 0,
    Balanced = 1,
    Aggressive = 2,
}

impl DecimationFilterProfile {
    const fn from_wire(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Light),
            1 => Some(Self::Balanced),
            2 => Some(Self::Aggressive),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeasurementConfiguration {
    pub output_rate: OutputRateCode,
    pub range_g: u8,
    pub high_pass_corner: u8,
    pub filter_profile: DecimationFilterProfile,
    pub fifo_watermark: u8,
    pub offset_x: i16,
    pub offset_y: i16,
    pub offset_z: i16,
}

impl MeasurementConfiguration {
    /// Product default used by ResetConfig. It mirrors the firmware
    /// ConfigManager default and intentionally excludes address and link data.
    pub const fn product_default() -> Self {
        Self {
            output_rate: OutputRateCode::Hz250,
            range_g: 2,
            high_pass_corner: 0,
            filter_profile: DecimationFilterProfile::Balanced,
            fifo_watermark: 30,
            offset_x: 0,
            offset_y: 0,
            offset_z: 0,
        }
    }

    pub fn validate(self) -> Result<Self, ConfigurationWireError> {
        if !matches!(self.range_g, 2 | 4 | 8) {
            return Err(ConfigurationWireError::InvalidRange(self.range_g));
        }
        if self.high_pass_corner > 7 {
            return Err(ConfigurationWireError::InvalidHighPassCorner(
                self.high_pass_corner,
            ));
        }
        if !(3..=96).contains(&self.fifo_watermark) || self.fifo_watermark % 3 != 0 {
            return Err(ConfigurationWireError::InvalidFifoWatermark(
                self.fifo_watermark,
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservedConfiguration {
    pub revision: u32,
    pub desired: MeasurementConfiguration,
    pub sensor_odr: Adxl355Odr,
    pub decimation_factor: u8,
}

impl ObservedConfiguration {
    pub fn validate(self) -> Result<Self, ConfigurationWireError> {
        self.desired.validate()?;
        if self.revision == 0 {
            return Err(ConfigurationWireError::ZeroRevision);
        }
        if self.decimation_factor == 0 {
            return Err(ConfigurationWireError::ZeroDecimationFactor);
        }
        if self
            .sensor_odr
            .sample_rate()
            .divided_by(self.decimation_factor as u32)
            .ok()
            != Some(self.desired.output_rate.sample_rate())
        {
            return Err(ConfigurationWireError::InconsistentObservedRate);
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ConfigValidationCode {
    Valid = 0,
    UnsupportedOutputRate = 1,
    InvalidRange = 2,
    InvalidHighPassCorner = 3,
    InvalidFilterProfile = 4,
    InvalidFifoWatermark = 5,
    InvalidOffset = 6,
}

impl ConfigValidationCode {
    fn from_wire(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Valid),
            1 => Some(Self::UnsupportedOutputRate),
            2 => Some(Self::InvalidRange),
            3 => Some(Self::InvalidHighPassCorner),
            4 => Some(Self::InvalidFilterProfile),
            5 => Some(Self::InvalidFifoWatermark),
            6 => Some(Self::InvalidOffset),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ConfigField {
    None = 0,
    OutputRate = 1,
    Range = 2,
    HighPassCorner = 3,
    FilterProfile = 4,
    FifoWatermark = 5,
    OffsetX = 6,
    OffsetY = 7,
    OffsetZ = 8,
}

impl ConfigField {
    fn from_wire(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::OutputRate),
            2 => Some(Self::Range),
            3 => Some(Self::HighPassCorner),
            4 => Some(Self::FilterProfile),
            5 => Some(Self::FifoWatermark),
            6 => Some(Self::OffsetX),
            7 => Some(Self::OffsetY),
            8 => Some(Self::OffsetZ),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigValidationResult {
    pub code: ConfigValidationCode,
    pub field: ConfigField,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigOperationRequest {
    pub operation_id: u32,
    pub expected_revision: u32,
    pub configuration: MeasurementConfiguration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ConfigOperationCode {
    Applied = 0,
    RevisionConflict = 1,
    InvalidConfiguration = 2,
    StorageFailed = 3,
    SensorFailed = 4,
    RollbackFailed = 5,
    WrongMode = 6,
}

impl ConfigOperationCode {
    fn from_wire(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Applied),
            1 => Some(Self::RevisionConflict),
            2 => Some(Self::InvalidConfiguration),
            3 => Some(Self::StorageFailed),
            4 => Some(Self::SensorFailed),
            5 => Some(Self::RollbackFailed),
            6 => Some(Self::WrongMode),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigOperationResult {
    pub operation_id: u32,
    pub code: ConfigOperationCode,
    pub observed: ObservedConfiguration,
    pub effective_sample_seq: u64,
}

pub fn encode_configuration(
    value: MeasurementConfiguration,
) -> Result<[u8; CONFIGURATION_VALUE_BYTES], ConfigurationWireError> {
    value.validate()?;
    let mut out = [0; CONFIGURATION_VALUE_BYTES];
    out[0] = CONFIGURATION_SCHEMA_VERSION;
    out[1] = value.output_rate as u8;
    out[2] = value.range_g;
    out[3] = value.high_pass_corner;
    out[4] = value.filter_profile as u8;
    out[5] = value.fifo_watermark;
    out[6..8].copy_from_slice(&value.offset_x.to_le_bytes());
    out[8..10].copy_from_slice(&value.offset_y.to_le_bytes());
    out[10..12].copy_from_slice(&value.offset_z.to_le_bytes());
    Ok(out)
}

pub fn decode_configuration(
    payload: &[u8],
) -> Result<MeasurementConfiguration, ConfigurationWireError> {
    if payload.len() != CONFIGURATION_VALUE_BYTES {
        return Err(ConfigurationWireError::InvalidLength(payload.len()));
    }
    if payload[0] != CONFIGURATION_SCHEMA_VERSION {
        return Err(ConfigurationWireError::UnsupportedSchema(payload[0]));
    }
    MeasurementConfiguration {
        output_rate: OutputRateCode::from_wire(payload[1])
            .ok_or(ConfigurationWireError::UnknownOutputRate(payload[1]))?,
        range_g: payload[2],
        high_pass_corner: payload[3],
        filter_profile: DecimationFilterProfile::from_wire(payload[4])
            .ok_or(ConfigurationWireError::UnknownFilterProfile(payload[4]))?,
        fifo_watermark: payload[5],
        offset_x: i16::from_le_bytes(payload[6..8].try_into().unwrap()),
        offset_y: i16::from_le_bytes(payload[8..10].try_into().unwrap()),
        offset_z: i16::from_le_bytes(payload[10..12].try_into().unwrap()),
    }
    .validate()
}

pub fn encode_apply_config_request(
    request: ConfigOperationRequest,
) -> Result<[u8; APPLY_CONFIG_REQUEST_BYTES], ConfigurationWireError> {
    validate_operation_ids(request.operation_id, request.expected_revision)?;
    let mut out = [0; APPLY_CONFIG_REQUEST_BYTES];
    out[0..4].copy_from_slice(&request.operation_id.to_le_bytes());
    out[4..8].copy_from_slice(&request.expected_revision.to_le_bytes());
    out[8..].copy_from_slice(&encode_configuration(request.configuration)?);
    Ok(out)
}

pub fn decode_apply_config_request(
    payload: &[u8],
) -> Result<ConfigOperationRequest, ConfigurationWireError> {
    if payload.len() != APPLY_CONFIG_REQUEST_BYTES {
        return Err(ConfigurationWireError::InvalidLength(payload.len()));
    }
    let request = ConfigOperationRequest {
        operation_id: read_u32(payload, 0),
        expected_revision: read_u32(payload, 4),
        configuration: decode_configuration(&payload[8..])?,
    };
    validate_operation_ids(request.operation_id, request.expected_revision)?;
    Ok(request)
}

pub fn encode_reset_config_request(
    operation_id: u32,
    expected_revision: u32,
) -> Result<[u8; RESET_CONFIG_REQUEST_BYTES], ConfigurationWireError> {
    validate_operation_ids(operation_id, expected_revision)?;
    let mut out = [0; RESET_CONFIG_REQUEST_BYTES];
    out[0..4].copy_from_slice(&operation_id.to_le_bytes());
    out[4..8].copy_from_slice(&expected_revision.to_le_bytes());
    Ok(out)
}

pub fn decode_reset_config_request(payload: &[u8]) -> Result<(u32, u32), ConfigurationWireError> {
    if payload.len() != RESET_CONFIG_REQUEST_BYTES {
        return Err(ConfigurationWireError::InvalidLength(payload.len()));
    }
    let values = (read_u32(payload, 0), read_u32(payload, 4));
    validate_operation_ids(values.0, values.1)?;
    Ok(values)
}

pub fn decode_get_config_response(
    payload: &[u8],
) -> Result<ObservedConfiguration, ConfigurationWireError> {
    require_success(payload, GET_CONFIG_RESPONSE_BYTES)?;
    decode_observed(&payload[1..])
}

pub fn encode_get_config_response(
    value: ObservedConfiguration,
) -> Result<[u8; GET_CONFIG_RESPONSE_BYTES], ConfigurationWireError> {
    value.validate()?;
    let mut out = [0; GET_CONFIG_RESPONSE_BYTES];
    out[0] = ResponseStatus::Ok as u8;
    encode_observed(value, &mut out[1..])?;
    Ok(out)
}

pub fn encode_validate_config_response(
    value: ConfigValidationResult,
) -> [u8; VALIDATE_CONFIG_RESPONSE_BYTES] {
    [
        ResponseStatus::Ok as u8,
        value.code as u8,
        value.field as u8,
    ]
}

pub fn decode_validate_config_response(
    payload: &[u8],
) -> Result<ConfigValidationResult, ConfigurationWireError> {
    require_success(payload, VALIDATE_CONFIG_RESPONSE_BYTES)?;
    Ok(ConfigValidationResult {
        code: ConfigValidationCode::from_wire(payload[1])
            .ok_or(ConfigurationWireError::UnknownValidationCode(payload[1]))?,
        field: ConfigField::from_wire(payload[2])
            .ok_or(ConfigurationWireError::UnknownConfigField(payload[2]))?,
    })
}

pub fn decode_config_operation_response(
    payload: &[u8],
) -> Result<ConfigOperationResult, ConfigurationWireError> {
    require_success(payload, CONFIG_OPERATION_RESPONSE_BYTES)?;
    let result = ConfigOperationResult {
        operation_id: read_u32(payload, 1),
        code: ConfigOperationCode::from_wire(payload[5])
            .ok_or(ConfigurationWireError::UnknownOperationCode(payload[5]))?,
        observed: decode_observed(&payload[6..24])?,
        effective_sample_seq: u64::from_le_bytes(payload[24..32].try_into().unwrap()),
    };
    if result.operation_id == 0 {
        return Err(ConfigurationWireError::ZeroOperationId);
    }
    Ok(result)
}

pub fn encode_config_operation_response(
    value: ConfigOperationResult,
) -> Result<[u8; CONFIG_OPERATION_RESPONSE_BYTES], ConfigurationWireError> {
    if value.operation_id == 0 {
        return Err(ConfigurationWireError::ZeroOperationId);
    }
    value.observed.validate()?;
    let mut out = [0; CONFIG_OPERATION_RESPONSE_BYTES];
    out[0] = ResponseStatus::Ok as u8;
    out[1..5].copy_from_slice(&value.operation_id.to_le_bytes());
    out[5] = value.code as u8;
    encode_observed(value.observed, &mut out[6..24])?;
    out[24..32].copy_from_slice(&value.effective_sample_seq.to_le_bytes());
    Ok(out)
}

fn decode_observed(payload: &[u8]) -> Result<ObservedConfiguration, ConfigurationWireError> {
    if payload.len() != OBSERVED_CONFIG_BYTES {
        return Err(ConfigurationWireError::InvalidLength(payload.len()));
    }
    ObservedConfiguration {
        revision: read_u32(payload, 0),
        desired: decode_configuration(&payload[4..16])?,
        sensor_odr: Adxl355Odr::from_register_value(payload[16])
            .ok_or(ConfigurationWireError::UnknownSensorOdr(payload[16]))?,
        decimation_factor: payload[17],
    }
    .validate()
}

fn encode_observed(
    value: ObservedConfiguration,
    output: &mut [u8],
) -> Result<(), ConfigurationWireError> {
    value.validate()?;
    if output.len() != OBSERVED_CONFIG_BYTES {
        return Err(ConfigurationWireError::InvalidLength(output.len()));
    }
    output[..4].copy_from_slice(&value.revision.to_le_bytes());
    output[4..16].copy_from_slice(&encode_configuration(value.desired)?);
    output[16] = value.sensor_odr.register_value();
    output[17] = value.decimation_factor;
    Ok(())
}

fn require_success(payload: &[u8], expected: usize) -> Result<(), ConfigurationWireError> {
    let raw = *payload
        .first()
        .ok_or(ConfigurationWireError::InvalidLength(0))?;
    let status = ResponseStatus::try_from(raw)
        .map_err(|_| ConfigurationWireError::UnknownResponseStatus(raw))?;
    if status != ResponseStatus::Ok {
        if payload.len() != 1 {
            return Err(ConfigurationWireError::InvalidLength(payload.len()));
        }
        return Err(ConfigurationWireError::Remote(status));
    }
    if payload.len() != expected {
        return Err(ConfigurationWireError::InvalidLength(payload.len()));
    }
    Ok(())
}

fn validate_operation_ids(operation_id: u32, revision: u32) -> Result<(), ConfigurationWireError> {
    if operation_id == 0 {
        return Err(ConfigurationWireError::ZeroOperationId);
    }
    if revision == 0 {
        return Err(ConfigurationWireError::ZeroRevision);
    }
    Ok(())
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigurationWireError {
    InvalidLength(usize),
    UnsupportedSchema(u8),
    UnknownOutputRate(u8),
    UnknownSensorOdr(u8),
    UnknownFilterProfile(u8),
    InvalidRange(u8),
    InvalidHighPassCorner(u8),
    InvalidFifoWatermark(u8),
    ZeroOperationId,
    ZeroRevision,
    ZeroDecimationFactor,
    InconsistentObservedRate,
    UnknownResponseStatus(u8),
    Remote(ResponseStatus),
    UnknownValidationCode(u8),
    UnknownConfigField(u8),
    UnknownOperationCode(u8),
}

impl fmt::Display for ConfigurationWireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Configuration wire error: {self:?}")
    }
}
impl std::error::Error for ConfigurationWireError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> MeasurementConfiguration {
        MeasurementConfiguration {
            output_rate: OutputRateCode::Hz125,
            range_g: 2,
            high_pass_corner: 0,
            filter_profile: DecimationFilterProfile::Balanced,
            fifo_watermark: 30,
            offset_x: -12,
            offset_y: 34,
            offset_z: -56,
        }
    }

    #[test]
    fn compact_configuration_round_trips() {
        let encoded = encode_configuration(config()).unwrap();
        assert_eq!(encoded, [1, 5, 2, 0, 1, 30, 0xf4, 0xff, 34, 0, 0xc8, 0xff]);
        assert_eq!(decode_configuration(&encoded).unwrap(), config());
    }

    #[test]
    fn reset_product_default_is_explicit_and_valid() {
        let value = MeasurementConfiguration::product_default();
        assert_eq!(value.output_rate, OutputRateCode::Hz250);
        assert_eq!(value.range_g, 2);
        assert_eq!(value.high_pass_corner, 0);
        assert_eq!(value.filter_profile, DecimationFilterProfile::Balanced);
        assert_eq!(value.fifo_watermark, 30);
        assert_eq!((value.offset_x, value.offset_y, value.offset_z), (0, 0, 0));
        assert_eq!(value.validate(), Ok(value));
    }

    #[test]
    fn apply_is_correlated_and_revision_guarded() {
        let request = ConfigOperationRequest {
            operation_id: 0x12345678,
            expected_revision: 9,
            configuration: config(),
        };
        let encoded = encode_apply_config_request(request).unwrap();
        assert_eq!(encoded.len(), 20);
        assert_eq!(decode_apply_config_request(&encoded).unwrap(), request);
        assert_eq!(
            encode_reset_config_request(0, 9),
            Err(ConfigurationWireError::ZeroOperationId)
        );
        assert_eq!(
            encode_reset_config_request(1, 0),
            Err(ConfigurationWireError::ZeroRevision)
        );
    }

    #[test]
    fn validation_rejects_values_that_firmware_would_normalize() {
        let mut value = config();
        value.fifo_watermark = 31;
        assert_eq!(
            encode_configuration(value),
            Err(ConfigurationWireError::InvalidFifoWatermark(31))
        );
        value = config();
        value.range_g = 3;
        assert_eq!(
            encode_configuration(value),
            Err(ConfigurationWireError::InvalidRange(3))
        );
    }
}
