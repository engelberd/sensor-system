use core::fmt;

use sensor_core::BootEpoch;

use crate::{ResponseStatus, ServiceId};

pub const TIME_SERVICE_ID: u8 = ServiceId::Time as u8;
pub const OBSERVE_CLOCK_MESSAGE_ID: u8 = 1;
pub const OBSERVE_CLOCK_REQUEST_BYTES: usize = 0;
pub const OBSERVE_CLOCK_RESPONSE_BYTES: usize = 25;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeExchangeResponse {
    pub boot_epoch: BootEpoch,
    pub node_receive_us: u64,
    pub node_send_us: u64,
}

impl TimeExchangeResponse {
    pub const fn validate(self) -> Result<Self, TimeWireError> {
        if self.boot_epoch.get() == 0 {
            return Err(TimeWireError::ZeroBootEpoch);
        }
        if self.node_send_us < self.node_receive_us {
            return Err(TimeWireError::NodeClockRegressed);
        }
        Ok(self)
    }
}

pub fn validate_time_request(payload: &[u8]) -> Result<(), TimeWireError> {
    if payload.len() == OBSERVE_CLOCK_REQUEST_BYTES {
        Ok(())
    } else {
        Err(TimeWireError::InvalidRequestLength(payload.len()))
    }
}

pub fn encode_time_success(
    response: TimeExchangeResponse,
) -> Result<[u8; OBSERVE_CLOCK_RESPONSE_BYTES], TimeWireError> {
    let response = response.validate()?;
    let mut output = [0_u8; OBSERVE_CLOCK_RESPONSE_BYTES];
    output[0] = ResponseStatus::Ok as u8;
    output[1..9].copy_from_slice(&response.boot_epoch.get().to_le_bytes());
    output[9..17].copy_from_slice(&response.node_receive_us.to_le_bytes());
    output[17..25].copy_from_slice(&response.node_send_us.to_le_bytes());
    Ok(output)
}

pub fn decode_time_response(payload: &[u8]) -> Result<TimeExchangeResponse, TimeWireError> {
    let Some(status) = payload.first().copied() else {
        return Err(TimeWireError::InvalidResponseLength(0));
    };
    let status = ResponseStatus::try_from(status).map_err(TimeWireError::Status)?;
    if status != ResponseStatus::Ok {
        if payload.len() != 1 {
            return Err(TimeWireError::InvalidRemoteErrorLength(payload.len()));
        }
        return Err(TimeWireError::Remote(status));
    }
    if payload.len() != OBSERVE_CLOCK_RESPONSE_BYTES {
        return Err(TimeWireError::InvalidResponseLength(payload.len()));
    }
    TimeExchangeResponse {
        boot_epoch: BootEpoch::new(read_u64(payload, 1)),
        node_receive_us: read_u64(payload, 9),
        node_send_us: read_u64(payload, 17),
    }
    .validate()
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("fixed field"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeWireError {
    InvalidRequestLength(usize),
    InvalidResponseLength(usize),
    InvalidRemoteErrorLength(usize),
    Status(crate::ApplicationContractError),
    Remote(ResponseStatus),
    ZeroBootEpoch,
    NodeClockRegressed,
}

impl fmt::Display for TimeWireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequestLength(length) => {
                write!(formatter, "Time request has invalid length {length}")
            }
            Self::InvalidResponseLength(length) => {
                write!(formatter, "Time response has invalid length {length}")
            }
            Self::InvalidRemoteErrorLength(length) => {
                write!(formatter, "Time error response has invalid length {length}")
            }
            Self::Status(error) => write!(formatter, "invalid Time response status: {error}"),
            Self::Remote(status) => write!(formatter, "Time Service returned {status:?}"),
            Self::ZeroBootEpoch => formatter.write_str("Time response has zero boot epoch"),
            Self::NodeClockRegressed => formatter.write_str("Time response reverses node time"),
        }
    }
}

impl std::error::Error for TimeWireError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_success_payload_round_trips_exactly() {
        let response = TimeExchangeResponse {
            boot_epoch: BootEpoch::new(0x0102_0304_0506_0708),
            node_receive_us: 0x1112_1314_1516_1718,
            node_send_us: 0x2122_2324_2526_2728,
        };
        let encoded = encode_time_success(response).unwrap();
        assert_eq!(
            &encoded,
            include_bytes!("../../../contracts/golden/time_v5_observe_response.bin")
        );
        assert_eq!(decode_time_response(&encoded).unwrap(), response);
    }

    #[test]
    fn request_and_remote_errors_have_one_unambiguous_shape() {
        assert_eq!(validate_time_request(&[]), Ok(()));
        assert_eq!(
            validate_time_request(&[0]),
            Err(TimeWireError::InvalidRequestLength(1))
        );
        assert_eq!(
            decode_time_response(&[ResponseStatus::Busy as u8]),
            Err(TimeWireError::Remote(ResponseStatus::Busy))
        );
        assert_eq!(
            decode_time_response(&[ResponseStatus::Busy as u8, 0]),
            Err(TimeWireError::InvalidRemoteErrorLength(2))
        );
    }

    #[test]
    fn malformed_success_fails_closed() {
        let mut encoded = encode_time_success(TimeExchangeResponse {
            boot_epoch: BootEpoch::new(1),
            node_receive_us: 20,
            node_send_us: 21,
        })
        .unwrap();
        encoded[17..25].copy_from_slice(&19_u64.to_le_bytes());
        assert_eq!(
            decode_time_response(&encoded),
            Err(TimeWireError::NodeClockRegressed)
        );
        encoded[1..9].fill(0);
        assert_eq!(
            decode_time_response(&encoded),
            Err(TimeWireError::ZeroBootEpoch)
        );
    }
}
