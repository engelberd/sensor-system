use core::fmt;

use sensor_protocol::{
    decode_config_operation_response, decode_get_config_response, decode_validate_config_response,
    encode_apply_config_request, encode_configuration, encode_reset_config_request,
    ConfigOperationRequest, ConfigOperationResult, ConfigValidationResult, ConfigurationWireError,
    MeasurementConfiguration, ObservedConfiguration, ResponseStatus, APPLY_CONFIG_MESSAGE_ID,
    CONFIGURATION_SERVICE_ID, GET_CONFIG_MESSAGE_ID, RESET_CONFIG_MESSAGE_ID,
    VALIDATE_CONFIG_MESSAGE_ID,
};

use crate::transaction::{DurationMicros, RequestSpec, TimeoutRecovery};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigurationRequest {
    Get,
    Validate(MeasurementConfiguration),
    Apply(ConfigOperationRequest),
    Reset {
        operation_id: u32,
        expected_revision: u32,
    },
}

impl ConfigurationRequest {
    pub const fn operation_id(self) -> Option<u32> {
        match self {
            Self::Apply(value) => Some(value.operation_id),
            Self::Reset { operation_id, .. } => Some(operation_id),
            Self::Get | Self::Validate(_) => None,
        }
    }

    const fn message_id(self) -> u8 {
        match self {
            Self::Get => GET_CONFIG_MESSAGE_ID,
            Self::Validate(_) => VALIDATE_CONFIG_MESSAGE_ID,
            Self::Apply(_) => APPLY_CONFIG_MESSAGE_ID,
            Self::Reset { .. } => RESET_CONFIG_MESSAGE_ID,
        }
    }

