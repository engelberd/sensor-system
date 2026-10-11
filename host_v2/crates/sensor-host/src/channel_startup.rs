use core::fmt;

use crate::channel_admission::{
    AdmissionError, AdmissionPolicy, AdmissionReport, ChannelAdmission, NodeTrafficDemand,
    PeriodicTraffic,
};
use sensor_protocol::{ObservabilityProfile, RuntimeState};

use crate::channel_recovery::RecoveredNode;
use crate::channel_runtime::{
    ChannelRuntime, ChannelRuntimeError, NodeScheduleConfig, TimeSyncScheduleConfig,
};
use crate::channel_scheduler::{ChannelScheduler, ChannelSchedulerPolicy};
use crate::discovery::{DiscoveredNode, DiscoveryInventory};
use crate::measurement_coordinator::{ChannelWorkCoordinator, MeasurementCoordinatorError};
use crate::transaction::{DurationMicros, MonotonicMicros};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelStartupPolicy {
    pub admission: AdmissionPolicy,
    pub target_chunk_samples: u16,
    /// Conservative measured cost of one maximum Measurement exchange.
    pub measurement_transaction_time: DurationMicros,
    /// Scheduler reservation including turnaround and host-side margin.
    pub measurement_slot: DurationMicros,
    pub maximum_service_interval: DurationMicros,
    pub retry_base_delay: DurationMicros,
    pub retry_max_delay: DurationMicros,
    pub retry_attempt_limit: u8,
    pub offline_probe_interval: DurationMicros,
    pub time_sync_schedule: TimeSyncScheduleConfig,
    /// Conservative measured cost used only by admission control.
    pub time_sync_transaction_time: DurationMicros,
}

