use core::fmt;

use crate::safe_state::SafeStateTarget;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaintenanceEntryStage {
    Quiescing,
    ConvergingToIdle,
    ReconcilingStoppedStreams,
    DrainingStoppedStreams,
    ConvergingToMaintenance,
    FallingBackToIdle,
    CompleteMaintenance,
    CompleteIdle,
    Offline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaintenanceEntryDirective {
    Quiesce,
    Converge(SafeStateTarget),
    ReconcileStoppedStreams,
    DrainStoppedStreams,
    CompleteMaintenance,
    CompleteIdle,
    Offline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaintenanceEntryEvent {
    Quiesced,
    SafeStateReached(SafeStateTarget),
    StoppedStreamsReconciled,
    StoppedStreamsDrained,
    MaintenanceConvergenceFailed,
    NodeOffline,
}

/// Pure ordering policy for a channel-wide service transition. Wire, Capture
/// and scheduler adapters report completed facts; this object prevents them
/// from being accepted out of order. A node restart while stopping is handled
/// by the mandatory reconciliation stage before any final journal drain.
pub struct MaintenanceEntryPlan {
    stage: MaintenanceEntryStage,
}

impl MaintenanceEntryPlan {
    pub const fn new() -> Self {
        Self {
            stage: MaintenanceEntryStage::Quiescing,
        }
    }

    pub const fn stage(&self) -> MaintenanceEntryStage {
        self.stage
    }

    pub const fn directive(&self) -> MaintenanceEntryDirective {
        match self.stage {
            MaintenanceEntryStage::Quiescing => MaintenanceEntryDirective::Quiesce,
            MaintenanceEntryStage::ConvergingToIdle | MaintenanceEntryStage::FallingBackToIdle => {
                MaintenanceEntryDirective::Converge(SafeStateTarget::Idle)
            }
            MaintenanceEntryStage::ReconcilingStoppedStreams => {
                MaintenanceEntryDirective::ReconcileStoppedStreams
            }
            MaintenanceEntryStage::DrainingStoppedStreams => {
                MaintenanceEntryDirective::DrainStoppedStreams
            }
            MaintenanceEntryStage::ConvergingToMaintenance => {
                MaintenanceEntryDirective::Converge(SafeStateTarget::Maintenance)
            }
            MaintenanceEntryStage::CompleteMaintenance => {
                MaintenanceEntryDirective::CompleteMaintenance
            }
            MaintenanceEntryStage::CompleteIdle => MaintenanceEntryDirective::CompleteIdle,
            MaintenanceEntryStage::Offline => MaintenanceEntryDirective::Offline,
        }
    }

    pub fn accept(&mut self, event: MaintenanceEntryEvent) -> Result<(), ChannelModeError> {
        self.stage = match (self.stage, event) {
            (MaintenanceEntryStage::Quiescing, MaintenanceEntryEvent::Quiesced) => {
                MaintenanceEntryStage::ConvergingToIdle
            }
            (
                MaintenanceEntryStage::ConvergingToIdle,
                MaintenanceEntryEvent::SafeStateReached(SafeStateTarget::Idle),
            ) => MaintenanceEntryStage::ReconcilingStoppedStreams,
            (
                MaintenanceEntryStage::ReconcilingStoppedStreams,
                MaintenanceEntryEvent::StoppedStreamsReconciled,
            ) => MaintenanceEntryStage::DrainingStoppedStreams,
            (
                MaintenanceEntryStage::DrainingStoppedStreams,
                MaintenanceEntryEvent::StoppedStreamsDrained,
            ) => MaintenanceEntryStage::ConvergingToMaintenance,
            (
                MaintenanceEntryStage::ConvergingToMaintenance,
                MaintenanceEntryEvent::SafeStateReached(SafeStateTarget::Maintenance),
            ) => MaintenanceEntryStage::CompleteMaintenance,
            (
                MaintenanceEntryStage::ConvergingToMaintenance,
                MaintenanceEntryEvent::MaintenanceConvergenceFailed,
            ) => MaintenanceEntryStage::FallingBackToIdle,
            (
                MaintenanceEntryStage::FallingBackToIdle,
                MaintenanceEntryEvent::SafeStateReached(SafeStateTarget::Idle),
            ) => MaintenanceEntryStage::CompleteIdle,
            (_, MaintenanceEntryEvent::NodeOffline) => MaintenanceEntryStage::Offline,
            (stage, event) => {
                return Err(ChannelModeError::UnexpectedEvent { stage, event });
            }
        };
        Ok(())
    }
}

impl Default for MaintenanceEntryPlan {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelModeError {
    UnexpectedEvent {
        stage: MaintenanceEntryStage,
        event: MaintenanceEntryEvent,
    },
}

impl fmt::Display for ChannelModeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "channel mode transition error: {self:?}")
    }
}

impl std::error::Error for ChannelModeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance_requires_reconciliation_between_idle_and_drain() {
        let mut plan = MaintenanceEntryPlan::new();
        plan.accept(MaintenanceEntryEvent::Quiesced).unwrap();
        plan.accept(MaintenanceEntryEvent::SafeStateReached(
            SafeStateTarget::Idle,
        ))
        .unwrap();
        assert_eq!(
            plan.directive(),
            MaintenanceEntryDirective::ReconcileStoppedStreams
        );
        assert!(matches!(
            plan.accept(MaintenanceEntryEvent::StoppedStreamsDrained),
            Err(ChannelModeError::UnexpectedEvent { .. })
        ));
        plan.accept(MaintenanceEntryEvent::StoppedStreamsReconciled)
            .unwrap();
        plan.accept(MaintenanceEntryEvent::StoppedStreamsDrained)
            .unwrap();
        plan.accept(MaintenanceEntryEvent::SafeStateReached(
            SafeStateTarget::Maintenance,
        ))
        .unwrap();
        assert_eq!(plan.stage(), MaintenanceEntryStage::CompleteMaintenance);
    }

    #[test]
    fn failed_maintenance_convergence_has_an_explicit_idle_fallback() {
        let mut plan = MaintenanceEntryPlan::new();
        for event in [
            MaintenanceEntryEvent::Quiesced,
            MaintenanceEntryEvent::SafeStateReached(SafeStateTarget::Idle),
            MaintenanceEntryEvent::StoppedStreamsReconciled,
            MaintenanceEntryEvent::StoppedStreamsDrained,
            MaintenanceEntryEvent::MaintenanceConvergenceFailed,
        ] {
            plan.accept(event).unwrap();
        }
        assert_eq!(
            plan.directive(),
            MaintenanceEntryDirective::Converge(SafeStateTarget::Idle)
        );
        plan.accept(MaintenanceEntryEvent::SafeStateReached(
            SafeStateTarget::Idle,
        ))
        .unwrap();
        assert_eq!(plan.stage(), MaintenanceEntryStage::CompleteIdle);
    }

    #[test]
    fn offline_is_terminal_from_every_nonterminal_stage() {
        let mut plan = MaintenanceEntryPlan::new();
        plan.accept(MaintenanceEntryEvent::NodeOffline).unwrap();
        assert_eq!(plan.directive(), MaintenanceEntryDirective::Offline);
        assert!(plan.accept(MaintenanceEntryEvent::Quiesced).is_err());
    }
}
