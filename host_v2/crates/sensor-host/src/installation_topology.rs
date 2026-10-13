use core::fmt;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use sensor_core::HardwareId;
use sensor_protocol::identity::OutputRateCode;
use serde_json::{Map, Value};

use crate::installation_config::{
    load_merged_document, InstallationConfigError, InstallationStorageProfile,
};
use crate::runtime_profile::{HostRuntimeInstallationProfile, RuntimeProfileError};

pub const MAX_HOST_CHANNELS: usize = 16;
pub const MAX_NODES_PER_CHANNEL: usize = 253;
pub const PRODUCTION_RS485_BAUD_RATE: u32 = 115_200;

pub struct HostInstallationProfile {
    system_name: String,
    config_fingerprint: u64,
    storage: InstallationStorageProfile,
    topology: InstallationTopologyProfile,
    runtime: HostRuntimeInstallationProfile,
}

impl HostInstallationProfile {
    pub fn load(
        config_path: impl AsRef<Path>,
        repository_root: impl AsRef<Path>,
    ) -> Result<Self, HostInstallationProfileError> {
        let document = load_merged_document(config_path.as_ref())
            .map_err(HostInstallationProfileError::Installation)?;
        let storage =
            InstallationStorageProfile::from_document(&document, repository_root.as_ref())
                .map_err(HostInstallationProfileError::Installation)?;
        let topology = InstallationTopologyProfile::from_document(&document)
            .map_err(HostInstallationProfileError::Topology)?;
        let runtime = HostRuntimeInstallationProfile::from_document(&document)
            .map_err(HostInstallationProfileError::Runtime)?;
        runtime
            .channel_startup(storage.time_sync())
            .map_err(HostInstallationProfileError::Runtime)?;
        let system_name = topology.system_name().to_owned();
        let config_fingerprint = fingerprint_document(&document);
        Ok(Self {
            system_name,
            config_fingerprint,
            storage,
            topology,
            runtime,
        })
    }

    pub fn system_name(&self) -> &str {
        &self.system_name
    }

    pub const fn config_fingerprint(&self) -> u64 {
        self.config_fingerprint
    }

    pub const fn storage(&self) -> &InstallationStorageProfile {
        &self.storage
    }

    pub const fn topology(&self) -> &InstallationTopologyProfile {
        &self.topology
    }

    pub const fn runtime(&self) -> &HostRuntimeInstallationProfile {
        &self.runtime
    }
}

