use core::fmt;

use sensor_core::BootEpoch;
use sensor_protocol::{
    NodeIdentity, ObservabilityProfile, RuntimeOperation, RuntimeResultCode, RuntimeState,
};

use crate::channel_agent::{ChannelAgent, ChannelAgentError, ChannelEvent, ControlWorkKey};
use crate::handshake::HandshakeRequirements;
use crate::transaction::{DurationMicros, MonotonicMicros};

/// Stable quiescent state requested by the channel owner. Maintenance is the
/// normal service target; Idle is the convergence target after an aborted or
/// failed service transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeStateTarget {
    Idle,
    Maintenance,
    Recording,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SafeStatePolicy {
    /// Failures allowed for ordinary GetState/Stop/Enter/Exit commands before
    /// escalating to the independent RS485 restart primitive.
    pub orderly_attempts: u8,
    /// Failed restart transmissions/handshakes before declaring the node
    /// offline. A value greater than one tolerates one lost response.
    pub restart_attempts: u8,
    /// Successful restart recoveries allowed for one convergence request.
    /// This bounds a node which remains reachable but repeatedly refuses the
    /// requested stable state.
    pub restart_cycles: u8,
}

impl SafeStatePolicy {
    pub fn validate(self) -> Result<Self, SafeStateError> {
        if self.orderly_attempts == 0 {
            Err(SafeStateError::ZeroOrderlyAttempts)
        } else if self.restart_attempts == 0 {
            Err(SafeStateError::ZeroRestartAttempts)
        } else if self.restart_cycles == 0 {
            Err(SafeStateError::ZeroRestartCycles)
        } else {
            Ok(self)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeStateAction {
    GetState,
    Start,
    Stop,
    EnterMaintenance,
    ExitMaintenance,
    Restart,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeStateStatus {
    Converging,
    Reached(SafeStateTarget),
    ConvergenceFailed,
    Offline,
}

/// Deterministic per-node convergence policy. It owns no clock or wire. The
/// channel worker supplies bounded deadlines and reports each observed result.
/// Consequently a process restart can reconstruct this object from GetState;
/// no correctness depends on an in-memory transitional label.
pub struct SafeStateConverger {
    target: SafeStateTarget,
    policy: SafeStatePolicy,
    observed: Option<RuntimeState>,
    pending: Option<SafeStateAction>,
    orderly_failures: u8,
    restart_failures: u8,
    completed_restart_cycles: u8,
    status: SafeStateStatus,
}

impl SafeStateConverger {
    pub fn new(target: SafeStateTarget, policy: SafeStatePolicy) -> Result<Self, SafeStateError> {
        Ok(Self {
            target,
            policy: policy.validate()?,
            observed: None,
            pending: None,
            orderly_failures: 0,
            restart_failures: 0,
            completed_restart_cycles: 0,
            status: SafeStateStatus::Converging,
        })
    }

    pub const fn target(&self) -> SafeStateTarget {
        self.target
    }

    pub const fn status(&self) -> SafeStateStatus {
        self.status
    }

    pub const fn observed(&self) -> Option<RuntimeState> {
        self.observed
    }

    pub const fn pending(&self) -> Option<SafeStateAction> {
        self.pending
    }

    pub fn next_action(&mut self) -> Result<Option<SafeStateAction>, SafeStateError> {
        if self.pending.is_some() {
            return Err(SafeStateError::ActionAlreadyPending);
        }
        if self.status != SafeStateStatus::Converging {
            return Ok(None);
        }

        let action = if self.orderly_failures >= self.policy.orderly_attempts {
            if self.completed_restart_cycles >= self.policy.restart_cycles {
                self.status = SafeStateStatus::ConvergenceFailed;
                return Ok(None);
            }
            SafeStateAction::Restart
        } else {
            match self.observed {
                None => SafeStateAction::GetState,
                Some(RuntimeState::Idle) if self.target == SafeStateTarget::Idle => {
                    self.status = SafeStateStatus::Reached(SafeStateTarget::Idle);
                    return Ok(None);
                }
                Some(RuntimeState::Maintenance) if self.target == SafeStateTarget::Maintenance => {
                    self.status = SafeStateStatus::Reached(SafeStateTarget::Maintenance);
                    return Ok(None);
                }
                Some(RuntimeState::Recording) if self.target == SafeStateTarget::Recording => {
                    self.status = SafeStateStatus::Reached(SafeStateTarget::Recording);
                    return Ok(None);
                }
                Some(RuntimeState::Idle) if self.target == SafeStateTarget::Recording => {
                    SafeStateAction::Start
                }
                Some(RuntimeState::Idle) => SafeStateAction::EnterMaintenance,
                Some(RuntimeState::Maintenance) => SafeStateAction::ExitMaintenance,
                Some(RuntimeState::Recording | RuntimeState::Stopping) => SafeStateAction::Stop,
                Some(RuntimeState::Faulted) => SafeStateAction::Restart,
            }
        };
        self.pending = Some(action);
        Ok(Some(action))
    }

    /// Accepts only the result for the currently pending command. Runtime state
    /// is always re-evaluated instead of assuming that the requested transition
    /// happened. A successful restart clears observations and forces a new
    /// handshake/GetState in the new boot epoch.
    pub fn complete(
        &mut self,
        action: SafeStateAction,
        observed: RuntimeState,
    ) -> Result<(), SafeStateError> {
        self.take_pending(action)?;
        if action == SafeStateAction::Restart {
            self.observed = None;
            self.orderly_failures = 0;
            self.restart_failures = 0;
            self.completed_restart_cycles = self.completed_restart_cycles.saturating_add(1);
        } else {
            self.observed = Some(observed);
            self.orderly_failures = 0;
        }
        Ok(())
    }

    /// A timeout, rejected command, lost response or failed post-restart
    /// handshake is one bounded failure. Ordinary failures escalate to restart;
    /// exhausted restart recovery becomes stable Offline, never a transition.
    pub fn fail(&mut self, action: SafeStateAction) -> Result<(), SafeStateError> {
        self.take_pending(action)?;
        if action == SafeStateAction::Restart {
            self.restart_failures = self.restart_failures.saturating_add(1);
            if self.restart_failures >= self.policy.restart_attempts {
                self.status = SafeStateStatus::Offline;
            }
        } else {
            self.orderly_failures = self.orderly_failures.saturating_add(1);
        }
        Ok(())
    }

    /// Aborting a Maintenance request converges all reachable nodes to Idle.
    /// Existing observations are preserved, so a node already in Maintenance
    /// receives ExitMaintenance while Recording/Stopping receives Stop.
    pub fn fall_back_to_idle(&mut self) -> Result<(), SafeStateError> {
        if self.pending.is_some() {
            return Err(SafeStateError::ActionAlreadyPending);
        }
        if self.status == SafeStateStatus::Offline {
            return Ok(());
        }
        self.target = SafeStateTarget::Idle;
        self.status = SafeStateStatus::Converging;
        self.orderly_failures = 0;
        Ok(())
    }

    fn take_pending(&mut self, action: SafeStateAction) -> Result<(), SafeStateError> {
        match self.pending.take() {
            Some(expected) if expected == action => Ok(()),
            Some(expected) => {
                self.pending = Some(expected);
                Err(SafeStateError::WrongCompletion {
                    expected,
                    received: action,
                })
            }
            None => Err(SafeStateError::NoPendingAction),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeStateError {
    ZeroOrderlyAttempts,
    ZeroRestartAttempts,
    ZeroRestartCycles,
    ActionAlreadyPending,
    NoPendingAction,
    WrongCompletion {
        expected: SafeStateAction,
        received: SafeStateAction,
    },
}

impl fmt::Display for SafeStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "safe-state convergence error: {self:?}")
    }
}

impl std::error::Error for SafeStateError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SafeStateWirePolicy {
    pub action_deadline: DurationMicros,
    pub restart_settle_delay: DurationMicros,
    pub restart_confirmation_deadline: DurationMicros,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WirePhase {
    Ready,
    Lifecycle {
        action: SafeStateAction,
        operation_id: u32,
        deadline: MonotonicMicros,
    },
    RestartDelay {
        previous_boot_epoch: BootEpoch,
        probe_not_before: MonotonicMicros,
        deadline: MonotonicMicros,
    },
    RestartHandshake {
        previous_boot_epoch: BootEpoch,
        deadline: MonotonicMicros,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeStateWireProgress {
    Waiting,
    Submitted {
        action: SafeStateAction,
        operation_id: u32,
    },
    RestartHandshakeSubmitted,
    RestartConfirmationStarted,
    AwaitingWireSettlement {
        action: SafeStateAction,
    },
    AttemptFailed {
        action: SafeStateAction,
    },
    Reached(SafeStateTarget),
    Offline,
    ConvergenceFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeStateEventDisposition {
    Ignored,
    Progressed,
    AttemptFailed(SafeStateAction),
    RestartObserved { identity: NodeIdentity },
}

/// Incremental wire adapter for one `SafeStateConverger`. It never blocks and
/// never owns the port. The channel worker calls `drive`, performs one normal
/// `ChannelEndpoint::step`, and passes the resulting `ChannelEvent` back to
/// `handle_event`. Deadlines can abandon logical control work only after the
/// transaction core has made the wire idle and expired its late-response
/// quarantine.
pub struct SafeStateWireOperation {
    node_address: u8,
    requirements: HandshakeRequirements,
    current_boot_epoch: BootEpoch,
    policy: SafeStateWirePolicy,
    converger: SafeStateConverger,
    phase: WirePhase,
}

impl SafeStateWireOperation {
    pub fn new(
        target: SafeStateTarget,
        safe_state_policy: SafeStatePolicy,
        wire_policy: SafeStateWirePolicy,
        requirements: HandshakeRequirements,
        current_boot_epoch: BootEpoch,
    ) -> Result<Self, SafeStateWireError> {
        if current_boot_epoch.get() == 0 {
            return Err(SafeStateWireError::ZeroBootEpoch);
        }
        Ok(Self {
            node_address: requirements.node_address,
            requirements,
            current_boot_epoch,
            policy: wire_policy,
            converger: SafeStateConverger::new(target, safe_state_policy)
                .map_err(SafeStateWireError::Convergence)?,
            phase: WirePhase::Ready,
        })
    }

    pub const fn status(&self) -> SafeStateStatus {
        self.converger.status()
    }

    pub const fn current_boot_epoch(&self) -> BootEpoch {
        self.current_boot_epoch
    }

    pub const fn node_address(&self) -> u8 {
        self.node_address
    }

    pub const fn requirements(&self) -> HandshakeRequirements {
        self.requirements
    }

    pub fn fall_back_to_idle(&mut self) -> Result<(), SafeStateWireError> {
        if self.phase != WirePhase::Ready {
            return Err(SafeStateWireError::WireActionPending);
        }
        self.converger
            .fall_back_to_idle()
            .map_err(SafeStateWireError::Convergence)
    }

    pub fn drive(
        &mut self,
        agent: &mut ChannelAgent,
        now: MonotonicMicros,
    ) -> Result<SafeStateWireProgress, SafeStateWireError> {
        match self.phase {
            WirePhase::Ready => self.submit_next_action(agent, now),
            WirePhase::Lifecycle {
                action,
                operation_id,
                deadline,
            } if now >= deadline => {
                let key = ControlWorkKey::Lifecycle {
                    node_address: self.node_address,
                    operation_id,
                };
                match agent.abandon_control_work(key) {
                    Ok(_) => {
                        if action == SafeStateAction::Restart {
                            self.begin_restart_confirmation(now)?;
                            return Ok(SafeStateWireProgress::RestartConfirmationStarted);
                        }
                        self.converger
                            .fail(action)
                            .map_err(SafeStateWireError::Convergence)?;
                        self.phase = WirePhase::Ready;
                        Ok(SafeStateWireProgress::AttemptFailed { action })
                    }
                    Err(ChannelAgentError::ControlAttemptInFlight(_)) => {
                        Ok(SafeStateWireProgress::AwaitingWireSettlement { action })
                    }
                    Err(error) => Err(error.into()),
                }
            }
            WirePhase::Lifecycle { .. } => Ok(SafeStateWireProgress::Waiting),
            WirePhase::RestartDelay {
                previous_boot_epoch: _,
                probe_not_before: _,
                deadline,
            }
            | WirePhase::RestartHandshake {
                previous_boot_epoch: _,
                deadline,
            } if now >= deadline => self.fail_restart_confirmation(agent),
            WirePhase::RestartDelay {
                previous_boot_epoch,
                probe_not_before,
                deadline,
            } if now >= probe_not_before => {
                if agent.queued_work_items() != 0 {
                    return Err(SafeStateWireError::ChannelNotExclusive);
                }
                agent.submit_handshake(self.requirements)?;
                self.phase = WirePhase::RestartHandshake {
                    previous_boot_epoch,
                    deadline,
                };
                Ok(SafeStateWireProgress::RestartHandshakeSubmitted)
            }
            WirePhase::RestartDelay { .. } | WirePhase::RestartHandshake { .. } => {
                Ok(SafeStateWireProgress::Waiting)
            }
        }
    }

    pub fn handle_event(
        &mut self,
        event: &ChannelEvent,
        now: MonotonicMicros,
    ) -> Result<SafeStateEventDisposition, SafeStateWireError> {
        match (self.phase, event) {
            (
                WirePhase::Lifecycle {
                    action,
                    operation_id,
                    ..
                },
                ChannelEvent::LifecycleComplete {
                    node_address,
                    result,
                },
            ) if *node_address == self.node_address && result.operation_id == operation_id => {
                if result.code != RuntimeResultCode::Ok
                    || !runtime_result_matches_action(action, result.state)
                {
                    self.converger
                        .fail(action)
                        .map_err(SafeStateWireError::Convergence)?;
                    self.phase = WirePhase::Ready;
                    return Ok(SafeStateEventDisposition::AttemptFailed(action));
                }
                if action == SafeStateAction::Restart {
                    self.begin_restart_confirmation(now)?;
                } else {
                    self.converger
                        .complete(action, result.state)
                        .map_err(SafeStateWireError::Convergence)?;
                    self.phase = WirePhase::Ready;
                }
                Ok(SafeStateEventDisposition::Progressed)
            }
            (
                WirePhase::RestartHandshake {
                    previous_boot_epoch,
                    deadline,
                },
                ChannelEvent::HandshakeComplete {
                    node_address,
                    identity,
                    ..
                },
            ) if *node_address == self.node_address => {
                if identity.boot_epoch == previous_boot_epoch {
                    self.phase = WirePhase::RestartDelay {
                        previous_boot_epoch,
                        probe_not_before: checked_deadline(now, self.policy.restart_settle_delay)?,
                        deadline,
                    };
                    return Ok(SafeStateEventDisposition::Progressed);
                }
                self.current_boot_epoch = identity.boot_epoch;
                self.converger
                    .complete(SafeStateAction::Restart, RuntimeState::Idle)
                    .map_err(SafeStateWireError::Convergence)?;
                self.phase = WirePhase::Ready;
                Ok(SafeStateEventDisposition::RestartObserved {
                    identity: *identity,
                })
            }
            _ => Ok(SafeStateEventDisposition::Ignored),
        }
    }

    fn submit_next_action(
        &mut self,
        agent: &mut ChannelAgent,
        now: MonotonicMicros,
    ) -> Result<SafeStateWireProgress, SafeStateWireError> {
        match self.converger.status() {
            SafeStateStatus::Reached(target) => {
                return Ok(SafeStateWireProgress::Reached(target));
            }
            SafeStateStatus::Offline => return Ok(SafeStateWireProgress::Offline),
            SafeStateStatus::ConvergenceFailed => {
                return Ok(SafeStateWireProgress::ConvergenceFailed);
            }
            SafeStateStatus::Converging => {}
        }
        if agent.queued_work_items() != 0 {
            return Err(SafeStateWireError::ChannelNotExclusive);
        }
        let Some(action) = self
            .converger
            .next_action()
            .map_err(SafeStateWireError::Convergence)?
        else {
            return match self.converger.status() {
                SafeStateStatus::Reached(target) => Ok(SafeStateWireProgress::Reached(target)),
                SafeStateStatus::Offline => Ok(SafeStateWireProgress::Offline),
                SafeStateStatus::ConvergenceFailed => Ok(SafeStateWireProgress::ConvergenceFailed),
                SafeStateStatus::Converging => Err(SafeStateWireError::InvariantViolation),
            };
        };
        let operation_id = agent.submit_lifecycle(
            self.node_address,
            runtime_operation(action),
            ObservabilityProfile::Production,
        )?;
        self.phase = WirePhase::Lifecycle {
            action,
            operation_id,
            deadline: checked_deadline(now, self.policy.action_deadline)?,
        };
        Ok(SafeStateWireProgress::Submitted {
            action,
            operation_id,
        })
    }

    fn fail_restart_confirmation(
        &mut self,
        agent: &mut ChannelAgent,
    ) -> Result<SafeStateWireProgress, SafeStateWireError> {
        if matches!(self.phase, WirePhase::RestartHandshake { .. }) {
            let key = ControlWorkKey::Handshake {
                node_address: self.node_address,
            };
            match agent.abandon_control_work(key) {
                Ok(_) => {}
                Err(ChannelAgentError::ControlAttemptInFlight(_)) => {
                    return Ok(SafeStateWireProgress::AwaitingWireSettlement {
                        action: SafeStateAction::Restart,
                    });
                }
                Err(error) => return Err(error.into()),
            }
        }
        self.converger
            .fail(SafeStateAction::Restart)
            .map_err(SafeStateWireError::Convergence)?;
        self.phase = WirePhase::Ready;
        Ok(SafeStateWireProgress::AttemptFailed {
            action: SafeStateAction::Restart,
        })
    }

    fn begin_restart_confirmation(
        &mut self,
        now: MonotonicMicros,
    ) -> Result<(), SafeStateWireError> {
        self.phase = WirePhase::RestartDelay {
            previous_boot_epoch: self.current_boot_epoch,
            probe_not_before: checked_deadline(now, self.policy.restart_settle_delay)?,
            deadline: checked_deadline(now, self.policy.restart_confirmation_deadline)?,
        };
        Ok(())
    }
}

fn runtime_operation(action: SafeStateAction) -> RuntimeOperation {
    match action {
        SafeStateAction::GetState => RuntimeOperation::GetState,
        SafeStateAction::Start => RuntimeOperation::Start,
        SafeStateAction::Stop => RuntimeOperation::Stop,
        SafeStateAction::EnterMaintenance => RuntimeOperation::EnterMaintenance,
        SafeStateAction::ExitMaintenance => RuntimeOperation::ExitMaintenance,
        SafeStateAction::Restart => RuntimeOperation::RestartNode,
    }
}

fn runtime_result_matches_action(action: SafeStateAction, state: RuntimeState) -> bool {
    match action {
        SafeStateAction::GetState | SafeStateAction::Restart => true,
        SafeStateAction::Start => state == RuntimeState::Recording,
        SafeStateAction::Stop | SafeStateAction::ExitMaintenance => state == RuntimeState::Idle,
        SafeStateAction::EnterMaintenance => state == RuntimeState::Maintenance,
    }
}

fn checked_deadline(
    now: MonotonicMicros,
    duration: DurationMicros,
) -> Result<MonotonicMicros, SafeStateWireError> {
    now.checked_add(duration)
        .ok_or(SafeStateWireError::TimeOverflow)
}

#[derive(Debug)]
pub enum SafeStateWireError {
    ZeroBootEpoch,
    WireActionPending,
    ChannelNotExclusive,
    TimeOverflow,
    InvariantViolation,
    Convergence(SafeStateError),
    Agent(ChannelAgentError),
}

impl From<ChannelAgentError> for SafeStateWireError {
    fn from(value: ChannelAgentError) -> Self {
        Self::Agent(value)
    }
}

impl fmt::Display for SafeStateWireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "safe-state wire error: {self:?}")
    }
}

impl std::error::Error for SafeStateWireError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_agent::{ChannelEvent, ChannelPolicy};
    use sensor_core::HardwareId;
    use sensor_protocol::{
        encode_lifecycle_response, Frame, FrameRole, RuntimeResult, SensorStatusCode, HOST_ADDRESS,
    };

    const HARDWARE_ID: HardwareId = HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);

    fn policy() -> SafeStatePolicy {
        SafeStatePolicy {
            orderly_attempts: 2,
            restart_attempts: 2,
            restart_cycles: 1,
        }
    }

    fn duration(value: u64) -> DurationMicros {
        DurationMicros::new(value).unwrap()
    }

    fn wire_policy() -> SafeStateWirePolicy {
        SafeStateWirePolicy {
            action_deadline: duration(10),
            restart_settle_delay: duration(5),
            restart_confirmation_deadline: duration(100),
        }
    }

    fn requirements() -> HandshakeRequirements {
        requirements_for(7)
    }

    fn requirements_for(node_address: u8) -> HandshakeRequirements {
        HandshakeRequirements::recording_node(HARDWARE_ID, node_address, 2, 0)
    }

    fn agent() -> ChannelAgent {
        ChannelAgent::new(
            10,
            100,
            duration(5),
            ChannelPolicy {
                transaction_timeout: duration(10),
                retry_delay: duration(1),
                measurement_attempts_per_slot: 1,
                max_pending_work_items: 4,
            },
        )
        .unwrap()
    }

    fn complete_lifecycle(
        agent: &mut ChannelAgent,
        transmitted_at: u64,
        state: RuntimeState,
    ) -> ChannelEvent {
        assert_eq!(
            agent.poll(MonotonicMicros::new(transmitted_at)).unwrap(),
            ChannelEvent::TransmitReady
        );
        let request = agent.take_outbound().unwrap();
        agent
            .transmit_completed(MonotonicMicros::new(transmitted_at))
            .unwrap();
        let operation_id = u32::from_le_bytes(request.payload()[..4].try_into().unwrap());
        let payload = encode_lifecycle_response(RuntimeResult {
            operation_id,
            code: RuntimeResultCode::Ok,
            state,
            observability: ObservabilityProfile::Production,
            sensor_status: SensorStatusCode::Ok,
        })
        .unwrap();
        let response = Frame::new(
            FrameRole::Response,
            HOST_ADDRESS,
            request.destination,
            request.service,
            request.message,
            request.transaction_id,
            &payload,
        )
        .unwrap();
        agent
            .receive(MonotonicMicros::new(transmitted_at + 1), &response)
            .unwrap()
    }

    #[test]
    fn recording_reaches_maintenance_only_through_confirmed_idle() {
        let mut state = SafeStateConverger::new(SafeStateTarget::Maintenance, policy()).unwrap();
        assert_eq!(
            state.next_action().unwrap(),
            Some(SafeStateAction::GetState)
        );
        state
            .complete(SafeStateAction::GetState, RuntimeState::Recording)
            .unwrap();
        assert_eq!(state.next_action().unwrap(), Some(SafeStateAction::Stop));
        state
            .complete(SafeStateAction::Stop, RuntimeState::Idle)
            .unwrap();
        assert_eq!(
            state.next_action().unwrap(),
            Some(SafeStateAction::EnterMaintenance)
        );
        state
            .complete(SafeStateAction::EnterMaintenance, RuntimeState::Maintenance)
            .unwrap();
        assert_eq!(state.next_action().unwrap(), None);
        assert_eq!(
            state.status(),
            SafeStateStatus::Reached(SafeStateTarget::Maintenance)
        );
    }

    #[test]
    fn stuck_stopping_escalates_to_restart_and_reobserves_new_boot() {
        let mut state = SafeStateConverger::new(SafeStateTarget::Maintenance, policy()).unwrap();
        state.next_action().unwrap();
        state
            .complete(SafeStateAction::GetState, RuntimeState::Stopping)
            .unwrap();
        for _ in 0..2 {
            assert_eq!(state.next_action().unwrap(), Some(SafeStateAction::Stop));
            state.fail(SafeStateAction::Stop).unwrap();
        }
        assert_eq!(state.next_action().unwrap(), Some(SafeStateAction::Restart));
        state
            .complete(SafeStateAction::Restart, RuntimeState::Stopping)
            .unwrap();
        assert_eq!(state.observed(), None);
        assert_eq!(
            state.next_action().unwrap(),
            Some(SafeStateAction::GetState)
        );
    }

    #[test]
    fn aborted_mixed_transition_converges_maintenance_node_back_to_idle() {
        let mut state = SafeStateConverger::new(SafeStateTarget::Maintenance, policy()).unwrap();
        state.next_action().unwrap();
        state
            .complete(SafeStateAction::GetState, RuntimeState::Maintenance)
            .unwrap();
        state.fall_back_to_idle().unwrap();
        assert_eq!(
            state.next_action().unwrap(),
            Some(SafeStateAction::ExitMaintenance)
        );
        state
            .complete(SafeStateAction::ExitMaintenance, RuntimeState::Idle)
            .unwrap();
        assert_eq!(state.next_action().unwrap(), None);
        assert_eq!(
            state.status(),
            SafeStateStatus::Reached(SafeStateTarget::Idle)
        );
    }

    #[test]
    fn exhausted_restart_has_stable_offline_result() {
        let mut state = SafeStateConverger::new(SafeStateTarget::Idle, policy()).unwrap();
        state.next_action().unwrap();
        state
            .complete(SafeStateAction::GetState, RuntimeState::Faulted)
            .unwrap();
        for _ in 0..2 {
            assert_eq!(state.next_action().unwrap(), Some(SafeStateAction::Restart));
            state.fail(SafeStateAction::Restart).unwrap();
        }
        assert_eq!(state.status(), SafeStateStatus::Offline);
        assert_eq!(state.next_action().unwrap(), None);
    }

    #[test]
    fn reachable_node_cannot_loop_forever_when_it_refuses_maintenance() {
        let mut state = SafeStateConverger::new(
            SafeStateTarget::Maintenance,
            SafeStatePolicy {
                orderly_attempts: 1,
                restart_attempts: 2,
                restart_cycles: 1,
            },
        )
        .unwrap();

        assert_eq!(
            state.next_action().unwrap(),
            Some(SafeStateAction::GetState)
        );
        state
            .complete(SafeStateAction::GetState, RuntimeState::Idle)
            .unwrap();
        assert_eq!(
            state.next_action().unwrap(),
            Some(SafeStateAction::EnterMaintenance)
        );
        state.fail(SafeStateAction::EnterMaintenance).unwrap();
        assert_eq!(state.next_action().unwrap(), Some(SafeStateAction::Restart));
        state
            .complete(SafeStateAction::Restart, RuntimeState::Idle)
            .unwrap();
        assert_eq!(
            state.next_action().unwrap(),
            Some(SafeStateAction::GetState)
        );
        state
            .complete(SafeStateAction::GetState, RuntimeState::Idle)
            .unwrap();
        assert_eq!(
            state.next_action().unwrap(),
            Some(SafeStateAction::EnterMaintenance)
        );
        state.fail(SafeStateAction::EnterMaintenance).unwrap();

        assert_eq!(state.next_action().unwrap(), None);
        assert_eq!(state.status(), SafeStateStatus::ConvergenceFailed);
        state.fall_back_to_idle().unwrap();
        assert_eq!(state.next_action().unwrap(), None);
        assert_eq!(
            state.status(),
            SafeStateStatus::Reached(SafeStateTarget::Idle)
        );
    }

    #[test]
    fn wrong_completion_cannot_advance_state() {
        let mut state = SafeStateConverger::new(SafeStateTarget::Idle, policy()).unwrap();
        state.next_action().unwrap();
        assert_eq!(
            state.complete(SafeStateAction::Stop, RuntimeState::Idle),
            Err(SafeStateError::WrongCompletion {
                expected: SafeStateAction::GetState,
                received: SafeStateAction::Stop,
            })
        );
        assert_eq!(state.pending(), Some(SafeStateAction::GetState));
    }

    #[test]
    fn wire_operation_reaches_maintenance_without_blocking_or_assuming_state() {
        let mut agent = agent();
        let mut operation = SafeStateWireOperation::new(
            SafeStateTarget::Maintenance,
            policy(),
            wire_policy(),
            requirements(),
            BootEpoch::new(9),
        )
        .unwrap();

        assert!(matches!(
            operation
                .drive(&mut agent, MonotonicMicros::new(0))
                .unwrap(),
            SafeStateWireProgress::Submitted {
                action: SafeStateAction::GetState,
                ..
            }
        ));
        let event = complete_lifecycle(&mut agent, 0, RuntimeState::Recording);
        assert_eq!(
            operation
                .handle_event(&event, MonotonicMicros::new(1))
                .unwrap(),
            SafeStateEventDisposition::Progressed
        );

        assert!(matches!(
            operation
                .drive(&mut agent, MonotonicMicros::new(2))
                .unwrap(),
            SafeStateWireProgress::Submitted {
                action: SafeStateAction::Stop,
                ..
            }
        ));
        let event = complete_lifecycle(&mut agent, 2, RuntimeState::Idle);
        operation
            .handle_event(&event, MonotonicMicros::new(3))
            .unwrap();

        assert!(matches!(
            operation
                .drive(&mut agent, MonotonicMicros::new(4))
                .unwrap(),
            SafeStateWireProgress::Submitted {
                action: SafeStateAction::EnterMaintenance,
                ..
            }
        ));
        let event = complete_lifecycle(&mut agent, 4, RuntimeState::Maintenance);
        operation
            .handle_event(&event, MonotonicMicros::new(5))
            .unwrap();
        assert_eq!(
            operation
                .drive(&mut agent, MonotonicMicros::new(6))
                .unwrap(),
            SafeStateWireProgress::Reached(SafeStateTarget::Maintenance)
        );
    }

    #[test]
    fn wire_deadline_waits_for_quarantine_then_escalates_to_restart() {
        let mut agent = agent();
        let mut operation = SafeStateWireOperation::new(
            SafeStateTarget::Idle,
            SafeStatePolicy {
                orderly_attempts: 1,
                restart_attempts: 2,
                restart_cycles: 1,
            },
            wire_policy(),
            requirements(),
            BootEpoch::new(9),
        )
        .unwrap();

        operation
            .drive(&mut agent, MonotonicMicros::new(0))
            .unwrap();
        assert_eq!(
            agent.poll(MonotonicMicros::new(0)).unwrap(),
            ChannelEvent::TransmitReady
        );
        agent.take_outbound().unwrap();
        agent.transmit_completed(MonotonicMicros::new(0)).unwrap();

        assert_eq!(
            operation
                .drive(&mut agent, MonotonicMicros::new(11))
                .unwrap(),
            SafeStateWireProgress::AwaitingWireSettlement {
                action: SafeStateAction::GetState,
            }
        );
        assert!(matches!(
            agent.poll(MonotonicMicros::new(11)).unwrap(),
            ChannelEvent::LifecycleTimedOut { .. }
        ));
        assert_eq!(
            agent.poll(MonotonicMicros::new(15)).unwrap(),
            ChannelEvent::QuarantineEnded
        );
        assert_eq!(
            operation
                .drive(&mut agent, MonotonicMicros::new(15))
                .unwrap(),
            SafeStateWireProgress::AttemptFailed {
                action: SafeStateAction::GetState,
            }
        );
        assert!(matches!(
            operation
                .drive(&mut agent, MonotonicMicros::new(16))
                .unwrap(),
            SafeStateWireProgress::Submitted {
                action: SafeStateAction::Restart,
                ..
            }
        ));
    }
}
