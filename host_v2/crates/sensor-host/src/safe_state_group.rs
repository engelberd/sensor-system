use core::fmt;

use sensor_core::BootEpoch;

use crate::channel_agent::{ChannelAgent, ChannelEvent};
use crate::handshake::HandshakeRequirements;
use crate::safe_state::{
    SafeStateEventDisposition, SafeStatePolicy, SafeStateTarget, SafeStateWireError,
    SafeStateWireOperation, SafeStateWirePolicy, SafeStateWireProgress,
};
use crate::transaction::MonotonicMicros;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SafeStateNodeContract {
    pub requirements: HandshakeRequirements,
    pub boot_epoch: BootEpoch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeStateGroupProgress {
    Node {
        node_address: u8,
        progress: SafeStateWireProgress,
    },
    NodeReached {
        node_address: u8,
        target: SafeStateTarget,
    },
    NodeOffline {
        node_address: u8,
    },
    Complete,
}

/// Sequential group owner for one half-duplex channel. At most one node owns
/// logical control work, matching the single physical transaction owner.
pub struct SafeStateGroupOperation {
    nodes: Vec<SafeStateWireOperation>,
    current: usize,
}

impl SafeStateGroupOperation {
    pub fn new(
        target: SafeStateTarget,
        safe_state_policy: SafeStatePolicy,
        wire_policy: SafeStateWirePolicy,
        contracts: &[SafeStateNodeContract],
    ) -> Result<Self, SafeStateGroupError> {
        if contracts.is_empty() {
            return Err(SafeStateGroupError::EmptyGroup);
        }
        let mut nodes = Vec::with_capacity(contracts.len());
        for contract in contracts {
            if nodes.iter().any(|operation: &SafeStateWireOperation| {
                operation.node_address() == contract.requirements.node_address
            }) {
                return Err(SafeStateGroupError::DuplicateNodeAddress(
                    contract.requirements.node_address,
                ));
            }
            nodes.push(SafeStateWireOperation::new(
                target,
                safe_state_policy,
                wire_policy,
                contract.requirements,
                contract.boot_epoch,
            )?);
        }
        Ok(Self { nodes, current: 0 })
    }

    pub fn drive(
        &mut self,
        agent: &mut ChannelAgent,
        now: MonotonicMicros,
    ) -> Result<SafeStateGroupProgress, SafeStateGroupError> {
        let Some(operation) = self.nodes.get_mut(self.current) else {
            return Ok(SafeStateGroupProgress::Complete);
        };
        let node_address = operation.node_address();
        Ok(match operation.drive(agent, now)? {
            SafeStateWireProgress::Reached(target) => {
                self.current += 1;
                SafeStateGroupProgress::NodeReached {
                    node_address,
                    target,
                }
            }
            SafeStateWireProgress::Offline => SafeStateGroupProgress::NodeOffline { node_address },
            progress => SafeStateGroupProgress::Node {
                node_address,
                progress,
            },
        })
    }

    pub fn handle_event(
        &mut self,
        event: &ChannelEvent,
        now: MonotonicMicros,
    ) -> Result<SafeStateEventDisposition, SafeStateGroupError> {
        let Some(operation) = self.nodes.get_mut(self.current) else {
            return Ok(SafeStateEventDisposition::Ignored);
        };
        operation.handle_event(event, now).map_err(Into::into)
    }

    pub fn contracts(&self) -> Vec<SafeStateNodeContract> {
        self.nodes
            .iter()
            .map(|operation| SafeStateNodeContract {
                requirements: operation.requirements(),
                boot_epoch: operation.current_boot_epoch(),
            })
            .collect()
    }
}

#[derive(Debug)]
pub enum SafeStateGroupError {
    EmptyGroup,
    DuplicateNodeAddress(u8),
    Wire(SafeStateWireError),
}

impl From<SafeStateWireError> for SafeStateGroupError {
    fn from(value: SafeStateWireError) -> Self {
        Self::Wire(value)
    }
}

impl fmt::Display for SafeStateGroupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "safe-state group error: {self:?}")
    }
}

impl std::error::Error for SafeStateGroupError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_agent::ChannelPolicy;
    use crate::safe_state::{SafeStateAction, SafeStateWireProgress};
    use crate::transaction::DurationMicros;
    use sensor_core::HardwareId;

    fn duration(value: u64) -> DurationMicros {
        DurationMicros::new(value).unwrap()
    }

    fn contract(address: u8, epoch: u64) -> SafeStateNodeContract {
        SafeStateNodeContract {
            requirements: HandshakeRequirements::recording_node(
                HardwareId::from_bytes([address; 8]),
                address,
                2,
                0,
            ),
            boot_epoch: BootEpoch::new(epoch),
        }
    }

    #[test]
    fn rejects_empty_and_duplicate_channel_membership() {
        let safe = SafeStatePolicy {
            orderly_attempts: 2,
            restart_attempts: 2,
        };
        let wire = SafeStateWirePolicy {
            action_deadline: duration(10),
            restart_settle_delay: duration(5),
            restart_confirmation_deadline: duration(100),
        };
        assert!(matches!(
            SafeStateGroupOperation::new(SafeStateTarget::Idle, safe, wire, &[]),
            Err(SafeStateGroupError::EmptyGroup)
        ));
        assert!(matches!(
            SafeStateGroupOperation::new(
                SafeStateTarget::Idle,
                safe,
                wire,
                &[contract(7, 9), contract(7, 10)],
            ),
            Err(SafeStateGroupError::DuplicateNodeAddress(7))
        ));
    }

    #[test]
    fn starts_only_the_first_node_on_the_shared_agent() {
        let mut agent = ChannelAgent::new(
            1,
            1,
            duration(5),
            ChannelPolicy {
                transaction_timeout: duration(10),
                retry_delay: duration(1),
                measurement_attempts_per_slot: 1,
                max_pending_work_items: 4,
            },
        )
        .unwrap();
        let mut group = SafeStateGroupOperation::new(
            SafeStateTarget::Idle,
            SafeStatePolicy {
                orderly_attempts: 2,
                restart_attempts: 2,
            },
            SafeStateWirePolicy {
                action_deadline: duration(20),
                restart_settle_delay: duration(5),
                restart_confirmation_deadline: duration(100),
            },
            &[contract(7, 9), contract(8, 10)],
        )
        .unwrap();
        assert!(matches!(
            group.drive(&mut agent, MonotonicMicros::new(0)).unwrap(),
            SafeStateGroupProgress::Node {
                node_address: 7,
                progress: SafeStateWireProgress::Submitted {
                    action: SafeStateAction::GetState,
                    ..
                }
            }
        ));
        assert_eq!(agent.queued_work_items(), 1);
    }
}
