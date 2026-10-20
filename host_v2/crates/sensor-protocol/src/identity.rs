use core::fmt;

use sensor_core::{BootEpoch, HardwareId, SampleRate};

use crate::{
    ProtocolVersion, ResponseStatus, ServiceId, LOCAL_PROTOCOL_VERSION, MAX_PAYLOAD_BYTES,
};

pub const IDENTITY_SERVICE_ID: u8 = ServiceId::Identity as u8;
pub const GET_IDENTITY_MESSAGE_ID: u8 = 1;
pub const GET_CAPABILITIES_MESSAGE_ID: u8 = 2;
pub const IDENTITY_RESPONSE_BYTES: usize = 24;
pub const CAPABILITIES_RESPONSE_BYTES: usize = 24;
pub const CAPABILITIES_SCHEMA_VERSION: u8 = 2;
pub const KNOWN_OUTPUT_RATE_MASK: u16 = 0x07ff;
pub const IMPLEMENTED_OUTPUT_RATE_MASK: u16 = 0x007e;
pub const KNOWN_SERVICE_FLAGS: u32 = 0x0000_01ff;

pub const FEATURE_SAMPLE_JOURNAL: u64 = 1 << 0;
pub const FEATURE_DRDY_TIMESTAMPS: u64 = 1 << 1;
pub const FEATURE_ATOMIC_CONFIG: u64 = 1 << 2;
pub const FEATURE_TRIAL_BOOT_CONFIRMATION: u64 = 1 << 3;
pub const FEATURE_REMOTE_FIRMWARE_UPDATE: u64 = 1 << 4;
pub const KNOWN_FEATURE_FLAGS: u64 = FEATURE_SAMPLE_JOURNAL
    | FEATURE_DRDY_TIMESTAMPS
    | FEATURE_ATOMIC_CONFIG
    | FEATURE_TRIAL_BOOT_CONFIRMATION
    | FEATURE_REMOTE_FIRMWARE_UPDATE;

/// Stable bit positions used by the capabilities output-rate bitmap.
///
/// These values describe the rate of samples published by Measurement Service,
/// after every mandatory processing and decimation stage. They do not expose
/// the ADXL355 register value or its internal sensor ODR.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum OutputRateCode {
    Hz4000 = 0,
    Hz2000 = 1,
    Hz1000 = 2,
    Hz500 = 3,
    Hz250 = 4,
    Hz125 = 5,
    Hz62_5 = 6,
    Hz31_25 = 7,
    Hz15_625 = 8,
    Hz7_8125 = 9,
    Hz3_90625 = 10,
}

impl OutputRateCode {
    pub const ALL: [Self; 11] = [
        Self::Hz4000,
        Self::Hz2000,
        Self::Hz1000,
        Self::Hz500,
        Self::Hz250,
        Self::Hz125,
        Self::Hz62_5,
        Self::Hz31_25,
        Self::Hz15_625,
        Self::Hz7_8125,
        Self::Hz3_90625,
    ];

    pub const fn capability_bit(self) -> u16 {
        1u16 << self as u8
    }

    pub const fn sample_rate(self) -> SampleRate {
        match self {
            Self::Hz4000 => SampleRate::HZ_4000,
            Self::Hz2000 => SampleRate::HZ_2000,
            Self::Hz1000 => SampleRate::HZ_1000,
            Self::Hz500 => SampleRate::HZ_500,
            Self::Hz250 => SampleRate::HZ_250,
            Self::Hz125 => SampleRate::HZ_125,
            Self::Hz62_5 => SampleRate::HZ_62_5,
            Self::Hz31_25 => SampleRate::HZ_31_25,
            Self::Hz15_625 => SampleRate::HZ_15_625,
            Self::Hz7_8125 => SampleRate::HZ_7_8125,
            Self::Hz3_90625 => SampleRate::HZ_3_90625,
        }
    }

