use core::fmt;

use crate::BootEpoch;

/// One raw four-timestamp exchange.
///
/// `t1` is captured by the host after the complete request has left the
/// transmitter, `t2` by the node after the complete request is accepted,
/// `t3` by the node at the start of response transmission, and `t4` by the
/// host when the complete response is accepted. All values are monotonic
/// microseconds in their respective clock domains.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockObservation {
    pub boot_epoch: BootEpoch,
    pub host_send_us: u64,
    pub node_receive_us: u64,
    pub node_send_us: u64,
    pub host_receive_us: u64,
}

impl ClockObservation {
    pub const fn new(
        boot_epoch: BootEpoch,
        host_send_us: u64,
        node_receive_us: u64,
        node_send_us: u64,
        host_receive_us: u64,
    ) -> Result<Self, ClockObservationError> {
        if boot_epoch.get() == 0 {
            return Err(ClockObservationError::ZeroBootEpoch);
        }
        if host_receive_us < host_send_us {
            return Err(ClockObservationError::HostClockRegressed);
        }
        if node_send_us < node_receive_us {
            return Err(ClockObservationError::NodeClockRegressed);
        }
        let host_elapsed = host_receive_us - host_send_us;
        let node_processing = node_send_us - node_receive_us;
        if node_processing > host_elapsed {
            return Err(ClockObservationError::ProcessingExceedsRoundTrip);
        }
        Ok(Self {
            boot_epoch,
            host_send_us,
            node_receive_us,
            node_send_us,
            host_receive_us,
        })
    }

    pub const fn host_midpoint_us(self) -> u64 {
        midpoint(self.host_send_us, self.host_receive_us)
    }

    pub const fn node_midpoint_us(self) -> u64 {
        midpoint(self.node_receive_us, self.node_send_us)
    }

    /// Network delay after subtracting time spent processing on the node.
    pub const fn path_delay_us(self) -> u64 {
        (self.host_receive_us - self.host_send_us) - (self.node_send_us - self.node_receive_us)
    }

    /// Conservative one-way uncertainty under the symmetric-delay model.
    /// Odd microseconds round upward.
    pub const fn uncertainty_us(self) -> u64 {
        self.path_delay_us().div_ceil(2)
    }
}

const fn midpoint(first: u64, second: u64) -> u64 {
    first + (second - first) / 2
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockObservationError {
    ZeroBootEpoch,
    HostClockRegressed,
    NodeClockRegressed,
    ProcessingExceedsRoundTrip,
}

impl fmt::Display for ClockObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid clock observation: {self:?}")
    }
}

impl std::error::Error for ClockObservationError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockModelPolicy {
    pub max_observation_uncertainty_us: u64,
    pub minimum_anchor_separation_us: u64,
    pub maximum_drift_ppm: u32,
    pub maximum_holdover_us: u64,
}