impl ChannelStartupPolicy {
    pub fn validate(self) -> Result<Self, ChannelStartupError> {
        ChannelAdmission::new(self.admission).map_err(ChannelStartupError::Admission)?;
        if self.target_chunk_samples == 0 {
            return Err(ChannelStartupError::ZeroTargetChunk);
        }
        if self.measurement_transaction_time.get() > self.measurement_slot.get() {
            return Err(ChannelStartupError::TransactionExceedsSlot);
        }
        if self.time_sync_transaction_time.get() > self.time_sync_schedule.estimated_slot.get() {
            return Err(ChannelStartupError::TimeTransactionExceedsSlot);
        }
        self.time_sync_schedule
            .validate()
            .map_err(ChannelStartupError::Runtime)?;
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeStartupPlan {
    pub discovered: DiscoveredNodeRef,
    pub schedule: NodeScheduleConfig,
    pub time_sync: TimeSyncScheduleConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveredNodeRef {
    pub channel_id: u8,
    pub node_address: u8,
    pub hardware_id: sensor_core::HardwareId,
    pub boot_epoch: sensor_core::BootEpoch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelStartupPlan {
    channel_id: u8,
    channel_name: String,
    admission: AdmissionReport,
    nodes: Vec<NodeStartupPlan>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordingAssemblyPolicy {
    pub scheduler: ChannelSchedulerPolicy,
    pub first_time_sync_delay: DurationMicros,
    pub time_sync_phase_step: DurationMicros,
}

impl ChannelStartupPlan {
    pub const fn channel_id(&self) -> u8 {
        self.channel_id
    }

    pub fn channel_name(&self) -> &str {
        &self.channel_name
    }

    pub const fn admission(&self) -> AdmissionReport {
        self.admission
    }

    pub fn nodes(&self) -> &[NodeStartupPlan] {
        &self.nodes
    }
}

/// Converts immutable discovery evidence into bounded per-channel scheduler
/// inputs and rejects a channel whose measured traffic budget does not fit.
pub fn build_channel_startup_plans(
    inventory: &DiscoveryInventory,
    policy: ChannelStartupPolicy,
) -> Result<Vec<ChannelStartupPlan>, ChannelStartupError> {
    let policy = policy.validate()?;
    let admission =
        ChannelAdmission::new(policy.admission).map_err(ChannelStartupError::Admission)?;
    let mut plans = Vec::new();

    for discovered in inventory.nodes() {
        if plans.last().map_or(true, |plan: &ChannelStartupPlan| {
            plan.channel_id != discovered.channel_id()
        }) {
            plans.push(ChannelStartupPlan {
                channel_id: discovered.channel_id(),
                channel_name: discovered.channel_name().to_owned(),
                admission: AdmissionReport {
                    nominal_utilization_ppm: 0,
                    recovery_reserve_ppm: policy.admission.recovery_reserve_ppm,
                    required_utilization_ppm: policy.admission.recovery_reserve_ppm,
                    maximum_utilization_ppm: policy.admission.maximum_utilization_ppm,
                },
                nodes: Vec::new(),
            });
        }
        let plan = plans
            .last_mut()
            .ok_or(ChannelStartupError::InvariantViolation)?;
        if plan.channel_name != discovered.channel_name() {
            return Err(ChannelStartupError::InconsistentChannelIdentity(
                discovered.channel_id(),
            ));
        }
        plan.nodes.push(node_plan(discovered, policy)?);
    }

    for plan in &mut plans {
        let demands = plan
            .nodes
            .iter()
            .map(|node| NodeTrafficDemand {
                output_rate: node.schedule.output_rate,
                samples_per_chunk: node.schedule.target_chunk_samples,
                measurement_transaction_time: policy.measurement_transaction_time,
                time_sync: Some(PeriodicTraffic {
                    transaction_time: policy.time_sync_transaction_time,
                    interval: policy.time_sync_schedule.interval,
                }),
                // Health Service is deliberately absent until its wire executor
                // exists. Adding it requires a measured, explicit traffic term.
                health: None,
            })
            .collect::<Vec<_>>();
        plan.admission = admission
            .evaluate(&demands)
            .map_err(ChannelStartupError::Admission)?;
        if !plan.admission.accepted() {
            return Err(ChannelStartupError::ChannelOverCapacity {
                channel_id: plan.channel_id,
                report: plan.admission,
            });
        }
    }

    Ok(plans)
}

/// Creates the in-memory recording coordinator only after every session has
/// been recovered and every node is confirmed in Production Recording. No
/// cursor is accepted as a separate argument: the scheduler is initialized
/// from the cursor already owned by each `NodeSession`.
pub fn assemble_recording_coordinator(
    plan: &ChannelStartupPlan,
    mut recovered: Vec<RecoveredNode>,
    policy: RecordingAssemblyPolicy,
    now: MonotonicMicros,
    work_id_seed: u64,
) -> Result<ChannelWorkCoordinator, ChannelAssemblyError> {
    if recovered.len() != plan.nodes.len() {
        return Err(ChannelAssemblyError::NodeCountMismatch {
            planned: plan.nodes.len(),
            recovered: recovered.len(),
        });
    }
    let runtime = ChannelRuntime::new(
        ChannelScheduler::new(policy.scheduler),
        ObservabilityProfile::Production,
        plan.nodes.len(),
        work_id_seed,
    )
    .map_err(ChannelAssemblyError::Runtime)?;
    let mut coordinator = ChannelWorkCoordinator::new(runtime, plan.channel_id());

    for (index, planned) in plan.nodes.iter().enumerate() {
        let position = recovered
            .iter()
            .position(|node| {
                node.id().channel_id == planned.discovered.channel_id
                    && node.id().node_address == planned.discovered.node_address
            })
            .ok_or(ChannelAssemblyError::MissingRecoveredNode {
                channel_id: planned.discovered.channel_id,
                node_address: planned.discovered.node_address,
            })?;
        let node = recovered.swap_remove(position);
        if node.runtime().state != RuntimeState::Recording
            || node.runtime().observability != ObservabilityProfile::Production
        {
            return Err(ChannelAssemblyError::NodeNotRecording {
                node_address: node.id().node_address,
            });
        }
        if node.session().hardware_id() != planned.discovered.hardware_id
            || node.session().boot_epoch() != planned.discovered.boot_epoch
        {
            return Err(ChannelAssemblyError::SessionIdentityMismatch {
                node_address: node.id().node_address,
            });
        }
        coordinator
            .register_node(planned.schedule, node.into_session(), now)
            .map_err(ChannelAssemblyError::Coordinator)?;

        let phase = policy
            .time_sync_phase_step
            .get()
            .checked_mul(u64::try_from(index).map_err(|_| ChannelAssemblyError::TimeOverflow)?)
            .and_then(|value| value.checked_add(policy.first_time_sync_delay.get()))
            .ok_or(ChannelAssemblyError::TimeOverflow)?;
        let first_deadline = now
            .checked_add(
                DurationMicros::new(phase).map_err(|_| ChannelAssemblyError::TimeOverflow)?,
            )
            .ok_or(ChannelAssemblyError::TimeOverflow)?;
        coordinator
            .configure_time_sync(
                planned.discovered.node_address,
                planned.time_sync,
                first_deadline,
            )
            .map_err(ChannelAssemblyError::Coordinator)?;
    }
    Ok(coordinator)
}

#[derive(Debug)]
pub enum ChannelAssemblyError {
    NodeCountMismatch { planned: usize, recovered: usize },
    MissingRecoveredNode { channel_id: u8, node_address: u8 },
    NodeNotRecording { node_address: u8 },
    SessionIdentityMismatch { node_address: u8 },
    TimeOverflow,
    Runtime(ChannelRuntimeError),
    Coordinator(MeasurementCoordinatorError),
}

impl fmt::Display for ChannelAssemblyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "channel recording assembly error: {self:?}")
    }
}

impl std::error::Error for ChannelAssemblyError {}

fn node_plan(
    discovered: &DiscoveredNode,
    policy: ChannelStartupPolicy,
) -> Result<NodeStartupPlan, ChannelStartupError> {
    let identity = discovered.identity();
    let capabilities = discovered.capabilities();
    let target_chunk_samples = policy
        .target_chunk_samples
        .min(capabilities.max_sample_chunk_samples);
    let schedule = NodeScheduleConfig {
        node_address: discovered.node_address(),
        output_rate: discovered.output_rate().sample_rate(),
        journal_capacity_samples: capabilities.journal_capacity_samples,
        max_sample_chunk_samples: capabilities.max_sample_chunk_samples,
        target_chunk_samples,
        measurement_slot: policy.measurement_slot,
        maximum_service_interval: policy.maximum_service_interval,
        retry_base_delay: policy.retry_base_delay,
        retry_max_delay: policy.retry_max_delay,
        retry_attempt_limit: policy.retry_attempt_limit,
        offline_probe_interval: policy.offline_probe_interval,
    }
    .validate()
    .map_err(ChannelStartupError::Runtime)?;
    Ok(NodeStartupPlan {
        discovered: DiscoveredNodeRef {
            channel_id: discovered.channel_id(),
            node_address: discovered.node_address(),
            hardware_id: identity.hardware_id,
            boot_epoch: identity.boot_epoch,
        },
        schedule,
        time_sync: policy.time_sync_schedule,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelStartupError {
    ZeroTargetChunk,
    TransactionExceedsSlot,
    TimeTransactionExceedsSlot,
    InconsistentChannelIdentity(u8),
    ChannelOverCapacity {
        channel_id: u8,
        report: AdmissionReport,
    },
    Admission(AdmissionError),
    Runtime(ChannelRuntimeError),
    InvariantViolation,
}

impl fmt::Display for ChannelStartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "channel startup planning error: {self:?}")
    }
}

impl std::error::Error for ChannelStartupError {}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_core::BootEpoch;
    use sensor_protocol::{
        FirmwareVersion, NodeCapabilities, NodeIdentity, OutputRateCode, RuntimeResult,
        RuntimeResultCode, RuntimeState, SensorStatusCode, ServiceId, CAPABILITIES_SCHEMA_VERSION,
        LOCAL_PROTOCOL_VERSION, MAX_PAYLOAD_BYTES,
    };

    use crate::channel_recovery::RecoveredNode;
    use crate::channel_scheduler::ChannelSchedulerPolicy;
    use crate::discovery::{
        DiscoveryTargetId, InstallationDiscoveryPlan, REQUIRED_RECORDING_FEATURES,
    };
    use crate::installation_topology::InstallationTopologyProfile;
    use crate::node_session::NodeSession;
    use crate::stream_recovery::RecoveryContinuity;
    use serde_json::json;

    fn duration(value: u64) -> DurationMicros {
        DurationMicros::new(value).unwrap()
    }

    fn policy(maximum_utilization_ppm: u32) -> ChannelStartupPolicy {
        ChannelStartupPolicy {
            admission: AdmissionPolicy {
                maximum_utilization_ppm,
                recovery_reserve_ppm: 100_000,
            },
            target_chunk_samples: 32,
            measurement_transaction_time: duration(36_900),
            measurement_slot: duration(40_000),
            maximum_service_interval: duration(500_000),
            retry_base_delay: duration(10_000),
            retry_max_delay: duration(1_000_000),
            retry_attempt_limit: 3,
            offline_probe_interval: duration(5_000_000),
            time_sync_schedule: TimeSyncScheduleConfig::new(
                duration(30_000_000),
                duration(4_000),
                duration(250_000),
            )
            .unwrap(),
            time_sync_transaction_time: duration(4_000),
        }
    }

    fn inventory(node_count: u8, output_rate: OutputRateCode) -> DiscoveryInventory {
        let nodes = (1..=node_count)
            .map(|address| {
                json!({
                    "id": address,
                    "hardware_id": format!("{:02x}01010101010101", address),
                    "board_revision": 2,
                    "enabled": true,
                    "output_rate_hz": output_rate.hertz_label()
                })
            })
            .collect::<Vec<_>>();
        let topology = InstallationTopologyProfile::from_document(&json!({
            "system": {"name":"test-host"},
            "channels":[{
                "name":"line-a", "channel_id":1, "enabled":true,
                "transport":{"kind":"serial-rs485", "device_id":"adapter-1", "endpoint":"/dev/a", "baud_rate":115200},
                "nodes":nodes
            }]
        }))
        .unwrap();
        let plan = InstallationDiscoveryPlan::from_topology(&topology);
        let mut collector = plan.collector();
        for target in plan.channels()[0].targets() {
            let identity = NodeIdentity {
                hardware_id: target.hardware_id(),
                boot_epoch: BootEpoch::new(100 + u64::from(target.id().node_address)),
                node_address: target.id().node_address,
                board_revision: target.board_revision(),
                firmware: FirmwareVersion {
                    major: 0,
                    minor: 5,
                    patch: 6,
                },
                protocol: LOCAL_PROTOCOL_VERSION,
            };
            let capabilities = NodeCapabilities {
                schema_version: CAPABILITIES_SCHEMA_VERSION,
                service_flags: ServiceId::Identity.capability_bit()
                    | ServiceId::Measurement.capability_bit()
                    | ServiceId::Lifecycle.capability_bit()
                    | ServiceId::Time.capability_bit(),
                feature_flags: REQUIRED_RECORDING_FEATURES,
                max_payload_bytes: MAX_PAYLOAD_BYTES as u16,
                journal_capacity_samples: 2048,
                output_rate_mask: output_rate.capability_bit(),
                max_sample_chunk_samples: 48,
            };
            collector
                .accept(
                    DiscoveryTargetId {
                        channel_id: 1,
                        node_address: target.id().node_address,
                    },
                    identity,
                    capabilities,
                )
                .unwrap();
        }
        collector.finish().unwrap()
    }

    #[test]
    fn four_nodes_at_125_hz_build_bounded_scheduler_inputs() {
        let plans =
            build_channel_startup_plans(&inventory(4, OutputRateCode::Hz125), policy(700_000))
                .unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].nodes().len(), 4);
        assert_eq!(plans[0].admission().required_utilization_ppm, 677_100);
        assert!(plans[0].admission().accepted());
        assert!(plans[0]
            .nodes()
            .iter()
            .all(|node| node.schedule.target_chunk_samples == 32));
    }

    #[test]
    fn overcommitted_channel_is_rejected_before_runtime_start() {
        assert!(matches!(
            build_channel_startup_plans(&inventory(4, OutputRateCode::Hz125), policy(600_000)),
            Err(ChannelStartupError::ChannelOverCapacity { channel_id: 1, .. })
        ));
    }

    #[test]
    fn node_capability_can_only_reduce_the_requested_chunk() {
        let plans = build_channel_startup_plans(
            &inventory(1, OutputRateCode::Hz250),
            ChannelStartupPolicy {
                target_chunk_samples: 64,
                ..policy(900_000)
            },
        )
        .unwrap();
        assert_eq!(plans[0].nodes()[0].schedule.max_sample_chunk_samples, 48);
        assert_eq!(plans[0].nodes()[0].schedule.target_chunk_samples, 48);
    }

    #[test]
    fn assembly_uses_only_recovered_session_cursors_and_matches_identity() {
        let plans =
            build_channel_startup_plans(&inventory(2, OutputRateCode::Hz125), policy(900_000))
                .unwrap();
        let mut recovered = plans[0]
            .nodes()
            .iter()
            .map(|planned| {
                RecoveredNode::from_parts(
                    DiscoveryTargetId {
                        channel_id: planned.discovered.channel_id,
                        node_address: planned.discovered.node_address,
                    },
                    RuntimeResult {
                        operation_id: 7,
                        code: RuntimeResultCode::Ok,
                        state: RuntimeState::Recording,
                        observability: ObservabilityProfile::Production,
                        sensor_status: SensorStatusCode::Ok,
                    },
                    RecoveryContinuity::Contiguous,
                    NodeSession::recovered(
                        planned.discovered.hardware_id,
                        planned.discovered.boot_epoch,
                        sensor_core::SequenceCursor::new(
                            100 + u64::from(planned.discovered.node_address),
                        ),
                        sensor_core::SequenceCursor::new(90),
                    )
                    .unwrap(),
                )
            })
            .collect::<Vec<_>>();
        recovered.reverse();

        let now = MonotonicMicros::new(5_000);
        let coordinator = assemble_recording_coordinator(
            &plans[0],
            recovered,
            RecordingAssemblyPolicy {
                scheduler: ChannelSchedulerPolicy {
                    overflow_guard: duration(100_000),
                    max_slot: duration(50_000),
                },
                first_time_sync_delay: duration(1_000_000),
                time_sync_phase_step: duration(100_000),
            },
            now,
            44,
        )
        .unwrap();

        assert_eq!(coordinator.runtime().node_count(), 2);
        for planned in plans[0].nodes() {
            assert_eq!(
                coordinator
                    .runtime()
                    .snapshot(planned.discovered.node_address, now)
                    .unwrap()
                    .received,
                sensor_core::SequenceCursor::new(100 + u64::from(planned.discovered.node_address))
            );
        }
    }
}