    pub const fn hertz_label(self) -> &'static str {
        match self {
            Self::Hz4000 => "4000",
            Self::Hz2000 => "2000",
            Self::Hz1000 => "1000",
            Self::Hz500 => "500",
            Self::Hz250 => "250",
            Self::Hz125 => "125",
            Self::Hz62_5 => "62.5",
            Self::Hz31_25 => "31.25",
            Self::Hz15_625 => "15.625",
            Self::Hz7_8125 => "7.8125",
            Self::Hz3_90625 => "3.90625",
        }
    }

    pub const fn from_wire(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Hz4000),
            1 => Some(Self::Hz2000),
            2 => Some(Self::Hz1000),
            3 => Some(Self::Hz500),
            4 => Some(Self::Hz250),
            5 => Some(Self::Hz125),
            6 => Some(Self::Hz62_5),
            7 => Some(Self::Hz31_25),
            8 => Some(Self::Hz15_625),
            9 => Some(Self::Hz7_8125),
            10 => Some(Self::Hz3_90625),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirmwareVersion {
    pub major: u8,
    pub minor: u8,
    pub patch: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeIdentity {
    pub hardware_id: HardwareId,
    pub boot_epoch: BootEpoch,
    pub node_address: u8,
    pub board_revision: u8,
    pub firmware: FirmwareVersion,
    pub protocol: ProtocolVersion,
}

impl NodeIdentity {
    pub fn encode_success(self) -> Result<[u8; IDENTITY_RESPONSE_BYTES], IdentityContractError> {
        self.validate()?;
        let mut output = [0; IDENTITY_RESPONSE_BYTES];
        output[0] = ResponseStatus::Ok as u8;
        output[1..9].copy_from_slice(&self.hardware_id.bytes());
        output[9..17].copy_from_slice(&self.boot_epoch.get().to_le_bytes());
        output[17] = self.node_address;
        output[18] = self.board_revision;
        output[19] = self.firmware.major;
        output[20] = self.firmware.minor;
        output[21] = self.firmware.patch;
        output[22] = self.protocol.framing;
        output[23] = self.protocol.application;
        Ok(output)
    }

    pub fn decode(payload: &[u8]) -> Result<Self, IdentityContractError> {
        Self::decode_fields(payload)
    }

    fn decode_fields(payload: &[u8]) -> Result<Self, IdentityContractError> {
        let status = decode_status(payload)?;
        if status != ResponseStatus::Ok {
            return Err(IdentityContractError::RemoteError(status));
        }
        if payload.len() != IDENTITY_RESPONSE_BYTES {
            return Err(IdentityContractError::InvalidPayloadLength);
        }
        let identity = Self {
            hardware_id: HardwareId::from_bytes(payload[1..9].try_into().unwrap()),
            boot_epoch: BootEpoch::new(u64::from_le_bytes(payload[9..17].try_into().unwrap())),
            node_address: payload[17],
            board_revision: payload[18],
            firmware: FirmwareVersion {
                major: payload[19],
                minor: payload[20],
                patch: payload[21],
            },
            protocol: ProtocolVersion::new(payload[22], payload[23]),
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn validate(self) -> Result<Self, IdentityContractError> {
        if self.boot_epoch.get() == 0 {
            return Err(IdentityContractError::ZeroBootEpoch);
        }
        if !(1..=253).contains(&self.node_address) {
            return Err(IdentityContractError::InvalidNodeAddress);
        }
        if self.board_revision == 0 {
            return Err(IdentityContractError::InvalidBoardRevision);
        }
        Ok(self)
    }

    pub fn require_local_protocol(self) -> Result<Self, IdentityContractError> {
        LOCAL_PROTOCOL_VERSION
            .check_compatible(self.protocol)
            .map_err(|_| IdentityContractError::ProtocolMismatch)?;
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeCapabilities {
    pub schema_version: u8,
    pub service_flags: u32,
    pub feature_flags: u64,
    pub max_payload_bytes: u16,
    pub journal_capacity_samples: u32,
    pub output_rate_mask: u16,
    pub max_sample_chunk_samples: u16,
}

impl NodeCapabilities {
    pub fn supports_service(self, service: ServiceId) -> bool {
        self.service_flags & service.capability_bit() != 0
    }

    pub const fn supports_output_rate(self, rate: OutputRateCode) -> bool {
        self.output_rate_mask & rate.capability_bit() != 0
    }

    pub fn encode_success(
        self,
    ) -> Result<[u8; CAPABILITIES_RESPONSE_BYTES], IdentityContractError> {
        self.validate()?;
        let mut output = [0; CAPABILITIES_RESPONSE_BYTES];
        output[0] = ResponseStatus::Ok as u8;
        output[1] = self.schema_version;
        output[2..6].copy_from_slice(&self.service_flags.to_le_bytes());
        output[6..14].copy_from_slice(&self.feature_flags.to_le_bytes());
        output[14..16].copy_from_slice(&self.max_payload_bytes.to_le_bytes());
        output[16..20].copy_from_slice(&self.journal_capacity_samples.to_le_bytes());
        output[20..22].copy_from_slice(&self.output_rate_mask.to_le_bytes());
        output[22..24].copy_from_slice(&self.max_sample_chunk_samples.to_le_bytes());
        Ok(output)
    }

    pub fn decode(payload: &[u8]) -> Result<Self, IdentityContractError> {
        Self::decode_fields(payload)?.validate()
    }

    /// Compatibility decoder used only to enter maintenance on the released
    /// 0.5.4 image before replacing it with schema 2 firmware.
    pub fn decode_schema_1_for_migration(payload: &[u8]) -> Result<Self, IdentityContractError> {
        let capabilities = Self::decode_fields(payload)?;
        if capabilities.schema_version != 1 {
            return Err(IdentityContractError::UnsupportedCapabilitiesSchema);
        }
        capabilities.validate_payload_contract(false)
    }

    fn decode_fields(payload: &[u8]) -> Result<Self, IdentityContractError> {
        let status = decode_status(payload)?;
        if status != ResponseStatus::Ok {
            return Err(IdentityContractError::RemoteError(status));
        }
        if payload.len() != CAPABILITIES_RESPONSE_BYTES {
            return Err(IdentityContractError::InvalidPayloadLength);
        }
        Ok(Self {
            schema_version: payload[1],
            service_flags: u32::from_le_bytes(payload[2..6].try_into().unwrap()),
            feature_flags: u64::from_le_bytes(payload[6..14].try_into().unwrap()),
            max_payload_bytes: u16::from_le_bytes(payload[14..16].try_into().unwrap()),
            journal_capacity_samples: u32::from_le_bytes(payload[16..20].try_into().unwrap()),
            output_rate_mask: u16::from_le_bytes(payload[20..22].try_into().unwrap()),
            max_sample_chunk_samples: u16::from_le_bytes(payload[22..24].try_into().unwrap()),
        })
    }

    pub fn validate(self) -> Result<Self, IdentityContractError> {
        if self.schema_version != CAPABILITIES_SCHEMA_VERSION {
            return Err(IdentityContractError::UnsupportedCapabilitiesSchema);
        }
        self.validate_payload_contract(true)
    }

    fn validate_payload_contract(
        self,
        require_update_feature_pair: bool,
    ) -> Result<Self, IdentityContractError> {
        if self.max_payload_bytes == 0 || usize::from(self.max_payload_bytes) > MAX_PAYLOAD_BYTES {
            return Err(IdentityContractError::InvalidMaxPayload);
        }
        if self.service_flags & !KNOWN_SERVICE_FLAGS != 0 {
            return Err(IdentityContractError::UnknownServiceBits);
        }
        if !self.supports_service(ServiceId::Identity) {
            return Err(IdentityContractError::MissingIdentityService);
        }
        if self.feature_flags & !KNOWN_FEATURE_FLAGS != 0 {
            return Err(IdentityContractError::UnknownFeatureBits);
        }
        let trial_confirmation = self.feature_flags & FEATURE_TRIAL_BOOT_CONFIRMATION != 0;
        let remote_update = self.feature_flags & FEATURE_REMOTE_FIRMWARE_UPDATE != 0;
        if require_update_feature_pair && trial_confirmation != remote_update {
            return Err(IdentityContractError::InvalidFeatureCombination);
        }
        if self.output_rate_mask & !KNOWN_OUTPUT_RATE_MASK != 0 {
            return Err(IdentityContractError::UnknownOutputRateBits);
        }
        if self.supports_service(ServiceId::Measurement)
            && (self.feature_flags & FEATURE_SAMPLE_JOURNAL == 0
                || self.journal_capacity_samples == 0
                || self.output_rate_mask == 0
                || self.max_sample_chunk_samples == 0)
        {
            return Err(IdentityContractError::IncompleteMeasurementCapabilities);
        }
        let chunk_bytes = crate::measurement_wire::CHUNK_FIXED_BYTES
            .checked_add(
                usize::from(self.max_sample_chunk_samples)
                    .saturating_mul(crate::XYZ_I24_WIRE_SAMPLE_BYTES),
            )
            .ok_or(IdentityContractError::InvalidMaxSampleChunk)?;
        if self.supports_service(ServiceId::Measurement)
            && (usize::from(self.max_sample_chunk_samples)
                > crate::measurement_wire::MAX_WIRE_CHUNK_SAMPLES
                || u32::from(self.max_sample_chunk_samples) > self.journal_capacity_samples
                || chunk_bytes > usize::from(self.max_payload_bytes))
        {
            return Err(IdentityContractError::InvalidMaxSampleChunk);
        }
        Ok(self)
    }
}

fn decode_status(payload: &[u8]) -> Result<ResponseStatus, IdentityContractError> {
    let value = *payload
        .first()
        .ok_or(IdentityContractError::InvalidPayloadLength)?;
    ResponseStatus::try_from(value).map_err(|_| IdentityContractError::UnknownResponseStatus(value))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityContractError {
    InvalidPayloadLength,
    UnknownResponseStatus(u8),
    RemoteError(ResponseStatus),
    ZeroBootEpoch,
    InvalidNodeAddress,
    InvalidBoardRevision,
    ProtocolMismatch,
    UnsupportedCapabilitiesSchema,
    InvalidMaxPayload,
    UnknownServiceBits,
    MissingIdentityService,
    UnknownFeatureBits,
    InvalidFeatureCombination,
    UnknownOutputRateBits,
    IncompleteMeasurementCapabilities,
    InvalidMaxSampleChunk,
}

impl fmt::Display for IdentityContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidPayloadLength => "identity payload has an invalid length",
            Self::UnknownResponseStatus(_) => "identity payload has an unknown response status",
            Self::RemoteError(_) => "node returned an application error",
            Self::ZeroBootEpoch => "node boot epoch must be non-zero",
            Self::InvalidNodeAddress => "identity contains an invalid addressed node",
            Self::InvalidBoardRevision => "board revision must be non-zero",
            Self::ProtocolMismatch => "node protocol is incompatible with the host",
            Self::UnsupportedCapabilitiesSchema => "capabilities schema is unsupported",
            Self::InvalidMaxPayload => "capability max payload is outside framing limits",
            Self::UnknownServiceBits => "capabilities contain undefined service bits",
            Self::MissingIdentityService => "capabilities omit the identity service",
            Self::UnknownFeatureBits => "capabilities contain undefined feature bits",
            Self::InvalidFeatureCombination => {
                "capabilities contain an inconsistent feature combination"
            }
            Self::UnknownOutputRateBits => "capabilities contain undefined output-rate bits",
            Self::IncompleteMeasurementCapabilities => {
                "measurement service lacks required bounded-journal capabilities"
            }
            Self::InvalidMaxSampleChunk => {
                "maximum sample chunk does not fit the journal or payload"
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for IdentityContractError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> NodeIdentity {
        NodeIdentity {
            hardware_id: HardwareId::from_bytes([0x2f, 0x13, 0x34, 0x94, 0x50, 0xfa, 0x40, 0x84]),
            boot_epoch: BootEpoch::new(0x1122_3344_5566_7788),
            node_address: 7,
            board_revision: 2,
            firmware: FirmwareVersion {
                major: 0,
                minor: 5,
                patch: 0,
            },
            protocol: LOCAL_PROTOCOL_VERSION,
        }
    }

    fn capabilities() -> NodeCapabilities {
        NodeCapabilities {
            schema_version: CAPABILITIES_SCHEMA_VERSION,
            service_flags: ServiceId::Identity.capability_bit()
                | ServiceId::Measurement.capability_bit()
                | ServiceId::Time.capability_bit(),
            feature_flags: FEATURE_SAMPLE_JOURNAL | FEATURE_DRDY_TIMESTAMPS,
            max_payload_bytes: 512,
            journal_capacity_samples: 4096,
            output_rate_mask: IMPLEMENTED_OUTPUT_RATE_MASK,
            max_sample_chunk_samples: 48,
        }
    }

    #[test]
    fn identity_matches_the_independent_golden_payload() {
        let wire = identity().encode_success().unwrap();
        assert_eq!(
            &wire,
            include_bytes!("../../../contracts/golden/identity_v5_response.bin")
        );
        assert_eq!(NodeIdentity::decode(&wire).unwrap(), identity());
        assert!(NodeIdentity::decode(&wire)
            .unwrap()
            .require_local_protocol()
            .is_ok());
    }

    #[test]
    fn capabilities_match_the_independent_golden_payload() {
        let wire = capabilities().encode_success().unwrap();
        assert_eq!(
            &wire,
            include_bytes!("../../../contracts/golden/capabilities_v5_response.bin")
        );
        assert_eq!(NodeCapabilities::decode(&wire).unwrap(), capabilities());
    }

    #[test]
    fn capabilities_describe_measurement_output_rates_after_decimation() {
        let value = capabilities();
        let expected = [
            OutputRateCode::Hz2000,
            OutputRateCode::Hz1000,
            OutputRateCode::Hz500,
            OutputRateCode::Hz250,
            OutputRateCode::Hz125,
            OutputRateCode::Hz62_5,
        ];

        for rate in expected {
            assert!(value.supports_output_rate(rate));
        }
        assert!(!value.supports_output_rate(OutputRateCode::Hz4000));
        assert!(!value.supports_output_rate(OutputRateCode::Hz31_25));
        assert_eq!(OutputRateCode::Hz62_5.sample_rate(), SampleRate::HZ_62_5);
        assert_eq!(OutputRateCode::Hz62_5.hertz_label(), "62.5");
    }

    #[test]
    fn handshake_fails_closed_on_identity_invariants() {
        let mut wire = identity().encode_success().unwrap();
        wire[9..17].fill(0);
        assert_eq!(
            NodeIdentity::decode(&wire),
            Err(IdentityContractError::ZeroBootEpoch)
        );

        wire = identity().encode_success().unwrap();
        wire[22] = 2;
        assert_eq!(
            NodeIdentity::decode(&wire)
                .unwrap()
                .require_local_protocol(),
            Err(IdentityContractError::ProtocolMismatch)
        );
    }

    #[test]
    fn measurement_capability_requires_a_bounded_journal() {
        let mut value = capabilities();
        value.journal_capacity_samples = 0;
        assert_eq!(
            value.validate(),
            Err(IdentityContractError::IncompleteMeasurementCapabilities)
        );

        value = capabilities();
        value.output_rate_mask = 1 << 15;
        assert_eq!(
            value.validate(),
            Err(IdentityContractError::UnknownOutputRateBits)
        );

        value = capabilities();
        value.schema_version = 1;
        assert_eq!(
            value.validate(),
            Err(IdentityContractError::UnsupportedCapabilitiesSchema)
        );

        value = capabilities();
        value.max_sample_chunk_samples =
            (crate::measurement_wire::MAX_WIRE_CHUNK_SAMPLES + 1) as u16;
        assert_eq!(
            value.validate(),
            Err(IdentityContractError::InvalidMaxSampleChunk)
        );

        value = capabilities();
        value.feature_flags |= FEATURE_TRIAL_BOOT_CONFIRMATION;
        assert_eq!(
            value.validate(),
            Err(IdentityContractError::InvalidFeatureCombination)
        );

        value = capabilities();
        value.service_flags &= !ServiceId::Identity.capability_bit();
        assert_eq!(
            value.validate(),
            Err(IdentityContractError::MissingIdentityService)
        );
    }

    #[test]
    fn explicit_remote_error_does_not_require_a_success_payload() {
        assert_eq!(
            NodeIdentity::decode(&[ResponseStatus::Busy as u8]),
            Err(IdentityContractError::RemoteError(ResponseStatus::Busy))
        );
    }

    #[test]
    fn schema_1_is_available_only_through_the_explicit_migration_decoder() {
        let mut wire = capabilities().encode_success().unwrap();
        wire[1] = 1;
        wire[6..14].copy_from_slice(
            &(capabilities().feature_flags | FEATURE_TRIAL_BOOT_CONFIRMATION).to_le_bytes(),
        );
        assert_eq!(
            NodeCapabilities::decode(&wire),
            Err(IdentityContractError::UnsupportedCapabilitiesSchema)
        );

        let decoded = NodeCapabilities::decode_schema_1_for_migration(&wire).unwrap();
        assert_eq!(decoded.schema_version, 1);

        let mut invalid = wire;
        invalid[14..16].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(
            NodeCapabilities::decode_schema_1_for_migration(&invalid),
            Err(IdentityContractError::InvalidMaxPayload)
        );

        wire[1] = CAPABILITIES_SCHEMA_VERSION;
        assert_eq!(
            NodeCapabilities::decode_schema_1_for_migration(&wire),
            Err(IdentityContractError::UnsupportedCapabilitiesSchema)
        );
    }
}