    const fn mutates(self) -> bool {
        matches!(self, Self::Apply(_) | Self::Reset { .. })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigurationOutcome {
    NotAccepted,
    Observed(ObservedConfiguration),
    Validated(ConfigValidationResult),
    Applied(ConfigOperationResult),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigurationPhase {
    ReadyToSend,
    AwaitingResponse,
    ReconcileRequired,
    Complete,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigurationAttempt {
    pub request: RequestSpec,
    payload: [u8; sensor_protocol::APPLY_CONFIG_REQUEST_BYTES],
    payload_size: usize,
}

impl ConfigurationAttempt {
    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.payload_size]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigurationOperation {
    node_address: u8,
    request: ConfigurationRequest,
    phase: ConfigurationPhase,
}

impl ConfigurationOperation {
    pub fn new(
        node_address: u8,
        request: ConfigurationRequest,
    ) -> Result<Self, ConfigurationError> {
        if !(1..=253).contains(&node_address) {
            return Err(ConfigurationError::InvalidNodeAddress(node_address));
        }
        validate_request(request)?;
        Ok(Self {
            node_address,
            request,
            phase: ConfigurationPhase::ReadyToSend,
        })
    }

    pub const fn node_address(self) -> u8 {
        self.node_address
    }
    pub const fn request(self) -> ConfigurationRequest {
        self.request
    }
    pub const fn phase(self) -> ConfigurationPhase {
        self.phase
    }

    pub fn begin_attempt(
        &mut self,
        timeout: DurationMicros,
    ) -> Result<ConfigurationAttempt, ConfigurationError> {
        if self.phase != ConfigurationPhase::ReadyToSend
            && self.phase != ConfigurationPhase::ReconcileRequired
        {
            return Err(ConfigurationError::UnexpectedPhase);
        }
        let mut payload = [0; sensor_protocol::APPLY_CONFIG_REQUEST_BYTES];
        let payload_size = match self.request {
            ConfigurationRequest::Get => 0,
            ConfigurationRequest::Validate(value) => {
                let encoded = encode_configuration(value)?;
                payload[..encoded.len()].copy_from_slice(&encoded);
                encoded.len()
            }
            ConfigurationRequest::Apply(value) => {
                let encoded = encode_apply_config_request(value)?;
                payload.copy_from_slice(&encoded);
                encoded.len()
            }
            ConfigurationRequest::Reset {
                operation_id,
                expected_revision,
            } => {
                let encoded = encode_reset_config_request(operation_id, expected_revision)?;
                payload[..encoded.len()].copy_from_slice(&encoded);
                encoded.len()
            }
        };
        self.phase = ConfigurationPhase::AwaitingResponse;
        Ok(ConfigurationAttempt {
            request: RequestSpec {
                node_address: self.node_address,
                service: CONFIGURATION_SERVICE_ID,
                request_message: self.request.message_id(),
                response_message: self.request.message_id(),
                timeout,
                timeout_recovery: if self.request.mutates() {
                    TimeoutRecovery::ReconcileState
                } else {
                    TimeoutRecovery::RetrySafe
                },
            },
            payload,
            payload_size,
        })
    }

    pub fn mark_timeout(&mut self) -> Result<(), ConfigurationError> {
        if self.phase != ConfigurationPhase::AwaitingResponse {
            return Err(ConfigurationError::UnexpectedPhase);
        }
        self.phase = ConfigurationPhase::ReconcileRequired;
        Ok(())
    }

    pub fn accept_response(
        &mut self,
        payload: &[u8],
    ) -> Result<ConfigurationOutcome, ConfigurationError> {
        if self.phase != ConfigurationPhase::AwaitingResponse {
            return Err(ConfigurationError::UnexpectedPhase);
        }
        let decoded = match self.request {
            ConfigurationRequest::Get => {
                decode_get_config_response(payload).map(ConfigurationOutcome::Observed)
            }
            ConfigurationRequest::Validate(_) => {
                decode_validate_config_response(payload).map(ConfigurationOutcome::Validated)
            }
            ConfigurationRequest::Apply(_) | ConfigurationRequest::Reset { .. } => {
                decode_config_operation_response(payload).map(ConfigurationOutcome::Applied)
            }
        };
        match decoded {
            Ok(ConfigurationOutcome::Applied(result))
                if Some(result.operation_id) != self.request.operation_id() =>
            {
                self.phase = ConfigurationPhase::Failed;
                Err(ConfigurationError::OperationIdMismatch {
                    expected: self.request.operation_id().unwrap(),
                    received: result.operation_id,
                })
            }
            Ok(outcome) => {
                self.phase = ConfigurationPhase::Complete;
                Ok(outcome)
            }
            Err(ConfigurationWireError::Remote(ResponseStatus::Busy)) => {
                self.phase = if self.request.mutates() {
                    ConfigurationPhase::ReconcileRequired
                } else {
                    ConfigurationPhase::ReadyToSend
                };
                Ok(ConfigurationOutcome::NotAccepted)
            }
            Err(error) => {
                self.phase = ConfigurationPhase::Failed;
                Err(ConfigurationError::Wire(error))
            }
        }
    }
}

fn validate_request(request: ConfigurationRequest) -> Result<(), ConfigurationError> {
    match request {
        ConfigurationRequest::Get => Ok(()),
        ConfigurationRequest::Validate(value) => {
            encode_configuration(value).map(|_| ()).map_err(Into::into)
        }
        ConfigurationRequest::Apply(value) => encode_apply_config_request(value)
            .map(|_| ())
            .map_err(Into::into),
        ConfigurationRequest::Reset {
            operation_id,
            expected_revision,
        } => encode_reset_config_request(operation_id, expected_revision)
            .map(|_| ())
            .map_err(Into::into),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigurationError {
    InvalidNodeAddress(u8),
    UnexpectedPhase,
    OperationIdMismatch { expected: u32, received: u32 },
    Wire(ConfigurationWireError),
}

impl From<ConfigurationWireError> for ConfigurationError {
    fn from(value: ConfigurationWireError) -> Self {
        Self::Wire(value)
    }
}

impl fmt::Display for ConfigurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "configuration operation error: {self:?}")
    }
}
impl std::error::Error for ConfigurationError {}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_protocol::{
        encode_config_operation_response, Adxl355Odr, ConfigOperationCode, DecimationFilterProfile,
        OutputRateCode,
    };

    fn desired() -> MeasurementConfiguration {
        MeasurementConfiguration {
            output_rate: OutputRateCode::Hz125,
            range_g: 2,
            high_pass_corner: 0,
            filter_profile: DecimationFilterProfile::Balanced,
            fifo_watermark: 30,
            offset_x: 0,
            offset_y: 0,
            offset_z: 0,
        }
    }

    #[test]
    fn apply_reuses_operation_id_after_timeout() {
        let request = ConfigOperationRequest {
            operation_id: 77,
            expected_revision: 3,
            configuration: desired(),
        };
        let mut operation =
            ConfigurationOperation::new(7, ConfigurationRequest::Apply(request)).unwrap();
        let first = operation
            .begin_attempt(DurationMicros::new(100).unwrap())
            .unwrap();
        operation.mark_timeout().unwrap();
        let second = operation
            .begin_attempt(DurationMicros::new(100).unwrap())
            .unwrap();
        assert_eq!(first.payload(), second.payload());

        let result = ConfigOperationResult {
            operation_id: 77,
            code: ConfigOperationCode::Applied,
            observed: ObservedConfiguration {
                revision: 4,
                desired: desired(),
                sensor_odr: Adxl355Odr::Hz250,
                decimation_factor: 2,
            },
            effective_sample_seq: 1000,
        };
        assert_eq!(
            operation
                .accept_response(&encode_config_operation_response(result).unwrap())
                .unwrap(),
            ConfigurationOutcome::Applied(result)
        );
    }
}