fn fingerprint_document(document: &Value) -> u64 {
    // serde_json's default map representation has deterministic key ordering.
    // FNV-1a is used as a compact change detector, not as an authentication
    // primitive; the algorithm is named explicitly in the status contract.
    let bytes = serde_json::to_vec(document).expect("serializing a JSON Value cannot fail");
    bytes.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallationTopologyProfile {
    system_name: String,
    channels: Vec<ChannelInstallationProfile>,
}

impl InstallationTopologyProfile {
    pub fn from_document(document: &Value) -> Result<Self, TopologyConfigError> {
        let root = required_object(document, "root")?;
        let system = required_object_field(root, "system", "root")?;
        let system_name = required_identifier(system, "name", "system")?.to_owned();
        let channel_values = required_array_field(root, "channels", "root")?;
        if channel_values.is_empty() || channel_values.len() > MAX_HOST_CHANNELS {
            return Err(TopologyConfigError::InvalidChannelCount {
                actual: channel_values.len(),
                maximum: MAX_HOST_CHANNELS,
            });
        }

        let mut channels = Vec::with_capacity(channel_values.len());
        let mut channel_names = HashSet::new();
        let mut channel_ids = HashSet::new();
        let mut endpoints = HashSet::new();
        let mut hardware_ids = HashSet::new();

        for (index, value) in channel_values.iter().enumerate() {
            let path = format!("channels[{index}]");
            let channel = parse_channel(value, &path)?;
            if !channel_names.insert(channel.name.clone()) {
                return Err(TopologyConfigError::DuplicateChannelName(channel.name));
            }
            if !channel_ids.insert(channel.channel_id) {
                return Err(TopologyConfigError::DuplicateChannelId(channel.channel_id));
            }
            let endpoint = channel.transport.endpoint().to_path_buf();
            if !endpoints.insert(endpoint.clone()) {
                return Err(TopologyConfigError::DuplicateTransportEndpoint(endpoint));
            }
            for node in &channel.nodes {
                if !hardware_ids.insert(node.hardware_id) {
                    return Err(TopologyConfigError::DuplicateHardwareId(node.hardware_id));
                }
            }
            channels.push(channel);
        }

        Ok(Self {
            system_name,
            channels,
        })
    }

    pub fn system_name(&self) -> &str {
        &self.system_name
    }

    pub fn channels(&self) -> &[ChannelInstallationProfile] {
        &self.channels
    }

    pub fn enabled_channels(&self) -> impl Iterator<Item = &ChannelInstallationProfile> {
        self.channels.iter().filter(|channel| channel.enabled)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelInstallationProfile {
    name: String,
    channel_id: u8,
    display_label: Option<String>,
    enabled: bool,
    transport: TransportBinding,
    nodes: Vec<NodeInstallationProfile>,
}

impl ChannelInstallationProfile {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn channel_id(&self) -> u8 {
        self.channel_id
    }

    pub fn display_label(&self) -> Option<&str> {
        self.display_label.as_deref()
    }

    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    pub const fn transport(&self) -> &TransportBinding {
        &self.transport
    }

    pub fn nodes(&self) -> &[NodeInstallationProfile] {
        &self.nodes
    }

    pub fn enabled_nodes(&self) -> impl Iterator<Item = &NodeInstallationProfile> {
        self.nodes.iter().filter(|node| node.enabled)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportBinding {
    SerialRs485(SerialRs485Binding),
}

impl TransportBinding {
    pub fn device_id(&self) -> &str {
        match self {
            Self::SerialRs485(binding) => binding.device_id(),
        }
    }

    pub fn endpoint(&self) -> &Path {
        match self {
            Self::SerialRs485(binding) => binding.endpoint(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SerialRs485Binding {
    device_id: String,
    endpoint: PathBuf,
    baud_rate: u32,
}

impl SerialRs485Binding {
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    pub const fn baud_rate(&self) -> u32 {
        self.baud_rate
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeInstallationProfile {
    node_address: u8,
    hardware_id: HardwareId,
    board_revision: u8,
    enabled: bool,
    output_rate: OutputRateCode,
}

impl NodeInstallationProfile {
    pub const fn node_address(&self) -> u8 {
        self.node_address
    }

    pub const fn hardware_id(&self) -> HardwareId {
        self.hardware_id
    }

    pub const fn board_revision(&self) -> u8 {
        self.board_revision
    }

    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    pub const fn output_rate(&self) -> OutputRateCode {
        self.output_rate
    }
}

fn parse_channel(
    value: &Value,
    path: &str,
) -> Result<ChannelInstallationProfile, TopologyConfigError> {
    let object = required_object(value, path)?;
    let name = required_identifier(object, "name", path)?.to_owned();
    let channel_id = required_u8(object, "channel_id", path)?;
    if !(1..=MAX_HOST_CHANNELS as u8).contains(&channel_id) {
        return Err(TopologyConfigError::ChannelIdOutOfRange(channel_id));
    }
    let display_label = optional_nonempty_string(object, "label", path)?.map(str::to_owned);
    let enabled = required_bool(object, "enabled", path)?;
    let transport = parse_transport(required_value(object, "transport", path)?, path)?;
    let node_values = required_array_field(object, "nodes", path)?;
    if node_values.is_empty() || node_values.len() > MAX_NODES_PER_CHANNEL {
        return Err(TopologyConfigError::InvalidNodeCount {
            channel: name,
            actual: node_values.len(),
            maximum: MAX_NODES_PER_CHANNEL,
        });
    }

    let mut nodes = Vec::with_capacity(node_values.len());
    let mut addresses = HashSet::new();
    for (index, value) in node_values.iter().enumerate() {
        let node_path = format!("{path}.nodes[{index}]");
        let node = parse_node(value, &node_path)?;
        if !addresses.insert(node.node_address) {
            return Err(TopologyConfigError::DuplicateNodeAddress {
                channel: name,
                address: node.node_address,
            });
        }
        nodes.push(node);
    }
    if enabled && !nodes.iter().any(|node| node.enabled) {
        return Err(TopologyConfigError::EnabledChannelWithoutEnabledNode(name));
    }

    Ok(ChannelInstallationProfile {
        name,
        channel_id,
        display_label,
        enabled,
        transport,
        nodes,
    })
}

fn parse_transport(
    value: &Value,
    channel_path: &str,
) -> Result<TransportBinding, TopologyConfigError> {
    const TRANSPORT_KEYS: [&str; 4] = ["kind", "device_id", "endpoint", "baud_rate"];
    let path = format!("{channel_path}.transport");
    let object = required_object(value, &path)?;
    if let Some(key) = object
        .keys()
        .find(|key| !TRANSPORT_KEYS.contains(&key.as_str()))
    {
        return Err(TopologyConfigError::UnknownField {
            path,
            field: key.clone(),
        });
    }
    let kind = required_string(object, "kind", &path)?;
    if kind != "serial-rs485" {
        return Err(TopologyConfigError::UnsupportedTransportKind(
            kind.to_owned(),
        ));
    }
    let device_id = required_identifier(object, "device_id", &path)?.to_owned();
    let endpoint = PathBuf::from(required_string(object, "endpoint", &path)?);
    if !endpoint.is_absolute() {
        return Err(TopologyConfigError::TransportEndpointNotAbsolute(endpoint));
    }
    let baud = required_u64(object, "baud_rate", &path)?;
    if baud != u64::from(PRODUCTION_RS485_BAUD_RATE) {
        return Err(TopologyConfigError::UnsupportedBaudRate(baud));
    }

    Ok(TransportBinding::SerialRs485(SerialRs485Binding {
        device_id,
        endpoint,
        baud_rate: PRODUCTION_RS485_BAUD_RATE,
    }))
}

fn parse_node(value: &Value, path: &str) -> Result<NodeInstallationProfile, TopologyConfigError> {
    let object = required_object(value, path)?;
    let node_address = required_u8(object, "id", path)?;
    if node_address == 0 || node_address == u8::MAX {
        return Err(TopologyConfigError::NodeAddressOutOfRange(node_address));
    }
    let hardware_text = required_string(object, "hardware_id", path)?;
    let hardware_id = parse_hardware_id(hardware_text)
        .ok_or_else(|| TopologyConfigError::InvalidHardwareId(hardware_text.to_owned()))?;
    let board_revision = required_u8(object, "board_revision", path)?;
    if !matches!(board_revision, 1 | 2) {
        return Err(TopologyConfigError::UnsupportedBoardRevision(
            board_revision,
        ));
    }
    let enabled = required_bool(object, "enabled", path)?;
    let output_rate = parse_output_rate(required_value(object, "output_rate_hz", path)?)
        .ok_or_else(|| TopologyConfigError::UnsupportedOutputRate {
            value: object["output_rate_hz"].to_string(),
        })?;

    Ok(NodeInstallationProfile {
        node_address,
        hardware_id,
        board_revision,
        enabled,
        output_rate,
    })
}

fn parse_hardware_id(value: &str) -> Option<HardwareId> {
    if value.len() != 16 || !value.is_ascii() {
        return None;
    }
    let mut bytes = [0_u8; 8];
    for (index, byte) in bytes.iter_mut().enumerate() {
        let high = hex_nibble(value.as_bytes()[index * 2])?;
        let low = hex_nibble(value.as_bytes()[index * 2 + 1])?;
        *byte = (high << 4) | low;
    }
    if bytes == [0; 8] {
        None
    } else {
        Some(HardwareId::from_bytes(bytes))
    }
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn parse_output_rate(value: &Value) -> Option<OutputRateCode> {
    let label = match value {
        Value::String(value) => value.as_str(),
        Value::Number(value) => return parse_output_rate_label(&value.to_string()),
        _ => return None,
    };
    parse_output_rate_label(label)
}

fn parse_output_rate_label(value: &str) -> Option<OutputRateCode> {
    match value.trim().trim_end_matches(".0") {
        "4000" => Some(OutputRateCode::Hz4000),
        "2000" => Some(OutputRateCode::Hz2000),
        "1000" => Some(OutputRateCode::Hz1000),
        "500" => Some(OutputRateCode::Hz500),
        "250" => Some(OutputRateCode::Hz250),
        "125" => Some(OutputRateCode::Hz125),
        "62.5" => Some(OutputRateCode::Hz62_5),
        "31.25" => Some(OutputRateCode::Hz31_25),
        "15.625" => Some(OutputRateCode::Hz15_625),
        "7.8125" => Some(OutputRateCode::Hz7_8125),
        "3.90625" => Some(OutputRateCode::Hz3_90625),
        _ => None,
    }
}

fn required_object<'a>(
    value: &'a Value,
    path: &str,
) -> Result<&'a Map<String, Value>, TopologyConfigError> {
    value
        .as_object()
        .ok_or_else(|| TopologyConfigError::ExpectedObject(path.to_owned()))
}

fn required_object_field<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
    path: &str,
) -> Result<&'a Map<String, Value>, TopologyConfigError> {
    required_object(
        required_value(object, field, path)?,
        &format!("{path}.{field}"),
    )
}

fn required_array_field<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
    path: &str,
) -> Result<&'a [Value], TopologyConfigError> {
    object
        .get(field)
        .ok_or_else(|| TopologyConfigError::MissingField {
            path: path.to_owned(),
            field,
        })?
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| TopologyConfigError::InvalidFieldType {
            path: path.to_owned(),
            field,
        })
}

fn required_value<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
    path: &str,
) -> Result<&'a Value, TopologyConfigError> {
    object
        .get(field)
        .ok_or_else(|| TopologyConfigError::MissingField {
            path: path.to_owned(),
            field,
        })
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
    path: &str,
) -> Result<&'a str, TopologyConfigError> {
    required_value(object, field, path)?
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| TopologyConfigError::InvalidFieldType {
            path: path.to_owned(),
            field,
        })
}

fn required_identifier<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
    path: &str,
) -> Result<&'a str, TopologyConfigError> {
    let value = required_string(object, field, path)?;
    if value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Err(TopologyConfigError::InvalidIdentifier {
            path: path.to_owned(),
            field,
        })
    } else {
        Ok(value)
    }
}

