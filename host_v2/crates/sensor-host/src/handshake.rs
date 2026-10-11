use core::fmt;

use sensor_core::HardwareId;
use sensor_protocol::{
    FirmwareVersion, IdentityContractError, NodeCapabilities, NodeIdentity, OutputRateCode,
    ServiceId, GET_CAPABILITIES_MESSAGE_ID, GET_IDENTITY_MESSAGE_ID, IDENTITY_SERVICE_ID,
};

use crate::transaction::{DurationMicros, RequestSpec, TimeoutRecovery};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HandshakeRequirements {
    pub hardware_id: HardwareId,
    pub node_address: u8,
    pub board_revision: u8,
    pub required_services: u32,
    pub required_features: u64,
    pub required_output_rate: Option<OutputRateCode>,
    pub schema_1_migration_firmware: Option<FirmwareVersion>,
}

impl HandshakeRequirements {
    pub const fn recording_node(
        hardware_id: HardwareId,
        node_address: u8,
        board_revision: u8,
        required_features: u64,
    ) -> Self {
        Self {
            hardware_id,
            node_address,
            board_revision,
            required_services: ServiceId::Identity.capability_bit()
                | ServiceId::Measurement.capability_bit()
                | ServiceId::Lifecycle.capability_bit(),
            required_features,
            required_output_rate: None,
            schema_1_migration_firmware: None,
        }
    }

    pub const fn for_schema_1_migration(mut self, firmware: FirmwareVersion) -> Self {
        self.schema_1_migration_firmware = Some(firmware);
        self
    }

