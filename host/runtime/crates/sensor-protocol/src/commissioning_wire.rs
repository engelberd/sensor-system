use core::fmt;

use sensor_core::HardwareId;

use crate::{crc32c, FirmwareVersion, ProtocolVersion, ResponseStatus, ServiceId};

pub const COMMISSIONING_SERVICE_ID: u8 = ServiceId::Commissioning as u8;
pub const PROBE_SLOT_MESSAGE_ID: u8 = 1;
pub const ASSIGN_ADDRESS_MESSAGE_ID: u8 = 2;
pub const PROBE_SLOT_REQUEST_BYTES: usize = 12;
pub const PROBE_SLOT_RESPONSE_BYTES: usize = 23;
pub const ASSIGN_ADDRESS_REQUEST_BYTES: usize = 21;
pub const ASSIGN_ADDRESS_RESPONSE_BYTES: usize = 14;
pub const MAX_COMMISSIONING_SLOTS: u16 = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbeSlotRequest {
    pub round_nonce: u64,
    pub slot_count: u16,
    pub slot_index: u16,
}

impl ProbeSlotRequest {
    pub fn validate(self) -> Result<Self, CommissioningWireError> {
        if self.round_nonce == 0 {
            return Err(CommissioningWireError::ZeroRoundNonce);
        }
        if self.slot_count == 0
            || self.slot_count > MAX_COMMISSIONING_SLOTS
            || !self.slot_count.is_power_of_two()
        {
            return Err(CommissioningWireError::InvalidSlotCount(self.slot_count));
        }
        if self.slot_index >= self.slot_count {
            return Err(CommissioningWireError::InvalidSlotIndex {
                index: self.slot_index,
                count: self.slot_count,
            });
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommissioningIdentity {
    pub round_nonce: u64,
    pub hardware_id: HardwareId,
    pub board_revision: u8,
    pub firmware: FirmwareVersion,
    pub protocol: ProtocolVersion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssignAddressRequest {
    pub operation_id: u32,
    pub round_nonce: u64,
    pub hardware_id: HardwareId,
    pub node_address: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssignAddressResponse {
    pub operation_id: u32,
    pub hardware_id: HardwareId,
    pub node_address: u8,
}

pub fn commissioning_slot(
    hardware_id: HardwareId,
    round_nonce: u64,
    slot_count: u16,
) -> Result<u16, CommissioningWireError> {
    ProbeSlotRequest {
        round_nonce,
        slot_count,
        slot_index: 0,
    }
    .validate()?;
    let mut key = [0_u8; 16];
    key[..8].copy_from_slice(&round_nonce.to_le_bytes());
    key[8..].copy_from_slice(&hardware_id.bytes());
    Ok((crc32c(&key) % u32::from(slot_count)) as u16)
}

pub fn encode_probe_slot_request(
    request: ProbeSlotRequest,
) -> Result<[u8; PROBE_SLOT_REQUEST_BYTES], CommissioningWireError> {
    request.validate()?;
    let mut out = [0; PROBE_SLOT_REQUEST_BYTES];
    out[..8].copy_from_slice(&request.round_nonce.to_le_bytes());
    out[8..10].copy_from_slice(&request.slot_count.to_le_bytes());
    out[10..12].copy_from_slice(&request.slot_index.to_le_bytes());
    Ok(out)
}

pub fn decode_probe_slot_request(
    payload: &[u8],
) -> Result<ProbeSlotRequest, CommissioningWireError> {
    if payload.len() != PROBE_SLOT_REQUEST_BYTES {
        return Err(CommissioningWireError::InvalidLength(payload.len()));
    }
    ProbeSlotRequest {
        round_nonce: u64::from_le_bytes(payload[..8].try_into().unwrap()),
        slot_count: u16::from_le_bytes(payload[8..10].try_into().unwrap()),
        slot_index: u16::from_le_bytes(payload[10..12].try_into().unwrap()),
    }
    .validate()
}

pub fn decode_probe_slot_response(
    payload: &[u8],
) -> Result<CommissioningIdentity, CommissioningWireError> {
    require_success(payload, PROBE_SLOT_RESPONSE_BYTES)?;
    let value = CommissioningIdentity {
        round_nonce: u64::from_le_bytes(payload[1..9].try_into().unwrap()),
        hardware_id: HardwareId::from_bytes(payload[9..17].try_into().unwrap()),
        board_revision: payload[17],
        firmware: FirmwareVersion {
            major: payload[18],
            minor: payload[19],
            patch: payload[20],
        },
        protocol: ProtocolVersion::new(payload[21], payload[22]),
    };
    if value.round_nonce == 0 {
        return Err(CommissioningWireError::ZeroRoundNonce);
    }
    if value.board_revision == 0 {
        return Err(CommissioningWireError::InvalidBoardRevision);
    }
    Ok(value)
}

pub fn encode_probe_slot_response(
    value: CommissioningIdentity,
) -> Result<[u8; PROBE_SLOT_RESPONSE_BYTES], CommissioningWireError> {
    if value.round_nonce == 0 {
        return Err(CommissioningWireError::ZeroRoundNonce);
    }
    if value.board_revision == 0 {
        return Err(CommissioningWireError::InvalidBoardRevision);
    }
    let mut out = [0; PROBE_SLOT_RESPONSE_BYTES];
    out[0] = ResponseStatus::Ok as u8;
    out[1..9].copy_from_slice(&value.round_nonce.to_le_bytes());
    out[9..17].copy_from_slice(&value.hardware_id.bytes());
    out[17] = value.board_revision;
    out[18] = value.firmware.major;
    out[19] = value.firmware.minor;
    out[20] = value.firmware.patch;
    out[21] = value.protocol.framing;
    out[22] = value.protocol.application;
    Ok(out)
}

pub fn encode_assign_address_request(
    request: AssignAddressRequest,
) -> Result<[u8; ASSIGN_ADDRESS_REQUEST_BYTES], CommissioningWireError> {
    validate_assignment(
        request.operation_id,
        request.round_nonce,
        request.node_address,
    )?;
    let mut out = [0; ASSIGN_ADDRESS_REQUEST_BYTES];
    out[..4].copy_from_slice(&request.operation_id.to_le_bytes());
    out[4..12].copy_from_slice(&request.round_nonce.to_le_bytes());
    out[12..20].copy_from_slice(&request.hardware_id.bytes());
    out[20] = request.node_address;
    Ok(out)
}

pub fn decode_assign_address_request(
    payload: &[u8],
) -> Result<AssignAddressRequest, CommissioningWireError> {
    if payload.len() != ASSIGN_ADDRESS_REQUEST_BYTES {
        return Err(CommissioningWireError::InvalidLength(payload.len()));
    }
    let value = AssignAddressRequest {
        operation_id: u32::from_le_bytes(payload[..4].try_into().unwrap()),
        round_nonce: u64::from_le_bytes(payload[4..12].try_into().unwrap()),
        hardware_id: HardwareId::from_bytes(payload[12..20].try_into().unwrap()),
        node_address: payload[20],
    };
    validate_assignment(value.operation_id, value.round_nonce, value.node_address)?;
    Ok(value)
}

pub fn decode_assign_address_response(
    payload: &[u8],
) -> Result<AssignAddressResponse, CommissioningWireError> {
    require_success(payload, ASSIGN_ADDRESS_RESPONSE_BYTES)?;
    let value = AssignAddressResponse {
        operation_id: u32::from_le_bytes(payload[1..5].try_into().unwrap()),
        hardware_id: HardwareId::from_bytes(payload[5..13].try_into().unwrap()),
        node_address: payload[13],
    };
    validate_assignment(value.operation_id, 1, value.node_address)?;
    Ok(value)
}

pub fn encode_assign_address_response(
    value: AssignAddressResponse,
) -> Result<[u8; ASSIGN_ADDRESS_RESPONSE_BYTES], CommissioningWireError> {
    validate_assignment(value.operation_id, 1, value.node_address)?;
    let mut out = [0; ASSIGN_ADDRESS_RESPONSE_BYTES];
    out[0] = ResponseStatus::Ok as u8;
    out[1..5].copy_from_slice(&value.operation_id.to_le_bytes());
    out[5..13].copy_from_slice(&value.hardware_id.bytes());
    out[13] = value.node_address;
    Ok(out)
}

fn validate_assignment(
    operation_id: u32,
    round_nonce: u64,
    address: u8,
) -> Result<(), CommissioningWireError> {
    if operation_id == 0 {
        return Err(CommissioningWireError::ZeroOperationId);
    }
    if round_nonce == 0 {
        return Err(CommissioningWireError::ZeroRoundNonce);
    }
    if !(1..=253).contains(&address) {
        return Err(CommissioningWireError::InvalidNodeAddress(address));
    }
    Ok(())
}

fn require_success(payload: &[u8], expected: usize) -> Result<(), CommissioningWireError> {
    let raw = *payload
        .first()
        .ok_or(CommissioningWireError::InvalidLength(0))?;
    let status = ResponseStatus::try_from(raw)
        .map_err(|_| CommissioningWireError::UnknownResponseStatus(raw))?;
    if status != ResponseStatus::Ok {
        if payload.len() != 1 {
            return Err(CommissioningWireError::InvalidLength(payload.len()));
        }
        return Err(CommissioningWireError::Remote(status));
    }
    if payload.len() != expected {
        return Err(CommissioningWireError::InvalidLength(payload.len()));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommissioningWireError {
    InvalidLength(usize),
    ZeroRoundNonce,
    InvalidSlotCount(u16),
    InvalidSlotIndex { index: u16, count: u16 },
    ZeroOperationId,
    InvalidNodeAddress(u8),
    InvalidBoardRevision,
    UnknownResponseStatus(u8),
    Remote(ResponseStatus),
}

impl fmt::Display for CommissioningWireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Commissioning wire error: {self:?}")
    }
}
impl std::error::Error for CommissioningWireError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_probe_is_bounded_and_deterministic() {
        let hardware = HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);
        let a = commissioning_slot(hardware, 0x0102030405060708, 16).unwrap();
        let b = commissioning_slot(hardware, 0x0102030405060708, 16).unwrap();
        assert_eq!(a, b);
        assert!(a < 16);
        assert!(commissioning_slot(hardware, 1, 3).is_err());
    }

    #[test]
    fn assignment_targets_exact_hardware_and_a_regular_address() {
        let request = AssignAddressRequest {
            operation_id: 7,
            round_nonce: 9,
            hardware_id: HardwareId::from_bytes([8, 7, 6, 5, 4, 3, 2, 1]),
            node_address: 42,
        };
        let encoded = encode_assign_address_request(request).unwrap();
        assert_eq!(decode_assign_address_request(&encoded).unwrap(), request);
        assert_eq!(
            encode_assign_address_request(AssignAddressRequest {
                node_address: 254,
                ..request
            }),
            Err(CommissioningWireError::InvalidNodeAddress(254))
        );
    }

    #[test]
    fn probes_reject_ambiguous_rounds() {
        assert_eq!(
            encode_probe_slot_request(ProbeSlotRequest {
                round_nonce: 0,
                slot_count: 16,
                slot_index: 0
            }),
            Err(CommissioningWireError::ZeroRoundNonce)
        );
        assert!(encode_probe_slot_request(ProbeSlotRequest {
            round_nonce: 1,
            slot_count: 16,
            slot_index: 16
        })
        .is_err());
    }
}
