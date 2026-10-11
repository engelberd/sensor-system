use core::fmt;
use std::collections::HashSet;

use sensor_core::{HardwareId, SequenceCursor};
use sensor_protocol::{
    NodeCapabilities, NodeIdentity, OutputRateCode, ServiceId, FEATURE_DRDY_TIMESTAMPS,
    FEATURE_SAMPLE_JOURNAL,
};

use crate::capture_profile::DiscoveredCaptureStream;
use crate::handshake::HandshakeRequirements;
use crate::installation_topology::{
    ChannelInstallationProfile, InstallationTopologyProfile, NodeInstallationProfile,
    TransportBinding,
};
use crate::time_profile::DiscoveredTimeStream;

pub const REQUIRED_RECORDING_FEATURES: u64 = FEATURE_SAMPLE_JOURNAL | FEATURE_DRDY_TIMESTAMPS;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DiscoveryTargetId {
    pub channel_id: u8,
    pub node_address: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeDiscoveryTarget {
    id: DiscoveryTargetId,
    hardware_id: HardwareId,
    board_revision: u8,
    output_rate: OutputRateCode,
    requirements: HandshakeRequirements,
}

impl NodeDiscoveryTarget {
    pub const fn id(self) -> DiscoveryTargetId {
        self.id
    }

    pub const fn hardware_id(self) -> HardwareId {
        self.hardware_id
    }

    pub const fn board_revision(self) -> u8 {
        self.board_revision
    }

    pub const fn output_rate(self) -> OutputRateCode {
        self.output_rate
    }

    pub const fn requirements(self) -> HandshakeRequirements {
        self.requirements
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelDiscoveryPlan {
    channel_name: String,
    channel_id: u8,
    transport: TransportBinding,
    targets: Vec<NodeDiscoveryTarget>,
}

impl ChannelDiscoveryPlan {
    pub fn channel_name(&self) -> &str {
        &self.channel_name
    }

    pub const fn channel_id(&self) -> u8 {
        self.channel_id
    }

    pub const fn transport(&self) -> &TransportBinding {
        &self.transport
    }

    pub fn targets(&self) -> &[NodeDiscoveryTarget] {
        &self.targets
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallationDiscoveryPlan {
    channels: Vec<ChannelDiscoveryPlan>,
    target_count: usize,
}

impl InstallationDiscoveryPlan {
    pub fn from_topology(topology: &InstallationTopologyProfile) -> Self {
        let channels = topology
            .enabled_channels()
            .map(channel_plan)
            .collect::<Vec<_>>();
        let target_count = channels.iter().map(|channel| channel.targets.len()).sum();
        Self {
            channels,
            target_count,
        }
    }

    pub fn channels(&self) -> &[ChannelDiscoveryPlan] {
        &self.channels
    }

    pub const fn target_count(&self) -> usize {
        self.target_count
    }

    pub fn collector(&self) -> DiscoveryInventoryCollector<'_> {
        DiscoveryInventoryCollector {
            plan: self,
            completed: HashSet::with_capacity(self.target_count),
            nodes: Vec::with_capacity(self.target_count),
        }
    }

    fn target(
        &self,
        id: DiscoveryTargetId,
    ) -> Option<(&ChannelDiscoveryPlan, NodeDiscoveryTarget)> {
        let channel = self
            .channels
            .iter()
            .find(|channel| channel.channel_id == id.channel_id)?;
        let target = channel
            .targets
            .iter()
            .copied()
            .find(|target| target.id == id)?;
        Some((channel, target))
    }
}

fn channel_plan(channel: &ChannelInstallationProfile) -> ChannelDiscoveryPlan {
    ChannelDiscoveryPlan {
        channel_name: channel.name().to_owned(),
        channel_id: channel.channel_id(),
        transport: channel.transport().clone(),
        targets: channel
            .enabled_nodes()
            .map(|node| node_target(channel.channel_id(), node))
            .collect(),
    }
}

fn node_target(channel_id: u8, node: &NodeInstallationProfile) -> NodeDiscoveryTarget {
    let mut requirements = HandshakeRequirements::recording_node(
        node.hardware_id(),
        node.node_address(),
        node.board_revision(),
        REQUIRED_RECORDING_FEATURES,
    )
    .with_output_rate(node.output_rate());
    requirements.required_services |= ServiceId::Time.capability_bit();
    NodeDiscoveryTarget {
        id: DiscoveryTargetId {
            channel_id,
            node_address: node.node_address(),
        },
        hardware_id: node.hardware_id(),
        board_revision: node.board_revision(),
        output_rate: node.output_rate(),
        requirements,
    }
}

pub struct DiscoveryInventoryCollector<'a> {
    plan: &'a InstallationDiscoveryPlan,
    completed: HashSet<DiscoveryTargetId>,
    nodes: Vec<DiscoveredNode>,
}

impl DiscoveryInventoryCollector<'_> {
    pub fn accept(
        &mut self,
        id: DiscoveryTargetId,
        identity: NodeIdentity,
        capabilities: NodeCapabilities,
    ) -> Result<(), DiscoveryError> {
        let (channel, target) = self
            .plan
            .target(id)
            .ok_or(DiscoveryError::UnexpectedTarget(id))?;
        if self.completed.contains(&id) {
            return Err(DiscoveryError::DuplicateResult(id));
        }
        if identity.node_address != id.node_address
            || identity.hardware_id != target.hardware_id
            || identity.board_revision != target.board_revision
        {
            return Err(DiscoveryError::IdentityMismatch(id));
        }
        identity
            .require_local_protocol()
            .map_err(|_| DiscoveryError::ProtocolMismatch(id))?;
        let requirements = target.requirements;
        let missing_services = requirements.required_services & !capabilities.service_flags;
        if missing_services != 0 {
            return Err(DiscoveryError::MissingServices {
                id,
                bits: missing_services,
            });
        }
        let missing_features = requirements.required_features & !capabilities.feature_flags;
        if missing_features != 0 {
            return Err(DiscoveryError::MissingFeatures {
                id,
                bits: missing_features,
            });
        }
        if !capabilities.supports_output_rate(target.output_rate) {
            return Err(DiscoveryError::UnsupportedOutputRate {
                id,
                rate: target.output_rate,
            });
        }

        self.completed.insert(id);
        self.nodes.push(DiscoveredNode {
            channel_name: channel.channel_name.clone(),
            channel_id: channel.channel_id,
            node_address: id.node_address,
            output_rate: target.output_rate,
            identity,
            capabilities,
        });
        Ok(())
    }

    pub fn finish(mut self) -> Result<DiscoveryInventory, DiscoveryError> {
        if self.completed.len() != self.plan.target_count {
            let missing = self
                .plan
                .channels
                .iter()
                .flat_map(|channel| channel.targets.iter())
                .map(|target| target.id)
                .find(|id| !self.completed.contains(id))
                .ok_or(DiscoveryError::InvariantViolation)?;
            return Err(DiscoveryError::MissingResult(missing));
        }
        self.nodes
            .sort_by_key(|node| (node.channel_id, node.node_address));
        Ok(DiscoveryInventory { nodes: self.nodes })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredNode {
    channel_name: String,
    channel_id: u8,
    node_address: u8,
    output_rate: OutputRateCode,
    identity: NodeIdentity,
    capabilities: NodeCapabilities,
}

impl DiscoveredNode {
    pub fn channel_name(&self) -> &str {
        &self.channel_name
    }

    pub const fn channel_id(&self) -> u8 {
        self.channel_id
    }

    pub const fn node_address(&self) -> u8 {
        self.node_address
    }

    pub const fn output_rate(&self) -> OutputRateCode {
        self.output_rate
    }

    pub const fn identity(&self) -> NodeIdentity {
        self.identity
    }

    pub const fn capabilities(&self) -> NodeCapabilities {
        self.capabilities
    }

    pub fn time_stream(&self) -> DiscoveredTimeStream {
        DiscoveredTimeStream {
            channel_name: self.channel_name.clone(),
            hardware_id: self.identity.hardware_id,
            node_address: self.node_address,
            boot_epoch: self.identity.boot_epoch,
        }
    }

    pub const fn capture_stream(
        &self,
        recovered_cursor: SequenceCursor,
        first_window_ordinal: u64,
    ) -> DiscoveredCaptureStream {
        DiscoveredCaptureStream {
            hardware_id: self.identity.hardware_id,
            boot_epoch: self.identity.boot_epoch,
            recovered_cursor,
            first_window_ordinal,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryInventory {
    nodes: Vec<DiscoveredNode>,
}

impl DiscoveryInventory {
    pub fn nodes(&self) -> &[DiscoveredNode] {
        &self.nodes
    }

    pub fn channel_nodes(&self, channel_id: u8) -> impl Iterator<Item = &DiscoveredNode> {
        self.nodes
            .iter()
            .filter(move |node| node.channel_id == channel_id)
    }

    pub fn time_streams(&self) -> Vec<DiscoveredTimeStream> {
        self.nodes.iter().map(DiscoveredNode::time_stream).collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryError {
    UnexpectedTarget(DiscoveryTargetId),
    DuplicateResult(DiscoveryTargetId),
    MissingResult(DiscoveryTargetId),
    IdentityMismatch(DiscoveryTargetId),
    ProtocolMismatch(DiscoveryTargetId),
    MissingServices {
        id: DiscoveryTargetId,
        bits: u32,
    },
    MissingFeatures {
        id: DiscoveryTargetId,
        bits: u64,
    },
    UnsupportedOutputRate {
        id: DiscoveryTargetId,
        rate: OutputRateCode,
    },
    InvariantViolation,
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "installation discovery error: {self:?}")
    }
}

impl std::error::Error for DiscoveryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_core::BootEpoch;
    use sensor_protocol::{
        FirmwareVersion, ProtocolVersion, CAPABILITIES_SCHEMA_VERSION, FEATURE_ATOMIC_CONFIG,
        LOCAL_PROTOCOL_VERSION, MAX_PAYLOAD_BYTES,
    };
    use serde_json::json;

    fn topology() -> InstallationTopologyProfile {
        InstallationTopologyProfile::from_document(&json!({
            "system": {"name": "host-1"},
            "channels": [
                {
                    "name": "line-a", "channel_id": 1, "enabled": true,
                    "transport": {"kind":"serial-rs485", "device_id":"adapter-1", "endpoint":"/dev/a", "baud_rate":115200},
                    "nodes": [
                        {"id":1, "hardware_id":"0101010101010101", "board_revision":2, "enabled":true, "output_rate_hz":125},
                        {"id":2, "hardware_id":"0202020202020202", "board_revision":2, "enabled":false, "output_rate_hz":62.5}
                    ]
                },
                {
                    "name": "line-b", "channel_id": 2, "enabled": false,
                    "transport": {"kind":"serial-rs485", "device_id":"adapter-1", "endpoint":"/dev/b", "baud_rate":115200},
                    "nodes": [{"id":1, "hardware_id":"0303030303030303", "board_revision":2, "enabled":true, "output_rate_hz":250}]
                }
            ]
        })).unwrap()
    }

    fn identity(target: NodeDiscoveryTarget) -> NodeIdentity {
        NodeIdentity {
            hardware_id: target.hardware_id,
            boot_epoch: BootEpoch::new(77),
            node_address: target.id.node_address,
            board_revision: target.board_revision,
            firmware: FirmwareVersion {
                major: 0,
                minor: 5,
                patch: 6,
            },
            protocol: LOCAL_PROTOCOL_VERSION,
        }
    }

    fn capabilities(target: NodeDiscoveryTarget) -> NodeCapabilities {
        NodeCapabilities {
            schema_version: CAPABILITIES_SCHEMA_VERSION,
            service_flags: target.requirements.required_services,
            feature_flags: target.requirements.required_features | FEATURE_ATOMIC_CONFIG,
            max_payload_bytes: MAX_PAYLOAD_BYTES as u16,
            journal_capacity_samples: 2048,
            output_rate_mask: target.output_rate.capability_bit(),
            max_sample_chunk_samples: 48,
        }
    }

    #[test]
    fn plan_contains_only_enabled_channels_and_nodes_with_full_recording_contract() {
        let plan = InstallationDiscoveryPlan::from_topology(&topology());
        assert_eq!(plan.channels().len(), 1);
        assert_eq!(plan.target_count(), 1);
        let target = plan.channels()[0].targets()[0];
        assert_eq!(
            target.id(),
            DiscoveryTargetId {
                channel_id: 1,
                node_address: 1
            }
        );
        assert_eq!(target.output_rate(), OutputRateCode::Hz125);
        assert_ne!(
            target.requirements().required_services & ServiceId::Time.capability_bit(),
            0
        );
        assert_eq!(
            target.requirements().required_features,
            REQUIRED_RECORDING_FEATURES
        );
    }

    #[test]
    fn complete_inventory_is_sorted_and_builds_storage_stream_identity() {
        let plan = InstallationDiscoveryPlan::from_topology(&topology());
        let target = plan.channels()[0].targets()[0];
        let mut collector = plan.collector();
        collector
            .accept(target.id(), identity(target), capabilities(target))
            .unwrap();
        let inventory = collector.finish().unwrap();
        assert_eq!(inventory.nodes().len(), 1);
        assert_eq!(
            inventory.nodes()[0].time_stream().boot_epoch,
            BootEpoch::new(77)
        );
        assert_eq!(
            inventory.nodes()[0]
                .capture_stream(SequenceCursor::new(19), 3)
                .recovered_cursor,
            SequenceCursor::new(19)
        );
    }

    #[test]
    fn incomplete_or_mismatched_discovery_fails_closed() {
        let plan = InstallationDiscoveryPlan::from_topology(&topology());
        let target = plan.channels()[0].targets()[0];
        assert_eq!(
            plan.collector().finish(),
            Err(DiscoveryError::MissingResult(target.id()))
        );

        let mut wrong_identity = identity(target);
        wrong_identity.protocol = ProtocolVersion::new(3, 4);
        assert_eq!(
            plan.collector()
                .accept(target.id(), wrong_identity, capabilities(target)),
            Err(DiscoveryError::ProtocolMismatch(target.id()))
        );

        let mut weak = capabilities(target);
        weak.feature_flags &= !FEATURE_DRDY_TIMESTAMPS;
        assert_eq!(
            plan.collector().accept(target.id(), identity(target), weak),
            Err(DiscoveryError::MissingFeatures {
                id: target.id(),
                bits: FEATURE_DRDY_TIMESTAMPS
            })
        );
    }
}
