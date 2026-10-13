use core::fmt;

use sensor_protocol::RuntimeState;

/// Stable quiescent state requested by the channel owner. Maintenance is the
/// normal service target; Idle is the convergence target after an aborted or
/// failed service transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeStateTarget {
    Idle,
    Maintenance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SafeStatePolicy {
    /// Failures allowed for ordinary GetState/Stop/Enter/Exit commands before
    /// escalating to the independent RS485 restart primitive.
    pub orderly_attempts: u8,
    /// Failed restart transmissions/handshakes before declaring the node
    /// offline. A value greater than one tolerates one lost response.
    pub restart_attempts: u8,
}

impl SafeStatePolicy {
    pub fn validate(self) -> Result<Self, SafeStateError> {
        if self.orderly_attempts == 0 {
            Err(SafeStateError::ZeroOrderlyAttempts)
        } else if self.restart_attempts == 0 {
            Err(SafeStateError::ZeroRestartAttempts)
        } else {
            Ok(self)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeStateAction {
    GetState,
    Stop,
    EnterMaintenance,
    ExitMaintenance,
    Restart,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeStateStatus {
    Converging,
    Reached(SafeStateTarget),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> SafeStatePolicy {
        SafeStatePolicy {
            orderly_attempts: 2,
            restart_attempts: 2,
        }
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
}
