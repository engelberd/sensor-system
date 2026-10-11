use core::fmt;

use crate::{ResponseStatus, ServiceId};

pub const LIFECYCLE_SERVICE_ID: u8 = ServiceId::Lifecycle as u8;
pub const GET_RUNTIME_STATE_MESSAGE_ID: u8 = 1;
pub const START_RECORDING_MESSAGE_ID: u8 = 2;
pub const STOP_RECORDING_MESSAGE_ID: u8 = 3;
pub const ENTER_MAINTENANCE_MESSAGE_ID: u8 = 4;
pub const EXIT_MAINTENANCE_MESSAGE_ID: u8 = 5;
pub const SET_OBSERVABILITY_MESSAGE_ID: u8 = 6;
pub const PREPARE_FIRMWARE_UPDATE_MESSAGE_ID: u8 = 7;
pub const CONFIRM_TRIAL_BOOT_MESSAGE_ID: u8 = 8;
pub const LIFECYCLE_OPERATION_REQUEST_BYTES: usize = 4;
pub const LIFECYCLE_PROFILE_REQUEST_BYTES: usize = 5;
pub const LIFECYCLE_RESPONSE_BYTES: usize = 9;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum RuntimeOperation {
    GetState = 0,
    Start = 1,
    Stop = 2,
    EnterMaintenance = 3,
    ExitMaintenance = 4,
    SetObservabilityProfile = 5,
    PrepareFirmwareUpdate = 6,
    ConfirmTrialBoot = 7,
}

impl RuntimeOperation {
    pub const fn message_id(self) -> u8 {
        match self {
            Self::GetState => GET_RUNTIME_STATE_MESSAGE_ID,
            Self::Start => START_RECORDING_MESSAGE_ID,
            Self::Stop => STOP_RECORDING_MESSAGE_ID,
            Self::EnterMaintenance => ENTER_MAINTENANCE_MESSAGE_ID,
            Self::ExitMaintenance => EXIT_MAINTENANCE_MESSAGE_ID,
            Self::SetObservabilityProfile => SET_OBSERVABILITY_MESSAGE_ID,
            Self::PrepareFirmwareUpdate => PREPARE_FIRMWARE_UPDATE_MESSAGE_ID,
            Self::ConfirmTrialBoot => CONFIRM_TRIAL_BOOT_MESSAGE_ID,
        }
    }