fn optional_nonempty_string<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
    path: &str,
) -> Result<Option<&'a str>, TopologyConfigError> {
    match object.get(field) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .map(Some)
            .ok_or_else(|| TopologyConfigError::InvalidFieldType {
                path: path.to_owned(),
                field,
            }),
    }
}

fn required_bool(
    object: &Map<String, Value>,
    field: &'static str,
    path: &str,
) -> Result<bool, TopologyConfigError> {
    required_value(object, field, path)?
        .as_bool()
        .ok_or_else(|| TopologyConfigError::InvalidFieldType {
            path: path.to_owned(),
            field,
        })
}

fn required_u64(
    object: &Map<String, Value>,
    field: &'static str,
    path: &str,
) -> Result<u64, TopologyConfigError> {
    required_value(object, field, path)?
        .as_u64()
        .ok_or_else(|| TopologyConfigError::InvalidFieldType {
            path: path.to_owned(),
            field,
        })
}

fn required_u8(
    object: &Map<String, Value>,
    field: &'static str,
    path: &str,
) -> Result<u8, TopologyConfigError> {
    let value = required_u64(object, field, path)?;
    u8::try_from(value).map_err(|_| TopologyConfigError::InvalidFieldType {
        path: path.to_owned(),
        field,
    })
}

#[derive(Debug)]
pub enum HostInstallationProfileError {
    Installation(InstallationConfigError),
    Topology(TopologyConfigError),
    Runtime(RuntimeProfileError),
}

