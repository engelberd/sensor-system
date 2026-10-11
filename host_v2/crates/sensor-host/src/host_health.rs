use crate::channel_supervisor::{ChannelWorkerPhase, ChannelWorkerSnapshot};
use crate::time_health::TimePersistenceHealthState;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostCoordinatorLifecycle {
    Running,
    StopRequested,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostHealthLevel {
    Healthy,
    Degraded,
    StopRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostHealthReason {
    ChannelFaulted { channel_id: u8 },
    ChannelTerminated { channel_id: u8 },
    TimeRecoveryRequired,
    TimeRecovering,
    TimeIntegrityFault,
    SharedStorageFault(SharedStorageFault),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SharedStorageFault {
    ReserveBreached {
        available_bytes: u64,
        required_bytes: u64,
    },
    ReadOnly,
    ProbeFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostHealthSnapshot {
    pub level: HostHealthLevel,
    pub coordinator: HostCoordinatorLifecycle,
    pub time: TimePersistenceHealthState,
    pub channels: Vec<ChannelWorkerSnapshot>,
    pub reasons: Vec<HostHealthReason>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ObservedChannelHealth {
    pub snapshot: ChannelWorkerSnapshot,
    pub thread_finished: bool,
}

/// Pure projection of runtime facts. It never performs recovery or sends a
/// command, so status queries cannot accidentally change the system.
pub(crate) fn evaluate_host_health(
    coordinator: HostCoordinatorLifecycle,
    time: TimePersistenceHealthState,
    shared_storage_fault: Option<SharedStorageFault>,
    channels: impl IntoIterator<Item = ObservedChannelHealth>,
) -> HostHealthSnapshot {
    let mut reasons = Vec::new();
    let mut snapshots = Vec::new();
    for channel in channels {
        let snapshot = channel.snapshot;
        if snapshot.phase == ChannelWorkerPhase::Faulted {
            reasons.push(HostHealthReason::ChannelFaulted {
                channel_id: snapshot.channel_id,
            });
        } else if coordinator == HostCoordinatorLifecycle::Running && channel.thread_finished {
            reasons.push(HostHealthReason::ChannelTerminated {
                channel_id: snapshot.channel_id,
            });
        }
        snapshots.push(snapshot);
    }

    match time {
        TimePersistenceHealthState::Healthy => {}
        TimePersistenceHealthState::RecoveryRequired => {
            reasons.push(HostHealthReason::TimeRecoveryRequired);
        }
        TimePersistenceHealthState::Recovering => {
            reasons.push(HostHealthReason::TimeRecovering);
        }
        TimePersistenceHealthState::IntegrityFault => {
            reasons.push(HostHealthReason::TimeIntegrityFault);
        }
    }
    if let Some(fault) = shared_storage_fault {
        reasons.push(HostHealthReason::SharedStorageFault(fault));
    }

    let level = if reasons.iter().any(|reason| {
        matches!(
            reason,
            HostHealthReason::TimeIntegrityFault | HostHealthReason::SharedStorageFault(_)
        )
    }) {
        HostHealthLevel::StopRequired
    } else if reasons.is_empty() {
        HostHealthLevel::Healthy
    } else {
        HostHealthLevel::Degraded
    };
    HostHealthSnapshot {
        level,
        coordinator,
        time,
        channels: snapshots,
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(
        channel_id: u8,
        phase: ChannelWorkerPhase,
        thread_finished: bool,
    ) -> ObservedChannelHealth {
        ObservedChannelHealth {
            snapshot: ChannelWorkerSnapshot {
                channel_id,
                phase,
                completed_steps: 7,
            },
            thread_finished,
        }
    }

    #[test]
    fn one_channel_fault_degrades_without_becoming_a_global_stop() {
        let health = evaluate_host_health(
            HostCoordinatorLifecycle::Running,
            TimePersistenceHealthState::Healthy,
            None,
            [
                channel(1, ChannelWorkerPhase::Running, false),
                channel(2, ChannelWorkerPhase::Faulted, true),
            ],
        );
        assert_eq!(health.level, HostHealthLevel::Degraded);
        assert_eq!(
            health.reasons,
            vec![HostHealthReason::ChannelFaulted { channel_id: 2 }]
        );
    }

    #[test]
    fn time_recovery_is_degraded_but_integrity_fault_requires_stop() {
        let recovering = evaluate_host_health(
            HostCoordinatorLifecycle::Running,
            TimePersistenceHealthState::Recovering,
            None,
            [channel(1, ChannelWorkerPhase::Running, false)],
        );
        assert_eq!(recovering.level, HostHealthLevel::Degraded);

        let faulted = evaluate_host_health(
            HostCoordinatorLifecycle::Running,
            TimePersistenceHealthState::IntegrityFault,
            None,
            [channel(1, ChannelWorkerPhase::Running, false)],
        );
        assert_eq!(faulted.level, HostHealthLevel::StopRequired);
        assert!(faulted
            .reasons
            .contains(&HostHealthReason::TimeIntegrityFault));
    }

    #[test]
    fn orderly_worker_exit_is_not_a_health_fault_after_stop_request() {
        let health = evaluate_host_health(
            HostCoordinatorLifecycle::StopRequested,
            TimePersistenceHealthState::Healthy,
            None,
            [channel(1, ChannelWorkerPhase::Stopped, true)],
        );
        assert_eq!(health.level, HostHealthLevel::Healthy);
        assert!(health.reasons.is_empty());
    }

    #[test]
    fn shared_storage_fault_requires_global_stop() {
        let fault = SharedStorageFault::ReserveBreached {
            available_bytes: 99,
            required_bytes: 100,
        };
        let health = evaluate_host_health(
            HostCoordinatorLifecycle::StopRequested,
            TimePersistenceHealthState::Healthy,
            Some(fault),
            [channel(1, ChannelWorkerPhase::StopRequested, false)],
        );
        assert_eq!(health.level, HostHealthLevel::StopRequired);
        assert!(health
            .reasons
            .contains(&HostHealthReason::SharedStorageFault(fault)));
    }
}
