#![forbid(unsafe_code)]

pub mod adxl355;
pub mod application;
pub mod diagnostics_wire;
pub mod framing;
pub mod identity;
pub mod lifecycle_wire;
pub mod measurement;
pub mod measurement_wire;
pub mod time_wire;
pub mod version;

pub use adxl355::{Adxl355Odr, Adxl355SampleCode, Adxl355SampleCodeError};
pub use application::{ApplicationContractError, ResponseStatus, ServiceId};
pub use diagnostics_wire::*;
pub use framing::{
    crc32c, Frame, FrameError, FrameRole, StreamDecoder, BROADCAST_ADDRESS, CRC_BYTES,
    FRAMING_VERSION, HEADER_BYTES, HOST_ADDRESS, MAX_COBS_BODY_BYTES, MAX_PAYLOAD_BYTES,
    MAX_RAW_FRAME_BYTES, MAX_WIRE_FRAME_BYTES,
};
pub use identity::{
    FirmwareVersion, IdentityContractError, NodeCapabilities, NodeIdentity, OutputRateCode,
    CAPABILITIES_RESPONSE_BYTES, CAPABILITIES_SCHEMA_VERSION, FEATURE_ATOMIC_CONFIG,
    FEATURE_DRDY_TIMESTAMPS, FEATURE_REMOTE_FIRMWARE_UPDATE, FEATURE_SAMPLE_JOURNAL,
    FEATURE_TRIAL_BOOT_CONFIRMATION, GET_CAPABILITIES_MESSAGE_ID, GET_IDENTITY_MESSAGE_ID,
    IDENTITY_RESPONSE_BYTES, IDENTITY_SERVICE_ID, IMPLEMENTED_OUTPUT_RATE_MASK,
    KNOWN_FEATURE_FLAGS, KNOWN_OUTPUT_RATE_MASK, KNOWN_SERVICE_FLAGS,
};
pub use lifecycle_wire::*;
pub use measurement::{
    ContinuityBreakEvidence, GapEvidence, MeasurementContractError, PullSamplesRequest,
    ReleaseSamplesRequest, SampleChunkHeader, StreamState, PULL_SAMPLES_REQUEST_BYTES,
    SAMPLE_CHUNK_FIELDS_BYTES, SAMPLE_CHUNK_SUCCESS_HEADER_BYTES, XYZ_I24_WIRE_SAMPLE_BYTES,
};
pub use measurement_wire::*;
pub use time_wire::*;
pub use version::{ProtocolMismatch, ProtocolVersion, LOCAL_PROTOCOL_VERSION};