impl fmt::Display for HostInstallationProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Installation(error) => error.fmt(formatter),
            Self::Topology(error) => error.fmt(formatter),
            Self::Runtime(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for HostInstallationProfileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Installation(error) => Some(error),
            Self::Topology(error) => Some(error),
            Self::Runtime(error) => Some(error),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum TopologyConfigError {
    ExpectedObject(String),
    MissingField {
        path: String,
        field: &'static str,
    },
    InvalidFieldType {
        path: String,
        field: &'static str,
    },
    InvalidIdentifier {
        path: String,
        field: &'static str,
    },
    UnknownField {
        path: String,
        field: String,
    },
    InvalidChannelCount {
        actual: usize,
        maximum: usize,
    },
    ChannelIdOutOfRange(u8),
    InvalidNodeCount {
        channel: String,
        actual: usize,
        maximum: usize,
    },
    NodeAddressOutOfRange(u8),
    DuplicateChannelName(String),
    DuplicateChannelId(u8),
    DuplicateTransportEndpoint(PathBuf),
    DuplicateNodeAddress {
        channel: String,
        address: u8,
    },
    DuplicateHardwareId(HardwareId),
    EnabledChannelWithoutEnabledNode(String),
    UnsupportedTransportKind(String),
    TransportEndpointNotAbsolute(PathBuf),
    UnsupportedBaudRate(u64),
    InvalidHardwareId(String),
    UnsupportedBoardRevision(u8),
    UnsupportedOutputRate {
        value: String,
    },
}

impl fmt::Display for TopologyConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "installation topology error: {self:?}")
    }
}