    pub const fn with_output_rate(mut self, output_rate: OutputRateCode) -> Self {
        self.required_output_rate = Some(output_rate);
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandshakePhase {
    NeedIdentity,
    NeedCapabilities,
    Ready,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeHandshake {
    requirements: HandshakeRequirements,
    phase: HandshakePhase,
    identity: Option<NodeIdentity>,
    capabilities: Option<NodeCapabilities>,
}

impl NodeHandshake {
    pub const fn new(requirements: HandshakeRequirements) -> Self {
        Self {
            requirements,
            phase: HandshakePhase::NeedIdentity,
            identity: None,
            capabilities: None,
        }
    }

    pub const fn phase(self) -> HandshakePhase {
        self.phase
    }

    pub const fn requirements(self) -> HandshakeRequirements {
        self.requirements
    }

    pub const fn identity(self) -> Option<NodeIdentity> {
        self.identity
    }

    pub const fn capabilities(self) -> Option<NodeCapabilities> {
        self.capabilities
    }

    pub fn accept_identity(&mut self, payload: &[u8]) -> Result<(), HandshakeError> {
        if self.phase != HandshakePhase::NeedIdentity {
            return Err(HandshakeError::UnexpectedPhase);
        }
        let identity = NodeIdentity::decode(payload)?.require_local_protocol()?;
        if identity.hardware_id != self.requirements.hardware_id {
            return Err(HandshakeError::HardwareIdMismatch);
        }
        if identity.node_address != self.requirements.node_address {
            return Err(HandshakeError::NodeAddressMismatch);
        }
        if identity.board_revision != self.requirements.board_revision {
            return Err(HandshakeError::BoardRevisionMismatch);
        }
        self.identity = Some(identity);
        self.phase = HandshakePhase::NeedCapabilities;
        Ok(())
    }

    pub fn accept_capabilities(&mut self, payload: &[u8]) -> Result<(), HandshakeError> {
        if self.phase != HandshakePhase::NeedCapabilities {
            return Err(HandshakeError::UnexpectedPhase);
        }
        let capabilities = match NodeCapabilities::decode(payload) {
            Ok(capabilities) => capabilities,
            Err(error) => {
                let Some(expected_firmware) = self.requirements.schema_1_migration_firmware else {
                    return Err(error.into());
                };
                if self.identity.map(|identity| identity.firmware) != Some(expected_firmware) {
                    return Err(HandshakeError::FirmwareVersionMismatch);
                }
                NodeCapabilities::decode_schema_1_for_migration(payload)?
            }
        };
        let missing_services = self.requirements.required_services & !capabilities.service_flags;
        if missing_services != 0 {
            return Err(HandshakeError::MissingServices(missing_services));
        }
        let missing_features = self.requirements.required_features & !capabilities.feature_flags;
        if missing_features != 0 {
            return Err(HandshakeError::MissingFeatures(missing_features));
        }
        if let Some(required_rate) = self.requirements.required_output_rate {
            if !capabilities.supports_output_rate(required_rate) {
                return Err(HandshakeError::UnsupportedOutputRate(required_rate));
            }
        }
        self.capabilities = Some(capabilities);
        self.phase = HandshakePhase::Ready;
        Ok(())
    }
}

pub const fn identity_request(node_address: u8, timeout: DurationMicros) -> RequestSpec {
    RequestSpec {
        node_address,
        service: IDENTITY_SERVICE_ID,
        request_message: GET_IDENTITY_MESSAGE_ID,
        response_message: GET_IDENTITY_MESSAGE_ID,
        timeout,
        timeout_recovery: TimeoutRecovery::RetrySafe,
    }
}

pub const fn capabilities_request(node_address: u8, timeout: DurationMicros) -> RequestSpec {
    RequestSpec {
        node_address,
        service: IDENTITY_SERVICE_ID,
        request_message: GET_CAPABILITIES_MESSAGE_ID,
        response_message: GET_CAPABILITIES_MESSAGE_ID,
        timeout,
        timeout_recovery: TimeoutRecovery::RetrySafe,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandshakeError {
    UnexpectedPhase,
    Contract(IdentityContractError),
    HardwareIdMismatch,
    NodeAddressMismatch,
    BoardRevisionMismatch,
    FirmwareVersionMismatch,
    MissingServices(u32),
    MissingFeatures(u64),
    UnsupportedOutputRate(OutputRateCode),
}

impl From<IdentityContractError> for HandshakeError {
    fn from(value: IdentityContractError) -> Self {
        Self::Contract(value)
    }
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedPhase => formatter.write_str("handshake response arrived out of order"),
            Self::Contract(error) => write!(formatter, "invalid handshake payload: {error}"),
            Self::HardwareIdMismatch => formatter.write_str("unexpected node hardware identity"),
            Self::NodeAddressMismatch => formatter.write_str("node reported a different address"),
            Self::BoardRevisionMismatch => formatter.write_str("unexpected node board revision"),
            Self::FirmwareVersionMismatch => {
                formatter.write_str("unexpected node firmware version")
            }
            Self::MissingServices(bits) => {
                write!(
                    formatter,
                    "node lacks required services bitmap 0x{bits:08x}"
                )
            }
            Self::MissingFeatures(bits) => {
                write!(
                    formatter,
                    "node lacks required features bitmap 0x{bits:016x}"
                )
            }
            Self::UnsupportedOutputRate(rate) => {
                write!(
                    formatter,
                    "node does not support {} Hz output",
                    rate.hertz_label()
                )
            }
        }
    }
}

impl std::error::Error for HandshakeError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::{MonotonicMicros, ResponseDisposition, TransactionCore};
    use sensor_core::BootEpoch;
    use sensor_protocol::{
        FirmwareVersion, Frame, FrameRole, ProtocolVersion, CAPABILITIES_SCHEMA_VERSION,
        FEATURE_SAMPLE_JOURNAL, FEATURE_TRIAL_BOOT_CONFIRMATION, HOST_ADDRESS,
        LOCAL_PROTOCOL_VERSION, MAX_PAYLOAD_BYTES,
    };

    const HARDWARE_ID: HardwareId = HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);

    fn timeout() -> DurationMicros {
        DurationMicros::new(10_000).unwrap()
    }

    fn identity(protocol: ProtocolVersion) -> NodeIdentity {
        NodeIdentity {
            hardware_id: HARDWARE_ID,
            boot_epoch: BootEpoch::new(77),
            node_address: 7,
            board_revision: 2,
            firmware: FirmwareVersion {
                major: 0,
                minor: 5,
                patch: 0,
            },
            protocol,
        }
    }

    fn identity_for_firmware(firmware: FirmwareVersion) -> NodeIdentity {
        NodeIdentity {
            firmware,
            ..identity(LOCAL_PROTOCOL_VERSION)
        }
    }

    fn capabilities() -> NodeCapabilities {
        NodeCapabilities {
            schema_version: CAPABILITIES_SCHEMA_VERSION,
            service_flags: ServiceId::Identity.capability_bit()
                | ServiceId::Measurement.capability_bit()
                | ServiceId::Lifecycle.capability_bit(),
            feature_flags: FEATURE_SAMPLE_JOURNAL,
            max_payload_bytes: MAX_PAYLOAD_BYTES as u16,
            journal_capacity_samples: 4096,
            output_rate_mask: sensor_protocol::IMPLEMENTED_OUTPUT_RATE_MASK,
            max_sample_chunk_samples: 48,
        }
    }

    fn response(active: crate::transaction::ActiveTransaction, payload: &[u8]) -> Frame {
        Frame::new(
            FrameRole::Response,
            HOST_ADDRESS,
            active.node_address,
            active.service,
            active.response_message,
            active.transaction_id,
            payload,
        )
        .unwrap()
    }

    #[test]
    fn completes_a_fail_closed_handshake_through_transaction_core() {
        let requirements =
            HandshakeRequirements::recording_node(HARDWARE_ID, 7, 2, FEATURE_SAMPLE_JOURNAL);
        let mut handshake = NodeHandshake::new(requirements);
        let mut transactions = TransactionCore::new(10, DurationMicros::new(50_000).unwrap());

        let identity_attempt = transactions
            .begin(MonotonicMicros::new(0), identity_request(7, timeout()))
            .unwrap();
        let identity_response = response(
            identity_attempt,
            &identity(LOCAL_PROTOCOL_VERSION).encode_success().unwrap(),
        );
        assert_eq!(
            transactions.receive(MonotonicMicros::new(100), &identity_response),
            Ok(ResponseDisposition::Accepted)
        );
        handshake
            .accept_identity(identity_response.payload())
            .unwrap();

        let capabilities_attempt = transactions
            .begin(
                MonotonicMicros::new(100),
                capabilities_request(7, timeout()),
            )
            .unwrap();
        let capabilities_response = response(
            capabilities_attempt,
            &capabilities().encode_success().unwrap(),
        );
        assert_eq!(
            transactions.receive(MonotonicMicros::new(200), &capabilities_response),
            Ok(ResponseDisposition::Accepted)
        );
        handshake
            .accept_capabilities(capabilities_response.payload())
            .unwrap();
        assert_eq!(handshake.phase(), HandshakePhase::Ready);
    }

    #[test]
    fn rejects_wrong_protocol_identity_and_missing_capability() {
        let requirements =
            HandshakeRequirements::recording_node(HARDWARE_ID, 7, 2, FEATURE_SAMPLE_JOURNAL);
        let mut wrong_protocol = NodeHandshake::new(requirements);
        let payload = identity(ProtocolVersion::new(3, 4))
            .encode_success()
            .unwrap();
        assert_eq!(
            wrong_protocol.accept_identity(&payload),
            Err(HandshakeError::Contract(
                IdentityContractError::ProtocolMismatch
            ))
        );

        let requirements =
            HandshakeRequirements::recording_node(HARDWARE_ID, 7, 2, FEATURE_SAMPLE_JOURNAL)
                .with_output_rate(OutputRateCode::Hz31_25);
        let mut unsupported_rate = NodeHandshake::new(requirements);
        unsupported_rate
            .accept_identity(&identity(LOCAL_PROTOCOL_VERSION).encode_success().unwrap())
            .unwrap();
        assert_eq!(
            unsupported_rate.accept_capabilities(&capabilities().encode_success().unwrap()),
            Err(HandshakeError::UnsupportedOutputRate(
                OutputRateCode::Hz31_25
            ))
        );

        let mut missing = NodeHandshake::new(requirements);
        missing
            .accept_identity(&identity(LOCAL_PROTOCOL_VERSION).encode_success().unwrap())
            .unwrap();
        let mut incomplete = capabilities();
        incomplete.service_flags = ServiceId::Identity.capability_bit();
        assert_eq!(
            missing.accept_capabilities(&incomplete.encode_success().unwrap()),
            Err(HandshakeError::MissingServices(
                ServiceId::Measurement.capability_bit() | ServiceId::Lifecycle.capability_bit()
            ))
        );
    }

    #[test]
    fn schema_1_migration_requires_the_exact_pinned_firmware() {
        let migration_firmware = FirmwareVersion {
            major: 0,
            minor: 5,
            patch: 4,
        };
        let requirements =
            HandshakeRequirements::recording_node(HARDWARE_ID, 7, 2, FEATURE_SAMPLE_JOURNAL)
                .for_schema_1_migration(migration_firmware);

        let mut wrong_version = NodeHandshake::new(requirements);
        wrong_version
            .accept_identity(
                &identity_for_firmware(FirmwareVersion {
                    major: 0,
                    minor: 5,
                    patch: 3,
                })
                .encode_success()
                .unwrap(),
            )
            .unwrap();
        let mut schema_1 = capabilities().encode_success().unwrap();
        schema_1[1] = 1;
        assert_eq!(
            wrong_version.accept_capabilities(&schema_1),
            Err(HandshakeError::FirmwareVersionMismatch)
        );

        let mut current_schema_newer_firmware = NodeHandshake::new(requirements);
        current_schema_newer_firmware
            .accept_identity(
                &identity_for_firmware(FirmwareVersion {
                    major: 0,
                    minor: 5,
                    patch: 5,
                })
                .encode_success()
                .unwrap(),
            )
            .unwrap();
        current_schema_newer_firmware
            .accept_capabilities(&capabilities().encode_success().unwrap())
            .unwrap();

        let mut migration = NodeHandshake::new(requirements);
        migration
            .accept_identity(
                &identity_for_firmware(migration_firmware)
                    .encode_success()
                    .unwrap(),
            )
            .unwrap();
        let mut schema_1 = capabilities().encode_success().unwrap();
        schema_1[1] = 1;
        schema_1[6..14].copy_from_slice(
            &(capabilities().feature_flags | FEATURE_TRIAL_BOOT_CONFIRMATION).to_le_bytes(),
        );
        migration.accept_capabilities(&schema_1).unwrap();
        assert_eq!(migration.phase(), HandshakePhase::Ready);

        let normal_requirements =
            HandshakeRequirements::recording_node(HARDWARE_ID, 7, 2, FEATURE_SAMPLE_JOURNAL);
        let mut normal = NodeHandshake::new(normal_requirements);
        normal
            .accept_identity(
                &identity_for_firmware(migration_firmware)
                    .encode_success()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            normal.accept_capabilities(&schema_1),
            Err(HandshakeError::Contract(
                IdentityContractError::UnsupportedCapabilitiesSchema
            ))
        );
    }
}