    fn from_message(value: u8) -> Result<Self, LifecycleWireError> {
        match value {
            GET_RUNTIME_STATE_MESSAGE_ID => Ok(Self::GetState),
            START_RECORDING_MESSAGE_ID => Ok(Self::Start),
            STOP_RECORDING_MESSAGE_ID => Ok(Self::Stop),
            ENTER_MAINTENANCE_MESSAGE_ID => Ok(Self::EnterMaintenance),
            EXIT_MAINTENANCE_MESSAGE_ID => Ok(Self::ExitMaintenance),
            SET_OBSERVABILITY_MESSAGE_ID => Ok(Self::SetObservabilityProfile),
            PREPARE_FIRMWARE_UPDATE_MESSAGE_ID => Ok(Self::PrepareFirmwareUpdate),
            CONFIRM_TRIAL_BOOT_MESSAGE_ID => Ok(Self::ConfirmTrialBoot),
            _ => Err(LifecycleWireError::UnknownMessage(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum RuntimeState {
    Idle = 0,
    Recording = 1,
    Stopping = 2,
    Maintenance = 3,
    Faulted = 4,
}

impl TryFrom<u8> for RuntimeState {
    type Error = LifecycleWireError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Idle),
            1 => Ok(Self::Recording),
            2 => Ok(Self::Stopping),
            3 => Ok(Self::Maintenance),
            4 => Ok(Self::Faulted),
            _ => Err(LifecycleWireError::UnknownRuntimeState(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ObservabilityProfile {
    Production = 0,
    Diagnostic = 1,
}

impl TryFrom<u8> for ObservabilityProfile {
    type Error = LifecycleWireError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Production),
            1 => Ok(Self::Diagnostic),
            _ => Err(LifecycleWireError::UnknownObservabilityProfile(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum RuntimeResultCode {
    Ok = 0,
    InvalidOperation = 1,
    InvalidState = 2,
    InvalidArgument = 3,
    InProgress = 4,
    TransitionFailed = 5,
    Faulted = 6,
}

impl TryFrom<u8> for RuntimeResultCode {
    type Error = LifecycleWireError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Ok),
            1 => Ok(Self::InvalidOperation),
            2 => Ok(Self::InvalidState),
            3 => Ok(Self::InvalidArgument),
            4 => Ok(Self::InProgress),
            5 => Ok(Self::TransitionFailed),
            6 => Ok(Self::Faulted),
            _ => Err(LifecycleWireError::UnknownResultCode(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SensorStatusCode {
    Ok = 0,
    NotInitialized = 1,
    Busy = 2,
    CommError = 3,
    Timeout = 4,
    InvalidParam = 5,
    NotSupported = 6,
    InvalidDevice = 7,
    NoData = 8,
    InvalidSample = 9,
    InternalError = 10,
}

impl TryFrom<u8> for SensorStatusCode {
    type Error = LifecycleWireError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Ok),
            1 => Ok(Self::NotInitialized),
            2 => Ok(Self::Busy),
            3 => Ok(Self::CommError),
            4 => Ok(Self::Timeout),
            5 => Ok(Self::InvalidParam),
            6 => Ok(Self::NotSupported),
            7 => Ok(Self::InvalidDevice),
            8 => Ok(Self::NoData),
            9 => Ok(Self::InvalidSample),
            10 => Ok(Self::InternalError),
            _ => Err(LifecycleWireError::UnknownSensorStatus(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeCommand {
    pub operation_id: u32,
    pub operation: RuntimeOperation,
    pub requested_profile: ObservabilityProfile,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EncodedRuntimeCommand {
    pub message: u8,
    pub payload: [u8; LIFECYCLE_PROFILE_REQUEST_BYTES],
    pub payload_size: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeResult {
    pub operation_id: u32,
    pub code: RuntimeResultCode,
    pub state: RuntimeState,
    pub observability: ObservabilityProfile,
    pub sensor_status: SensorStatusCode,
}

pub fn encode_lifecycle_request(
    command: RuntimeCommand,
) -> Result<EncodedRuntimeCommand, LifecycleWireError> {
    if command.operation_id == 0 {
        return Err(LifecycleWireError::ZeroOperationId);
    }
    let mut payload = [0; LIFECYCLE_PROFILE_REQUEST_BYTES];
    payload[0..4].copy_from_slice(&command.operation_id.to_le_bytes());
    let payload_size = if command.operation == RuntimeOperation::SetObservabilityProfile {
        payload[4] = command.requested_profile as u8;
        LIFECYCLE_PROFILE_REQUEST_BYTES
    } else {
        LIFECYCLE_OPERATION_REQUEST_BYTES
    };
    Ok(EncodedRuntimeCommand {
        message: command.operation.message_id(),
        payload,
        payload_size,
    })
}

pub fn decode_lifecycle_request(
    message: u8,
    payload: &[u8],
) -> Result<RuntimeCommand, LifecycleWireError> {
    let operation = RuntimeOperation::from_message(message)?;
    let expected = if operation == RuntimeOperation::SetObservabilityProfile {
        LIFECYCLE_PROFILE_REQUEST_BYTES
    } else {
        LIFECYCLE_OPERATION_REQUEST_BYTES
    };
    if payload.len() != expected {
        return Err(LifecycleWireError::InvalidLength);
    }
    let operation_id = u32::from_le_bytes(payload[0..4].try_into().unwrap());
    if operation_id == 0 {
        return Err(LifecycleWireError::ZeroOperationId);
    }
    let requested_profile = if operation == RuntimeOperation::SetObservabilityProfile {
        ObservabilityProfile::try_from(payload[4])?
    } else {
        ObservabilityProfile::Production
    };
    Ok(RuntimeCommand {
        operation_id,
        operation,
        requested_profile,
    })
}

pub fn encode_lifecycle_response(
    result: RuntimeResult,
) -> Result<[u8; LIFECYCLE_RESPONSE_BYTES], LifecycleWireError> {
    if result.operation_id == 0 {
        return Err(LifecycleWireError::ZeroOperationId);
    }
    let mut output = [0; LIFECYCLE_RESPONSE_BYTES];
    output[0] = ResponseStatus::Ok as u8;
    output[1..5].copy_from_slice(&result.operation_id.to_le_bytes());
    output[5] = result.code as u8;
    output[6] = result.state as u8;
    output[7] = result.observability as u8;
    output[8] = result.sensor_status as u8;
    Ok(output)
}

pub fn decode_lifecycle_response(payload: &[u8]) -> Result<RuntimeResult, LifecycleWireError> {
    let raw_status = *payload.first().ok_or(LifecycleWireError::InvalidLength)?;
    let status = ResponseStatus::try_from(raw_status)
        .map_err(|_| LifecycleWireError::UnknownResponseStatus(raw_status))?;
    if status != ResponseStatus::Ok {
        return Err(LifecycleWireError::RemoteError(status));
    }
    if payload.len() != LIFECYCLE_RESPONSE_BYTES {
        return Err(LifecycleWireError::InvalidLength);
    }
    let operation_id = u32::from_le_bytes(payload[1..5].try_into().unwrap());
    if operation_id == 0 {
        return Err(LifecycleWireError::ZeroOperationId);
    }
    Ok(RuntimeResult {
        operation_id,
        code: RuntimeResultCode::try_from(payload[5])?,
        state: RuntimeState::try_from(payload[6])?,
        observability: ObservabilityProfile::try_from(payload[7])?,
        sensor_status: SensorStatusCode::try_from(payload[8])?,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleWireError {
    InvalidLength,
    UnknownMessage(u8),
    ZeroOperationId,
    UnknownResponseStatus(u8),
    RemoteError(ResponseStatus),
    UnknownResultCode(u8),
    UnknownRuntimeState(u8),
    UnknownObservabilityProfile(u8),
    UnknownSensorStatus(u8),
}

impl fmt::Display for LifecycleWireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "lifecycle wire error: {self:?}")
    }
}

impl std::error::Error for LifecycleWireError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_matches_firmware_layout() {
        let encoded = encode_lifecycle_request(RuntimeCommand {
            operation_id: 0x1234_5678,
            operation: RuntimeOperation::SetObservabilityProfile,
            requested_profile: ObservabilityProfile::Diagnostic,
        })
        .unwrap();
        assert_eq!(encoded.message, SET_OBSERVABILITY_MESSAGE_ID);
        assert_eq!(encoded.payload_size, 5);
        assert_eq!(encoded.payload, [0x78, 0x56, 0x34, 0x12, 1]);
        assert_eq!(
            decode_lifecycle_request(encoded.message, &encoded.payload[..encoded.payload_size])
                .unwrap()
                .requested_profile,
            ObservabilityProfile::Diagnostic
        );
    }

    #[test]
    fn response_matches_firmware_layout() {
        let result = RuntimeResult {
            operation_id: 0x1234_5678,
            code: RuntimeResultCode::InProgress,
            state: RuntimeState::Stopping,
            observability: ObservabilityProfile::Diagnostic,
            sensor_status: SensorStatusCode::Ok,
        };
        let encoded = encode_lifecycle_response(result).unwrap();
        assert_eq!(encoded, [0, 0x78, 0x56, 0x34, 0x12, 4, 2, 1, 0]);
        assert_eq!(decode_lifecycle_response(&encoded).unwrap(), result);
    }

    #[test]
    fn remote_busy_is_distinct_from_in_progress() {
        assert_eq!(
            decode_lifecycle_response(&[ResponseStatus::Busy as u8]),
            Err(LifecycleWireError::RemoteError(ResponseStatus::Busy))
        );
    }

    #[test]
    fn firmware_update_request_is_a_plain_correlated_operation() {
        let encoded = encode_lifecycle_request(RuntimeCommand {
            operation_id: 0x1020_3040,
            operation: RuntimeOperation::PrepareFirmwareUpdate,
            requested_profile: ObservabilityProfile::Production,
        })
        .unwrap();
        assert_eq!(encoded.message, PREPARE_FIRMWARE_UPDATE_MESSAGE_ID);
        assert_eq!(encoded.payload_size, LIFECYCLE_OPERATION_REQUEST_BYTES);
        assert_eq!(&encoded.payload[..4], &[0x40, 0x30, 0x20, 0x10]);
        assert_eq!(
            decode_lifecycle_request(encoded.message, &encoded.payload[..4])
                .unwrap()
                .operation,
            RuntimeOperation::PrepareFirmwareUpdate
        );
    }

    #[test]
    fn trial_confirmation_has_its_own_correlated_message() {
        let encoded = encode_lifecycle_request(RuntimeCommand {
            operation_id: 0x5566_7788,
            operation: RuntimeOperation::ConfirmTrialBoot,
            requested_profile: ObservabilityProfile::Production,
        })
        .unwrap();
        assert_eq!(encoded.message, CONFIRM_TRIAL_BOOT_MESSAGE_ID);
        assert_eq!(encoded.payload_size, LIFECYCLE_OPERATION_REQUEST_BYTES);
        assert_eq!(
            decode_lifecycle_request(encoded.message, &encoded.payload[..4])
                .unwrap()
                .operation,
            RuntimeOperation::ConfirmTrialBoot
        );
    }
}