impl std::error::Error for TopologyConfigError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("sensor-host-topology-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn valid_document() -> Value {
        serde_json::from_str(r#"{
            "system": {"name": "rpi-sanok"},
            "channels": [
                {
                    "name": "line-a", "channel_id": 1, "label": "Linia A", "enabled": true,
                    "transport": {"kind": "serial-rs485", "device_id": "usb-rs485-1", "endpoint": "/dev/line-a", "baud_rate": 115200},
                    "nodes": [{"id": 1, "hardware_id": "2f13349450fa4084", "board_revision": 2, "enabled": true, "output_rate_hz": 125}]
                },
                {
                    "name": "line-b", "channel_id": 2, "enabled": true,
                    "transport": {"kind": "serial-rs485", "device_id": "usb-rs485-1", "endpoint": "/dev/line-b", "baud_rate": 115200},
                    "nodes": [{"id": 1, "hardware_id": "3f13349450fa4084", "board_revision": 2, "enabled": true, "output_rate_hz": "62.5"}]
                }
            ]
        }"#).unwrap()
    }

    #[test]
    fn parses_topology_and_preserves_shared_transport_device() {
        let profile = InstallationTopologyProfile::from_document(&valid_document()).unwrap();
        assert_eq!(profile.system_name(), "rpi-sanok");
        assert_eq!(profile.channels().len(), 2);
        assert_eq!(profile.enabled_channels().count(), 2);
        assert_eq!(profile.channels()[0].transport().device_id(), "usb-rs485-1");
        assert_eq!(profile.channels()[1].transport().device_id(), "usb-rs485-1");
        assert_eq!(
            profile.channels()[1].nodes()[0].output_rate(),
            OutputRateCode::Hz62_5
        );
    }

    #[test]
    fn rejects_routing_and_identity_aliases() {
        let mut document = valid_document();
        document["channels"][1]["channel_id"] = Value::from(1);
        assert_eq!(
            InstallationTopologyProfile::from_document(&document),
            Err(TopologyConfigError::DuplicateChannelId(1))
        );

        let mut document = valid_document();
        document["channels"][1]["nodes"][0]["hardware_id"] = Value::from("2f13349450fa4084");
        assert!(matches!(
            InstallationTopologyProfile::from_document(&document),
            Err(TopologyConfigError::DuplicateHardwareId(_))
        ));

        let mut document = valid_document();
        document["channels"][1]["transport"]["endpoint"] = Value::from("/dev/line-a");
        assert_eq!(
            InstallationTopologyProfile::from_document(&document),
            Err(TopologyConfigError::DuplicateTransportEndpoint(
                PathBuf::from("/dev/line-a")
            ))
        );
    }

    #[test]
    fn rejects_unsafe_physical_and_measurement_contracts() {
        let mut document = valid_document();
        document["channels"][0]["transport"]["baud_rate"] = Value::from(230400);
        assert_eq!(
            InstallationTopologyProfile::from_document(&document),
            Err(TopologyConfigError::UnsupportedBaudRate(230400))
        );

        let mut document = valid_document();
        document["channels"][0]["nodes"][0]["board_revision"] = Value::from(3);
        assert_eq!(
            InstallationTopologyProfile::from_document(&document),
            Err(TopologyConfigError::UnsupportedBoardRevision(3))
        );

        let mut document = valid_document();
        document["channels"][0]["nodes"][0]["output_rate_hz"] = Value::from(100);
        assert!(matches!(
            InstallationTopologyProfile::from_document(&document),
            Err(TopologyConfigError::UnsupportedOutputRate { .. })
        ));
    }

    #[test]
    fn enforces_host_and_channel_bounds() {
        let mut document = valid_document();
        document["channels"] = Value::Array(Vec::new());
        assert_eq!(
            InstallationTopologyProfile::from_document(&document),
            Err(TopologyConfigError::InvalidChannelCount {
                actual: 0,
                maximum: 16
            })
        );

        let mut document = valid_document();
        document["channels"][0]["nodes"][0]["enabled"] = Value::Bool(false);
        assert_eq!(
            InstallationTopologyProfile::from_document(&document),
            Err(TopologyConfigError::EnabledChannelWithoutEnabledNode(
                "line-a".to_owned()
            ))
        );
    }

    #[test]
    fn host_profile_loads_storage_and_topology_from_one_merged_document() {
        let temp = TempDirectory::new();
        let mut document = valid_document();
        document["storage"] = serde_json::from_str(r#"{
            "root_dir":"var/recordings", "window_seconds":600, "min_free_bytes":1073741824,
            "capture_v2":{"checkpoint_dir":"var/recordings/.retention","queue_capacity_records":64,"group_commit_max_records":8,"group_commit_max_wait_ms":5,"worker_prefix":"capture","capture_grace_seconds":172800,"archive_retention_seconds":2592000},
            "time_v2":{"root_dir":"var/recordings/time","queue_capacity_records":64,"worker_name":"time","sync_interval_ms":10000,"sync_estimated_slot_ms":20,"sync_retry_delay_ms":250}
        }"#).unwrap();
        document["runtime_v2"] =
            serde_json::from_str::<Value>(include_str!("../../../../host/configs/common.json"))
                .unwrap()["runtime_v2"]
                .clone();
        fs::write(
            temp.0.join("profile.json"),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();

        let profile = HostInstallationProfile::load(temp.0.join("profile.json"), &temp.0).unwrap();
        assert_eq!(profile.system_name(), "rpi-sanok");
        assert_eq!(profile.topology().channels().len(), 2);
        assert_eq!(
            profile.runtime().supervision_interval(),
            std::time::Duration::from_millis(100)
        );
        assert_eq!(
            profile.storage().checkpoint_root(),
            temp.0.join("var/recordings/.retention")
        );
    }

    #[test]
    fn tracked_v2_profiles_remain_loadable_as_complete_host_profiles() {
        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let system = repository_root.join("host/configs/systems/rpi-sanok.json");
        let profile = HostInstallationProfile::load(&system, &repository_root).unwrap();
        assert_eq!(profile.system_name(), "rpi-sanok");
        assert_eq!(profile.topology().channels().len(), 6);
        assert_eq!(profile.topology().enabled_channels().count(), 6);
    }

    #[test]
    fn configuration_fingerprint_is_stable_and_detects_a_semantic_change() {
        let first = serde_json::json!({"b": [1, 2], "a": {"enabled": true}});
        let reordered = serde_json::json!({"a": {"enabled": true}, "b": [1, 2]});
        let changed = serde_json::json!({"a": {"enabled": false}, "b": [1, 2]});
        assert_eq!(
            fingerprint_document(&first),
            fingerprint_document(&reordered)
        );
        assert_ne!(fingerprint_document(&first), fingerprint_document(&changed));
    }
}