impl ClockModelPolicy {
    pub const fn validate(self) -> Result<Self, ClockModelError> {
        if self.max_observation_uncertainty_us == 0
            || self.minimum_anchor_separation_us == 0
            || self.maximum_drift_ppm == 0
            || self.maximum_holdover_us == 0
        {
            Err(ClockModelError::InvalidPolicy)
        } else {
            Ok(self)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClockAnchor {
    node_us: u64,
    host_us: u64,
    uncertainty_us: u64,
}

impl From<ClockObservation> for ClockAnchor {
    fn from(value: ClockObservation) -> Self {
        Self {
            node_us: value.node_midpoint_us(),
            host_us: value.host_midpoint_us(),
            uncertainty_us: value.uncertainty_us(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockModelState {
    Acquiring,
    Locked,
    Holdover,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockEstimate {
    pub host_monotonic_us: u64,
    pub uncertainty_us: u64,
    pub state: ClockModelState,
}

/// Bounded online model for one node boot epoch.
///
/// Raw observations remain the source data. This model keeps only two anchors
/// and is therefore suitable for scheduling and online quality decisions; an
/// offline reconstruction may fit all persisted observations later.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockModel {
    boot_epoch: BootEpoch,
    policy: ClockModelPolicy,
    first: Option<ClockAnchor>,
    second: Option<ClockAnchor>,
}

impl ClockModel {
    pub fn new(boot_epoch: BootEpoch, policy: ClockModelPolicy) -> Result<Self, ClockModelError> {
        if boot_epoch.get() == 0 {
            return Err(ClockModelError::ZeroBootEpoch);
        }
        Ok(Self {
            boot_epoch,
            policy: policy.validate()?,
            first: None,
            second: None,
        })
    }

    pub const fn boot_epoch(self) -> BootEpoch {
        self.boot_epoch
    }

    pub const fn state(self) -> ClockModelState {
        if self.second.is_some() {
            ClockModelState::Locked
        } else {
            ClockModelState::Acquiring
        }
    }

    /// Accepts a quality-bounded observation without ever mutating the model
    /// on rejection.
    pub fn observe(&mut self, observation: ClockObservation) -> Result<(), ClockModelError> {
        if observation.boot_epoch != self.boot_epoch {
            return Err(ClockModelError::BootEpochMismatch);
        }
        let candidate = ClockAnchor::from(observation);
        if candidate.uncertainty_us > self.policy.max_observation_uncertainty_us {
            return Err(ClockModelError::ObservationTooUncertain);
        }
        let Some(first) = self.first else {
            self.first = Some(candidate);
            return Ok(());
        };
        let latest = self.second.unwrap_or(first);
        if candidate.node_us <= latest.node_us || candidate.host_us <= latest.host_us {
            return Err(ClockModelError::NonMonotonicObservation);
        }

        if self.second.is_none()
            && candidate.node_us - first.node_us < self.policy.minimum_anchor_separation_us
        {
            if candidate.uncertainty_us < first.uncertainty_us {
                self.first = Some(candidate);
            }
            return Ok(());
        }

        let baseline = self.second.unwrap_or(first);
        validate_drift(baseline, candidate, self.policy.maximum_drift_ppm)?;
        self.first = Some(baseline);
        self.second = Some(candidate);
        Ok(())
    }

    pub fn estimate(&self, node_us: u64) -> Result<ClockEstimate, ClockModelError> {
        let (first, second) = match (self.first, self.second) {
            (Some(first), Some(second)) => (first, second),
            _ => return Err(ClockModelError::ModelNotLocked),
        };
        let outside_us = if node_us < first.node_us {
            first.node_us - node_us
        } else {
            node_us.saturating_sub(second.node_us)
        };
        if outside_us > self.policy.maximum_holdover_us {
            return Err(ClockModelError::HoldoverExpired);
        }
        let host_monotonic_us = interpolate(first, second, node_us)?;
        let drift_growth = mul_div_ceil(
            outside_us,
            u64::from(self.policy.maximum_drift_ppm),
            1_000_000,
        )?;
        let uncertainty_us = first
            .uncertainty_us
            .max(second.uncertainty_us)
            .checked_add(drift_growth)
            .ok_or(ClockModelError::ArithmeticOverflow)?;
        Ok(ClockEstimate {
            host_monotonic_us,
            uncertainty_us,
            state: if outside_us == 0 {
                ClockModelState::Locked
            } else {
                ClockModelState::Holdover
            },
        })
    }
}

fn validate_drift(
    first: ClockAnchor,
    second: ClockAnchor,
    maximum_drift_ppm: u32,
) -> Result<(), ClockModelError> {
    let node_delta = second.node_us - first.node_us;
    let host_delta = second.host_us - first.host_us;
    let difference = node_delta.abs_diff(host_delta);
    let allowed = mul_div_ceil(node_delta, u64::from(maximum_drift_ppm), 1_000_000)?;
    if difference > allowed {
        Err(ClockModelError::DriftOutOfRange)
    } else {
        Ok(())
    }
}

fn interpolate(
    first: ClockAnchor,
    second: ClockAnchor,
    node_us: u64,
) -> Result<u64, ClockModelError> {
    let node_delta = i128::from(second.node_us - first.node_us);
    let host_delta = i128::from(second.host_us - first.host_us);
    let relative = i128::from(node_us) - i128::from(first.node_us);
    let mapped_delta = relative
        .checked_mul(host_delta)
        .ok_or(ClockModelError::ArithmeticOverflow)?
        / node_delta;
    let mapped = i128::from(first.host_us)
        .checked_add(mapped_delta)
        .ok_or(ClockModelError::ArithmeticOverflow)?;
    u64::try_from(mapped).map_err(|_| ClockModelError::ArithmeticOverflow)
}

fn mul_div_ceil(value: u64, multiplier: u64, divisor: u64) -> Result<u64, ClockModelError> {
    let product = u128::from(value)
        .checked_mul(u128::from(multiplier))
        .ok_or(ClockModelError::ArithmeticOverflow)?;
    let rounded = product.div_ceil(u128::from(divisor));
    u64::try_from(rounded).map_err(|_| ClockModelError::ArithmeticOverflow)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockModelError {
    InvalidPolicy,
    ZeroBootEpoch,
    BootEpochMismatch,
    ObservationTooUncertain,
    NonMonotonicObservation,
    DriftOutOfRange,
    ModelNotLocked,
    HoldoverExpired,
    ArithmeticOverflow,
}

impl fmt::Display for ClockModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "clock model error: {self:?}")
    }
}

impl std::error::Error for ClockModelError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> ClockModelPolicy {
        ClockModelPolicy {
            max_observation_uncertainty_us: 100,
            minimum_anchor_separation_us: 1_000_000,
            maximum_drift_ppm: 100,
            maximum_holdover_us: 60_000_000,
        }
    }

    fn observation(node_midpoint: u64, host_midpoint: u64, path_delay: u64) -> ClockObservation {
        let half = path_delay / 2;
        ClockObservation::new(
            BootEpoch::new(7),
            host_midpoint - half,
            node_midpoint,
            node_midpoint,
            host_midpoint + path_delay - half,
        )
        .unwrap()
    }

    #[test]
    fn observation_subtracts_node_processing_and_rounds_uncertainty_up() {
        let value = ClockObservation::new(BootEpoch::new(1), 100, 1_000, 1_004, 109).unwrap();
        assert_eq!(value.path_delay_us(), 5);
        assert_eq!(value.uncertainty_us(), 3);
        assert_eq!(value.host_midpoint_us(), 104);
        assert_eq!(value.node_midpoint_us(), 1_002);
    }

    #[test]
    fn malformed_timestamp_order_is_rejected() {
        assert_eq!(
            ClockObservation::new(BootEpoch::new(1), 10, 100, 101, 9),
            Err(ClockObservationError::HostClockRegressed)
        );
        assert_eq!(
            ClockObservation::new(BootEpoch::new(1), 10, 101, 100, 20),
            Err(ClockObservationError::NodeClockRegressed)
        );
        assert_eq!(
            ClockObservation::new(BootEpoch::new(1), 10, 100, 111, 20),
            Err(ClockObservationError::ProcessingExceedsRoundTrip)
        );
    }

    #[test]
    fn two_anchors_lock_and_map_node_time_with_measured_drift() {
        let mut model = ClockModel::new(BootEpoch::new(7), policy()).unwrap();
        model
            .observe(observation(1_000_000, 5_000_000, 20))
            .unwrap();
        assert_eq!(model.state(), ClockModelState::Acquiring);
        model
            .observe(observation(2_000_000, 6_000_050, 10))
            .unwrap();
        assert_eq!(model.state(), ClockModelState::Locked);
        let estimate = model.estimate(1_500_000).unwrap();
        assert_eq!(estimate.host_monotonic_us, 5_500_025);
        assert_eq!(estimate.uncertainty_us, 10);
        assert_eq!(estimate.state, ClockModelState::Locked);
    }

    #[test]
    fn holdover_grows_uncertainty_and_expires() {
        let mut model = ClockModel::new(BootEpoch::new(7), policy()).unwrap();
        model
            .observe(observation(1_000_000, 5_000_000, 20))
            .unwrap();
        model
            .observe(observation(2_000_000, 6_000_050, 20))
            .unwrap();
        let estimate = model.estimate(12_000_000).unwrap();
        assert_eq!(estimate.state, ClockModelState::Holdover);
        assert_eq!(estimate.uncertainty_us, 1_010);
        assert_eq!(
            model.estimate(62_000_001),
            Err(ClockModelError::HoldoverExpired)
        );
    }

    #[test]
    fn rejected_outlier_does_not_mutate_the_locked_model() {
        let mut model = ClockModel::new(BootEpoch::new(7), policy()).unwrap();
        model
            .observe(observation(1_000_000, 5_000_000, 20))
            .unwrap();
        model
            .observe(observation(2_000_000, 6_000_050, 20))
            .unwrap();
        let before = model;
        assert_eq!(
            model.observe(observation(3_000_000, 7_100_000, 20)),
            Err(ClockModelError::DriftOutOfRange)
        );
        assert_eq!(model, before);
    }

    #[test]
    fn a_better_early_anchor_replaces_a_noisy_one_without_locking() {
        let mut model = ClockModel::new(BootEpoch::new(7), policy()).unwrap();
        model
            .observe(observation(1_000_000, 5_000_000, 100))
            .unwrap();
        model
            .observe(observation(1_100_000, 5_100_000, 10))
            .unwrap();
        assert_eq!(model.state(), ClockModelState::Acquiring);
        model
            .observe(observation(2_100_000, 6_100_050, 10))
            .unwrap();
        assert_eq!(model.state(), ClockModelState::Locked);
    }
}
