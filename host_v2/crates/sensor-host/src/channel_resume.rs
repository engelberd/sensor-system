use core::fmt;

use sensor_core::BootEpoch;
use sensor_protocol::{
    NodeCapabilities, NodeIdentity, ObservabilityProfile, RuntimeOperation, RuntimeResult,
    RuntimeResultCode, StreamState,
};

use crate::channel_agent::{ChannelAgent, ChannelAgentError, ChannelEvent, ControlWorkKey};
use crate::handshake::HandshakeRequirements;
use crate::transaction::{DurationMicros, MonotonicMicros};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResumeVerificationPolicy {
    /// One bound covers handshake, GetState and GetStreamState for one node.
    /// Internal transaction retries never extend it.
    pub node_deadline: DurationMicros,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResumeNodeContract {
    pub requirements: HandshakeRequirements,
    pub previous_boot_epoch: BootEpoch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResumeNodeEvidence {
    pub identity: NodeIdentity,
    pub capabilities: NodeCapabilities,
    pub runtime: RuntimeResult,
    pub stream: StreamState,
    pub stream_recovery_required: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResumeNodePhase {
    NeedHandshake,
    Handshake,
    NeedRuntime,
    Runtime { operation_id: u32 },
    NeedStreamState,
    StreamState,
    Complete,
}

struct ResumeNodeVerification {
    contract: ResumeNodeContract,
    deadline: MonotonicMicros,
    phase: ResumeNodePhase,
    identity: Option<NodeIdentity>,
    capabilities: Option<NodeCapabilities>,
    runtime: Option<RuntimeResult>,
    evidence: Option<ResumeNodeEvidence>,
}

impl ResumeNodeVerification {
    fn new(
        contract: ResumeNodeContract,
        deadline: MonotonicMicros,
    ) -> Result<Self, ResumeVerificationError> {
        if contract.previous_boot_epoch.get() == 0 {
            return Err(ResumeVerificationError::ZeroBootEpoch);
        }
        Ok(Self {
            contract,
            deadline,
            phase: ResumeNodePhase::NeedHandshake,
            identity: None,
            capabilities: None,
            runtime: None,
            evidence: None,
        })
    }

    fn drive(
        &mut self,
        agent: &mut ChannelAgent,
        now: MonotonicMicros,
    ) -> Result<ResumeVerificationProgress, ResumeVerificationError> {
        if now >= self.deadline {
            return self.expire(agent);
        }
        let node_address = self.contract.requirements.node_address;
        match self.phase {
            ResumeNodePhase::NeedHandshake => {
                ensure_exclusive(agent)?;
                agent.submit_handshake(self.contract.requirements)?;
                self.phase = ResumeNodePhase::Handshake;
                Ok(ResumeVerificationProgress::Submitted { node_address })
            }
            ResumeNodePhase::NeedRuntime => {
                ensure_exclusive(agent)?;
                let operation_id = agent.submit_lifecycle(
                    node_address,
                    RuntimeOperation::GetState,
                    ObservabilityProfile::Production,
                )?;
                self.phase = ResumeNodePhase::Runtime { operation_id };
                Ok(ResumeVerificationProgress::Submitted { node_address })
            }
            ResumeNodePhase::NeedStreamState => {
                ensure_exclusive(agent)?;
                agent.submit_stream_state(node_address)?;
                self.phase = ResumeNodePhase::StreamState;
                Ok(ResumeVerificationProgress::Submitted { node_address })
            }
            ResumeNodePhase::Complete => Ok(ResumeVerificationProgress::NodeVerified {
                node_address,
                evidence: self
                    .evidence
                    .ok_or(ResumeVerificationError::InvariantViolation)?,
            }),
            ResumeNodePhase::Handshake
            | ResumeNodePhase::Runtime { .. }
            | ResumeNodePhase::StreamState => {
                Ok(ResumeVerificationProgress::Waiting { node_address })
            }
        }
    }

    fn handle_event(&mut self, event: &ChannelEvent) -> Result<(), ResumeVerificationError> {
        let node_address = self.contract.requirements.node_address;
        match (self.phase, event) {
            (
                ResumeNodePhase::Handshake,
                ChannelEvent::HandshakeComplete {
                    node_address: observed,
                    identity,
                    capabilities,
                },
            ) if *observed == node_address => {
                self.identity = Some(*identity);
                self.capabilities = Some(*capabilities);
                self.phase = ResumeNodePhase::NeedRuntime;
            }
            (
                ResumeNodePhase::Runtime { operation_id },
                ChannelEvent::LifecycleComplete {
                    node_address: observed,
                    result,
                },
            ) if *observed == node_address && result.operation_id == operation_id => {
                if result.code != RuntimeResultCode::Ok {
                    return Err(ResumeVerificationError::RuntimeRejected {
                        node_address,
                        code: result.code,
                    });
                }
                self.runtime = Some(*result);
                self.phase = ResumeNodePhase::NeedStreamState;
            }
            (
                ResumeNodePhase::StreamState,
                ChannelEvent::StreamState {
                    node_address: observed,
                    state,
                },
            ) if *observed == node_address => {
                let identity = self
                    .identity
                    .ok_or(ResumeVerificationError::InvariantViolation)?;
                let capabilities = self
                    .capabilities
                    .ok_or(ResumeVerificationError::InvariantViolation)?;
                let runtime = self
                    .runtime
                    .ok_or(ResumeVerificationError::InvariantViolation)?;
                self.evidence = Some(validate_evidence(
                    self.contract,
                    identity,
                    capabilities,
                    runtime,
                    *state,
                )?);
                self.phase = ResumeNodePhase::Complete;
            }
            (
                ResumeNodePhase::Runtime { operation_id },
                ChannelEvent::LifecycleNotAccepted {
                    node_address: observed,
                    operation_id: rejected,
                },
            ) if *observed == node_address && *rejected == operation_id => {
                return Err(ResumeVerificationError::RuntimeNotAccepted(node_address));
            }
            (
                ResumeNodePhase::StreamState,
                ChannelEvent::StreamStateNotAccepted {
                    node_address: observed,
                },
            ) if *observed == node_address => {
                return Err(ResumeVerificationError::StreamStateNotAccepted(
                    node_address,
                ));
            }
            _ => {}
        }
        Ok(())
    }

    fn expire(
        &mut self,
        agent: &mut ChannelAgent,
    ) -> Result<ResumeVerificationProgress, ResumeVerificationError> {
        let node_address = self.contract.requirements.node_address;
        let key = match self.phase {
            ResumeNodePhase::Handshake => Some(ControlWorkKey::Handshake { node_address }),
            ResumeNodePhase::Runtime { operation_id } => Some(ControlWorkKey::Lifecycle {
                node_address,
                operation_id,
            }),
            ResumeNodePhase::StreamState => Some(ControlWorkKey::StreamState { node_address }),
            ResumeNodePhase::NeedHandshake
            | ResumeNodePhase::NeedRuntime
            | ResumeNodePhase::NeedStreamState => None,
            ResumeNodePhase::Complete => {
                return Ok(ResumeVerificationProgress::NodeVerified {
                    node_address,
                    evidence: self
                        .evidence
                        .ok_or(ResumeVerificationError::InvariantViolation)?,
                });
            }
        };
        if let Some(key) = key {
            match agent.abandon_control_work(key) {
                Ok(_) => {}
                Err(ChannelAgentError::ControlAttemptInFlight(_)) => {
                    return Ok(ResumeVerificationProgress::AwaitingWireSettlement { node_address });
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(ResumeVerificationProgress::NodeOffline { node_address })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResumeVerificationProgress {
    Submitted {
        node_address: u8,
    },
    Waiting {
        node_address: u8,
    },
    AwaitingWireSettlement {
        node_address: u8,
    },
    NodeVerified {
        node_address: u8,
        evidence: ResumeNodeEvidence,
    },
    NodeOffline {
        node_address: u8,
    },
    Complete,
}

/// Sequential verification owner for one half-duplex channel. It submits at
/// most one control transaction at a time and retains all evidence needed by
/// stream reconciliation and admission.
pub struct ResumeVerificationGroup {
    policy: ResumeVerificationPolicy,
    contracts: Vec<ResumeNodeContract>,
    current: usize,
    active: Option<ResumeNodeVerification>,
    evidence: Vec<ResumeNodeEvidence>,
}

impl ResumeVerificationGroup {
    pub fn new(
        contracts: &[ResumeNodeContract],
        policy: ResumeVerificationPolicy,
    ) -> Result<Self, ResumeVerificationError> {
        if policy.node_deadline.get() == 0 {
            return Err(ResumeVerificationError::ZeroNodeDeadline);
        }
        if contracts.is_empty() {
            return Err(ResumeVerificationError::EmptyChannel);
        }
        let mut owned = Vec::with_capacity(contracts.len());
        for contract in contracts {
            let address = contract.requirements.node_address;
            if owned
                .iter()
                .any(|existing: &ResumeNodeContract| existing.requirements.node_address == address)
            {
                return Err(ResumeVerificationError::DuplicateNodeAddress(address));
            }
            if contract.previous_boot_epoch.get() == 0 {
                return Err(ResumeVerificationError::ZeroBootEpoch);
            }
            owned.push(*contract);
        }
        Ok(Self {
            policy,
            contracts: owned,
            current: 0,
            active: None,
            evidence: Vec::with_capacity(contracts.len()),
        })
    }

    pub fn drive(
        &mut self,
        agent: &mut ChannelAgent,
        now: MonotonicMicros,
    ) -> Result<ResumeVerificationProgress, ResumeVerificationError> {
        if self.current == self.contracts.len() {
            return Ok(ResumeVerificationProgress::Complete);
        }
        if self.active.is_none() {
            let deadline = now
                .checked_add(self.policy.node_deadline)
                .ok_or(ResumeVerificationError::TimeOverflow)?;
            self.active = Some(ResumeNodeVerification::new(
                self.contracts[self.current],
                deadline,
            )?);
        }
        let progress = self
            .active
            .as_mut()
            .ok_or(ResumeVerificationError::InvariantViolation)?
            .drive(agent, now)?;
        if let ResumeVerificationProgress::NodeVerified { evidence, .. } = progress {
            self.evidence.push(evidence);
            self.current += 1;
            self.active = None;
        }
        Ok(progress)
    }

    pub fn handle_event(&mut self, event: &ChannelEvent) -> Result<(), ResumeVerificationError> {
        if let Some(active) = &mut self.active {
            active.handle_event(event)?;
        }
        Ok(())
    }

    pub fn evidence(&self) -> &[ResumeNodeEvidence] {
        &self.evidence
    }

    pub fn stream_recovery_required(&self) -> Result<bool, ResumeVerificationError> {
        if self.current != self.contracts.len() || self.active.is_some() {
            return Err(ResumeVerificationError::VerificationIncomplete);
        }
        Ok(self
            .evidence
            .iter()
            .any(|evidence| evidence.stream_recovery_required))
    }
}

fn validate_evidence(
    contract: ResumeNodeContract,
    identity: NodeIdentity,
    capabilities: NodeCapabilities,
    runtime: RuntimeResult,
    stream: StreamState,
) -> Result<ResumeNodeEvidence, ResumeVerificationError> {
    let node_address = contract.requirements.node_address;
    if stream.boot_epoch != identity.boot_epoch {
        return Err(ResumeVerificationError::StreamEpochMismatch(node_address));
    }
    if stream.capacity_samples != capabilities.journal_capacity_samples {
        return Err(ResumeVerificationError::StreamCapacityMismatch {
            node_address,
            advertised: capabilities.journal_capacity_samples,
            observed: stream.capacity_samples,
        });
    }
    Ok(ResumeNodeEvidence {
        identity,
        capabilities,
        runtime,
        stream,
        stream_recovery_required: identity.boot_epoch != contract.previous_boot_epoch,
    })
}

fn ensure_exclusive(agent: &ChannelAgent) -> Result<(), ResumeVerificationError> {
    if agent.queued_work_items() == 0 {
        Ok(())
    } else {
        Err(ResumeVerificationError::ChannelNotExclusive)
    }
}

#[derive(Debug)]
pub enum ResumeVerificationError {
    ZeroNodeDeadline,
    EmptyChannel,
    DuplicateNodeAddress(u8),
    ZeroBootEpoch,
    TimeOverflow,
    ChannelNotExclusive,
    RuntimeNotAccepted(u8),
    RuntimeRejected {
        node_address: u8,
        code: RuntimeResultCode,
    },
    StreamStateNotAccepted(u8),
    StreamEpochMismatch(u8),
    StreamCapacityMismatch {
        node_address: u8,
        advertised: u32,
        observed: u32,
    },
    VerificationIncomplete,
    InvariantViolation,
    Agent(ChannelAgentError),
}

impl From<ChannelAgentError> for ResumeVerificationError {
    fn from(value: ChannelAgentError) -> Self {
        Self::Agent(value)
    }
}

impl fmt::Display for ResumeVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "resume verification error: {self:?}")
    }
}

impl std::error::Error for ResumeVerificationError {}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_core::HardwareId;
    use sensor_protocol::{
        FirmwareVersion, ProtocolVersion, RuntimeState, SensorStatusCode, LOCAL_PROTOCOL_VERSION,
    };

    fn contract(address: u8, epoch: u64) -> ResumeNodeContract {
        ResumeNodeContract {
            requirements: HandshakeRequirements::recording_node(
                HardwareId::from_bytes([address; 8]),
                address,
                2,
                0,
            ),
            previous_boot_epoch: BootEpoch::new(epoch),
        }
    }

    fn identity(address: u8, epoch: u64) -> NodeIdentity {
        NodeIdentity {
            protocol: ProtocolVersion {
                framing: LOCAL_PROTOCOL_VERSION.framing,
                application: LOCAL_PROTOCOL_VERSION.application,
            },
            firmware: FirmwareVersion {
                major: 0,
                minor: 5,
                patch: 8,
            },
            hardware_id: HardwareId::from_bytes([address; 8]),
            board_revision: 2,
            node_address: address,
            boot_epoch: BootEpoch::new(epoch),
        }
    }

    fn capabilities(capacity: u32) -> NodeCapabilities {
        NodeCapabilities {
            schema_version: 2,
            service_flags: 0,
            feature_flags: 0,
            max_payload_bytes: 512,
            journal_capacity_samples: capacity,
            max_sample_chunk_samples: 48,
            output_rate_mask: 0,
        }
    }

    fn runtime() -> RuntimeResult {
        RuntimeResult {
            operation_id: 7,
            code: RuntimeResultCode::Ok,
            state: RuntimeState::Maintenance,
            observability: ObservabilityProfile::Production,
            sensor_status: SensorStatusCode::Ok,
        }
    }

    fn stream(epoch: u64, capacity: u32) -> StreamState {
        StreamState {
            boot_epoch: BootEpoch::new(epoch),
            retained_from: sensor_core::SequenceCursor::new(10),
            produced: sensor_core::SequenceCursor::new(20),
            released: sensor_core::SequenceCursor::new(10),
            capacity_samples: capacity,
            overwritten_samples: 0,
        }
    }

    #[test]
    fn unchanged_epoch_uses_the_existing_stream() {
        let evidence = validate_evidence(
            contract(1, 100),
            identity(1, 100),
            capabilities(2048),
            runtime(),
            stream(100, 2048),
        )
        .unwrap();
        assert!(!evidence.stream_recovery_required);
    }

    #[test]
    fn changed_epoch_requires_a_new_stream() {
        let evidence = validate_evidence(
            contract(1, 100),
            identity(1, 101),
            capabilities(2048),
            runtime(),
            stream(101, 2048),
        )
        .unwrap();
        assert!(evidence.stream_recovery_required);
    }

    #[test]
    fn identity_stream_mismatch_and_capacity_drift_fail_closed() {
        assert!(matches!(
            validate_evidence(
                contract(1, 100),
                identity(1, 101),
                capabilities(2048),
                runtime(),
                stream(100, 2048),
            ),
            Err(ResumeVerificationError::StreamEpochMismatch(1))
        ));
        assert!(matches!(
            validate_evidence(
                contract(1, 100),
                identity(1, 100),
                capabilities(2048),
                runtime(),
                stream(100, 1024),
            ),
            Err(ResumeVerificationError::StreamCapacityMismatch { .. })
        ));
    }

    #[test]
    fn group_rejects_ambiguous_membership_and_incomplete_results() {
        let policy = ResumeVerificationPolicy {
            node_deadline: DurationMicros::new(1_000).unwrap(),
        };
        assert!(matches!(
            ResumeVerificationGroup::new(&[], policy),
            Err(ResumeVerificationError::EmptyChannel)
        ));
        assert!(matches!(
            ResumeVerificationGroup::new(&[contract(1, 100), contract(1, 100)], policy),
            Err(ResumeVerificationError::DuplicateNodeAddress(1))
        ));
        let group = ResumeVerificationGroup::new(&[contract(1, 100)], policy).unwrap();
        assert!(matches!(
            group.stream_recovery_required(),
            Err(ResumeVerificationError::VerificationIncomplete)
        ));
    }
}
