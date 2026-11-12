use core::fmt;

use sensor_protocol::{ObservabilityProfile, BROADCAST_ADDRESS, HOST_ADDRESS};

use crate::transaction::{DurationMicros, MonotonicMicros};

/// A class of work competing for one half-duplex channel.
///
/// The scheduler only chooses work. Encoding, retries, persistence and port
/// ownership remain in their respective services and `ChannelAgent`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrafficClass {
    Control,
    Measurement,
    TimeSync,
    Health,
    Telemetry,
    Diagnostics,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkCandidate {
    pub work_id: u64,
    pub node_address: u8,
    pub class: TrafficClass,
    pub ready_at: MonotonicMicros,
    pub deadline: Option<MonotonicMicros>,
    pub queued_at: MonotonicMicros,
    /// Conservative time until undelivered data can leave node retention.
    /// Zero means that loss is already possible.
    pub time_to_overflow_us: Option<u64>,
    pub estimated_slot: DurationMicros,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScheduleContext {
    pub now: MonotonicMicros,
    pub observability: ObservabilityProfile,
    /// Remaining channel time assigned to service diagnostics in the current
    /// accounting window. Production traffic never consumes this budget.
    pub diagnostic_budget_us: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScheduledWork {
    pub candidate_index: usize,
    pub work_id: u64,
    pub node_address: u8,
    pub class: TrafficClass,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelSchedulerPolicy {
    /// A measurement enters recovery priority when its retained data can be
    /// overwritten within this interval.
    pub overflow_guard: DurationMicros,
    /// No selected transaction may monopolize the channel for longer.
    pub max_slot: DurationMicros,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelScheduler {
    policy: ChannelSchedulerPolicy,
}

impl ChannelScheduler {
    pub const fn new(policy: ChannelSchedulerPolicy) -> Self {
        Self { policy }
    }

    /// Selects at most one ready transaction without mutating service state.
    ///
    /// Ordering is deterministic: imminent journal overflow, work whose
    /// completion deadline has arrived, ordinary measurement recovery, an
    /// explicit control request, and finally bounded diagnostics. Future sync
    /// and health candidates wait until their completion deadline approaches.
    pub fn select(
        &self,
        context: ScheduleContext,
        candidates: &[WorkCandidate],
    ) -> Result<Option<ScheduledWork>, SchedulerError> {
        let mut selected: Option<(usize, Rank)> = None;

        for (index, candidate) in candidates.iter().copied().enumerate() {
            self.validate_candidate(candidate)?;
            let Some(rank) = self.rank(context, candidate) else {
                continue;
            };
            if selected
                .as_ref()
                .map_or(true, |(_, selected_rank)| rank < *selected_rank)
            {
                selected = Some((index, rank));
            }
        }

        Ok(selected.map(|(candidate_index, _)| {
            let candidate = candidates[candidate_index];
            ScheduledWork {
                candidate_index,
                work_id: candidate.work_id,
                node_address: candidate.node_address,
                class: candidate.class,
            }
        }))
    }

    fn validate_candidate(&self, candidate: WorkCandidate) -> Result<(), SchedulerError> {
        if candidate.node_address == HOST_ADDRESS || candidate.node_address == BROADCAST_ADDRESS {
            return Err(SchedulerError::InvalidNodeAddress(candidate.node_address));
        }
        if candidate.estimated_slot.get() > self.policy.max_slot.get() {
            return Err(SchedulerError::SlotExceedsPolicy {
                work_id: candidate.work_id,
                estimated_us: candidate.estimated_slot.get(),
                maximum_us: self.policy.max_slot.get(),
            });
        }
        match candidate.class {
            TrafficClass::Measurement => {
                if candidate.deadline.is_none() {
                    return Err(SchedulerError::MissingDeadline(candidate.work_id));
                }
                if candidate.time_to_overflow_us.is_none() {
                    return Err(SchedulerError::MissingOverflowEstimate(candidate.work_id));
                }
            }
            TrafficClass::Control
            | TrafficClass::TimeSync
            | TrafficClass::Health
            | TrafficClass::Telemetry => {
                if candidate.deadline.is_none() {
                    return Err(SchedulerError::MissingDeadline(candidate.work_id));
                }
                if candidate.time_to_overflow_us.is_some() {
                    return Err(SchedulerError::OverflowOnNonMeasurement(candidate.work_id));
                }
            }
            TrafficClass::Diagnostics => {
                if candidate.deadline.is_some() {
                    return Err(SchedulerError::DiagnosticDeadline(candidate.work_id));
                }
                if candidate.time_to_overflow_us.is_some() {
                    return Err(SchedulerError::OverflowOnNonMeasurement(candidate.work_id));
                }
            }
        }
        Ok(())
    }

    fn rank(&self, context: ScheduleContext, candidate: WorkCandidate) -> Option<Rank> {
        if candidate.ready_at > context.now {
            return None;
        }
        if candidate.class == TrafficClass::Diagnostics
            && (context.observability != ObservabilityProfile::Diagnostic
                || candidate.estimated_slot.get() > context.diagnostic_budget_us)
        {
            return None;
        }

        if let Some(time_to_overflow_us) = candidate.time_to_overflow_us {
            if time_to_overflow_us <= self.policy.overflow_guard.get() {
                return Some(Rank::new(
                    0,
                    time_to_overflow_us,
                    candidate.queued_at.get(),
                    candidate,
                ));
            }
        }

        if let Some(deadline) = candidate.deadline {
            let completes_at = context
                .now
                .get()
                .saturating_add(candidate.estimated_slot.get());
            if completes_at >= deadline.get() {
                return Some(Rank::new(
                    1,
                    deadline.get(),
                    class_order(candidate.class),
                    candidate,
                ));
            }
        }

        match candidate.class {
            TrafficClass::Measurement => Some(Rank::new(
                2,
                candidate.time_to_overflow_us.unwrap_or(u64::MAX),
                candidate.queued_at.get(),
                candidate,
            )),
            TrafficClass::Control => Some(Rank::new(3, candidate.queued_at.get(), 0, candidate)),
            TrafficClass::Diagnostics => {
                Some(Rank::new(4, candidate.queued_at.get(), 0, candidate))
            }
            TrafficClass::TimeSync | TrafficClass::Health | TrafficClass::Telemetry => None,
        }
    }
}

const fn class_order(class: TrafficClass) -> u64 {
    match class {
        TrafficClass::Control => 0,
        TrafficClass::Measurement => 1,
        TrafficClass::TimeSync => 2,
        TrafficClass::Health => 3,
        TrafficClass::Telemetry => 4,
        TrafficClass::Diagnostics => 5,
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Rank {
    tier: u8,
    primary: u64,
    secondary: u64,
    node_address: u8,
    work_id: u64,
}

impl Rank {
    const fn new(tier: u8, primary: u64, secondary: u64, candidate: WorkCandidate) -> Self {
        Self {
            tier,
            primary,
            secondary,
            node_address: candidate.node_address,
            work_id: candidate.work_id,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulerError {
    InvalidNodeAddress(u8),
    SlotExceedsPolicy {
        work_id: u64,
        estimated_us: u64,
        maximum_us: u64,
    },
    OverflowOnNonMeasurement(u64),
    MissingDeadline(u64),
    MissingOverflowEstimate(u64),
    DiagnosticDeadline(u64),
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidNodeAddress(address) => {
                write!(formatter, "address {address} cannot identify a node")
            }
            Self::SlotExceedsPolicy {
                work_id,
                estimated_us,
                maximum_us,
            } => write!(
                formatter,
                "work {work_id} needs {estimated_us} us, above the {maximum_us} us slot limit"
            ),
            Self::OverflowOnNonMeasurement(work_id) => write!(
                formatter,
                "non-measurement work {work_id} declares journal overflow timing"
            ),
            Self::MissingDeadline(work_id) => {
                write!(formatter, "production work {work_id} has no deadline")
            }
            Self::MissingOverflowEstimate(work_id) => write!(
                formatter,
                "measurement work {work_id} has no journal overflow estimate"
            ),
            Self::DiagnosticDeadline(work_id) => write!(
                formatter,
                "diagnostic work {work_id} cannot declare a production deadline"
            ),
        }
    }
}

impl std::error::Error for SchedulerError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn duration(value: u64) -> DurationMicros {
        DurationMicros::new(value).unwrap()
    }

    fn candidate(work_id: u64, node_address: u8, class: TrafficClass) -> WorkCandidate {
        WorkCandidate {
            work_id,
            node_address,
            class,
            ready_at: MonotonicMicros::new(0),
            deadline: (class != TrafficClass::Diagnostics)
                .then_some(MonotonicMicros::new(2_000_000)),
            queued_at: MonotonicMicros::new(0),
            time_to_overflow_us: (class == TrafficClass::Measurement).then_some(2_000_000),
            estimated_slot: duration(1_000),
        }
    }

    fn scheduler() -> ChannelScheduler {
        ChannelScheduler::new(ChannelSchedulerPolicy {
            overflow_guard: duration(100_000),
            max_slot: duration(50_000),
        })
    }

    fn context(profile: ObservabilityProfile, budget_us: u64) -> ScheduleContext {
        ScheduleContext {
            now: MonotonicMicros::new(1_000_000),
            observability: profile,
            diagnostic_budget_us: budget_us,
        }
    }

    #[test]
    fn imminent_overflow_preempts_other_deadlines() {
        let mut sync = candidate(1, 1, TrafficClass::TimeSync);
        sync.deadline = Some(MonotonicMicros::new(900_000));
        let mut measurement = candidate(2, 2, TrafficClass::Measurement);
        measurement.time_to_overflow_us = Some(50_000);

        let selected = scheduler()
            .select(
                context(ObservabilityProfile::Production, 1),
                &[sync, measurement],
            )
            .unwrap()
            .unwrap();

        assert_eq!(selected.work_id, 2);
    }

    #[test]
    fn sync_completion_deadline_preempts_normal_measurement() {
        let mut measurement = candidate(1, 1, TrafficClass::Measurement);
        measurement.time_to_overflow_us = Some(500_000);
        let mut sync = candidate(2, 2, TrafficClass::TimeSync);
        sync.deadline = Some(MonotonicMicros::new(1_000_500));

        let selected = scheduler()
            .select(
                context(ObservabilityProfile::Production, 1),
                &[measurement, sync],
            )
            .unwrap()
            .unwrap();

        assert_eq!(selected.work_id, 2);
    }

    #[test]
    fn telemetry_waits_for_its_deadline_then_gets_one_bounded_slot() {
        let measurement = candidate(1, 1, TrafficClass::Measurement);
        let telemetry = candidate(2, 2, TrafficClass::Telemetry);
        let selected = scheduler()
            .select(
                context(ObservabilityProfile::Production, 0),
                &[measurement, telemetry],
            )
            .unwrap()
            .unwrap();
        assert_eq!(selected.work_id, 1);

        let mut due = telemetry;
        due.deadline = Some(MonotonicMicros::new(1_000_500));
        let selected = scheduler()
            .select(
                context(ObservabilityProfile::Production, 0),
                &[measurement, due],
            )
            .unwrap()
            .unwrap();
        assert_eq!(selected.work_id, 2);
    }

    #[test]
    fn measurement_deadline_prevents_low_rate_node_starvation() {
        let mut urgent_by_capacity = candidate(1, 1, TrafficClass::Measurement);
        urgent_by_capacity.time_to_overflow_us = Some(400_000);
        let mut waited_long_enough = candidate(2, 2, TrafficClass::Measurement);
        waited_long_enough.time_to_overflow_us = Some(900_000);
        waited_long_enough.deadline = Some(MonotonicMicros::new(1_000_500));

        let selected = scheduler()
            .select(
                context(ObservabilityProfile::Production, 1),
                &[urgent_by_capacity, waited_long_enough],
            )
            .unwrap()
            .unwrap();

        assert_eq!(selected.work_id, 2);
    }

    #[test]
    fn diagnostics_require_profile_and_remaining_budget() {
        let diagnostics = candidate(7, 1, TrafficClass::Diagnostics);
        assert!(scheduler()
            .select(
                context(ObservabilityProfile::Production, 10_000),
                &[diagnostics]
            )
            .unwrap()
            .is_none());
        assert!(scheduler()
            .select(
                context(ObservabilityProfile::Diagnostic, 500),
                &[diagnostics]
            )
            .unwrap()
            .is_none());
        assert_eq!(
            scheduler()
                .select(
                    context(ObservabilityProfile::Diagnostic, 1_000),
                    &[diagnostics]
                )
                .unwrap()
                .unwrap()
                .work_id,
            7
        );
    }

    #[test]
    fn retry_backoff_keeps_candidate_ineligible() {
        let mut measurement = candidate(1, 1, TrafficClass::Measurement);
        measurement.ready_at = MonotonicMicros::new(1_000_001);
        assert!(scheduler()
            .select(context(ObservabilityProfile::Production, 1), &[measurement])
            .unwrap()
            .is_none());
    }

    #[test]
    fn rejects_work_that_would_monopolize_the_channel() {
        let mut measurement = candidate(9, 1, TrafficClass::Measurement);
        measurement.estimated_slot = duration(50_001);
        assert_eq!(
            scheduler().select(context(ObservabilityProfile::Production, 1), &[measurement]),
            Err(SchedulerError::SlotExceedsPolicy {
                work_id: 9,
                estimated_us: 50_001,
                maximum_us: 50_000,
            })
        );
    }

    #[test]
    fn rejects_incomplete_production_and_privileged_diagnostic_candidates() {
        let mut measurement = candidate(1, 1, TrafficClass::Measurement);
        measurement.time_to_overflow_us = None;
        assert_eq!(
            scheduler().select(context(ObservabilityProfile::Production, 0), &[measurement]),
            Err(SchedulerError::MissingOverflowEstimate(1))
        );

        let mut diagnostics = candidate(2, 1, TrafficClass::Diagnostics);
        diagnostics.deadline = Some(MonotonicMicros::new(1));
        assert_eq!(
            scheduler().select(
                context(ObservabilityProfile::Diagnostic, 10_000),
                &[diagnostics]
            ),
            Err(SchedulerError::DiagnosticDeadline(2))
        );
    }
}
