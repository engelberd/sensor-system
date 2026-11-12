use core::fmt;

use sensor_protocol::{
    decode_assign_address_response, decode_probe_slot_response, encode_assign_address_request,
    encode_probe_slot_request, AssignAddressRequest, AssignAddressResponse, CommissioningIdentity,
    CommissioningWireError, ProbeSlotRequest, ResponseStatus, ASSIGN_ADDRESS_MESSAGE_ID,
    BROADCAST_ADDRESS, COMMISSIONING_ADDRESS, COMMISSIONING_SERVICE_ID, PROBE_SLOT_MESSAGE_ID,
};

use crate::transaction::{DurationMicros, RoutedRequestSpec, TimeoutRecovery};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommissioningRequest {
    Probe(ProbeSlotRequest),
    Assign(AssignAddressRequest),
}

impl CommissioningRequest {
    pub const fn operation_id(self) -> Option<u32> {
        match self {
            Self::Probe(_) => None,
            Self::Assign(value) => Some(value.operation_id),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommissioningOutcome {
    NotAccepted,
    Discovered(CommissioningIdentity),
    Assigned(AssignAddressResponse),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommissioningPhase {
    ReadyToSend,
    AwaitingResponse,
    ReconcileRequired,
    Complete,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommissioningAttempt {
    pub request: RoutedRequestSpec,
    payload: [u8; sensor_protocol::ASSIGN_ADDRESS_REQUEST_BYTES],
    payload_size: usize,
}

impl CommissioningAttempt {
    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.payload_size]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommissioningOperation {
    request: CommissioningRequest,
    phase: CommissioningPhase,
}

impl CommissioningOperation {
    pub fn new(request: CommissioningRequest) -> Result<Self, CommissioningError> {
        validate_request(request)?;
        Ok(Self {
            request,
            phase: CommissioningPhase::ReadyToSend,
        })
    }

    pub const fn request(self) -> CommissioningRequest {
        self.request
    }
    pub const fn phase(self) -> CommissioningPhase {
        self.phase
    }

    pub fn begin_attempt(
        &mut self,
        timeout: DurationMicros,
    ) -> Result<CommissioningAttempt, CommissioningError> {
        if self.phase != CommissioningPhase::ReadyToSend
            && self.phase != CommissioningPhase::ReconcileRequired
        {
            return Err(CommissioningError::UnexpectedPhase);
        }
        let mut payload = [0; sensor_protocol::ASSIGN_ADDRESS_REQUEST_BYTES];
        let (message, payload_size, recovery) = match self.request {
            CommissioningRequest::Probe(value) => {
                let encoded = encode_probe_slot_request(value)?;
                payload[..encoded.len()].copy_from_slice(&encoded);
                (
                    PROBE_SLOT_MESSAGE_ID,
                    encoded.len(),
                    TimeoutRecovery::RetrySafe,
                )
            }
            CommissioningRequest::Assign(value) => {
                let encoded = encode_assign_address_request(value)?;
                payload.copy_from_slice(&encoded);
                (
                    ASSIGN_ADDRESS_MESSAGE_ID,
                    encoded.len(),
                    TimeoutRecovery::ReconcileState,
                )
            }
        };
        self.phase = CommissioningPhase::AwaitingResponse;
        Ok(CommissioningAttempt {
            request: RoutedRequestSpec {
                request_destination: BROADCAST_ADDRESS,
                response_source: COMMISSIONING_ADDRESS,
                service: COMMISSIONING_SERVICE_ID,
                request_message: message,
                response_message: message,
                timeout,
                timeout_recovery: recovery,
            },
            payload,
            payload_size,
        })
    }

    pub fn mark_timeout(&mut self) -> Result<(), CommissioningError> {
        if self.phase != CommissioningPhase::AwaitingResponse {
            return Err(CommissioningError::UnexpectedPhase);
        }
        self.phase = match self.request {
            CommissioningRequest::Probe(_) => CommissioningPhase::Complete,
            CommissioningRequest::Assign(_) => CommissioningPhase::ReconcileRequired,
        };
        Ok(())
    }

    pub fn mark_transmit_failed(&mut self) -> Result<(), CommissioningError> {
        if self.phase != CommissioningPhase::AwaitingResponse {
            return Err(CommissioningError::UnexpectedPhase);
        }
        self.phase = match self.request {
            CommissioningRequest::Probe(_) => CommissioningPhase::ReadyToSend,
            CommissioningRequest::Assign(_) => CommissioningPhase::ReconcileRequired,
        };
        Ok(())
    }

    pub fn accept_response(
        &mut self,
        payload: &[u8],
    ) -> Result<CommissioningOutcome, CommissioningError> {
        if self.phase != CommissioningPhase::AwaitingResponse {
            return Err(CommissioningError::UnexpectedPhase);
        }
        let outcome = match self.request {
            CommissioningRequest::Probe(expected) => {
                let identity = decode_probe_slot_response(payload)?;
                if identity.round_nonce != expected.round_nonce {
                    return self.fail(CommissioningError::RoundNonceMismatch);
                }
                CommissioningOutcome::Discovered(identity)
            }
            CommissioningRequest::Assign(expected) => {
                let assigned = match decode_assign_address_response(payload) {
                    Ok(value) => value,
                    Err(CommissioningWireError::Remote(ResponseStatus::Busy)) => {
                        self.phase = CommissioningPhase::ReconcileRequired;
                        return Ok(CommissioningOutcome::NotAccepted);
                    }
                    Err(error) => return self.fail(error.into()),
                };
                if assigned.operation_id != expected.operation_id {
                    return self.fail(CommissioningError::OperationIdMismatch);
                }
                if assigned.hardware_id != expected.hardware_id
                    || assigned.node_address != expected.node_address
                {
                    return self.fail(CommissioningError::AssignmentMismatch);
                }
                CommissioningOutcome::Assigned(assigned)
            }
        };
        self.phase = CommissioningPhase::Complete;
        Ok(outcome)
    }

    fn fail<T>(&mut self, error: CommissioningError) -> Result<T, CommissioningError> {
        self.phase = CommissioningPhase::Failed;
        Err(error)
    }
}

fn validate_request(request: CommissioningRequest) -> Result<(), CommissioningError> {
    match request {
        CommissioningRequest::Probe(value) => encode_probe_slot_request(value)
            .map(|_| ())
            .map_err(Into::into),
        CommissioningRequest::Assign(value) => encode_assign_address_request(value)
            .map(|_| ())
            .map_err(Into::into),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommissioningError {
    UnexpectedPhase,
    RoundNonceMismatch,
    OperationIdMismatch,
    AssignmentMismatch,
    Wire(CommissioningWireError),
}

impl From<CommissioningWireError> for CommissioningError {
    fn from(value: CommissioningWireError) -> Self {
        Self::Wire(value)
    }
}
impl fmt::Display for CommissioningError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "commissioning operation error: {self:?}")
    }
}
impl std::error::Error for CommissioningError {}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_core::HardwareId;
    use sensor_protocol::{encode_probe_slot_response, FirmwareVersion, LOCAL_PROTOCOL_VERSION};

    #[test]
    fn probe_uses_broadcast_but_expects_only_commissioning_source() {
        let request = ProbeSlotRequest {
            round_nonce: 7,
            slot_count: 16,
            slot_index: 3,
        };
        let mut operation =
            CommissioningOperation::new(CommissioningRequest::Probe(request)).unwrap();
        let attempt = operation
            .begin_attempt(DurationMicros::new(100).unwrap())
            .unwrap();
        assert_eq!(attempt.request.request_destination, BROADCAST_ADDRESS);
        assert_eq!(attempt.request.response_source, COMMISSIONING_ADDRESS);
        let identity = CommissioningIdentity {
            round_nonce: 7,
            hardware_id: HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]),
            board_revision: 2,
            firmware: FirmwareVersion {
                major: 0,
                minor: 5,
                patch: 7,
            },
            protocol: LOCAL_PROTOCOL_VERSION,
        };
        assert_eq!(
            operation
                .accept_response(&encode_probe_slot_response(identity).unwrap())
                .unwrap(),
            CommissioningOutcome::Discovered(identity)
        );
    }
}
