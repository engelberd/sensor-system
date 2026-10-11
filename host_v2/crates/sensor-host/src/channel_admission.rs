use core::fmt;

use sensor_core::SampleRate;

use crate::transaction::DurationMicros;

const PARTS_PER_MILLION: u64 = 1_000_000;
const NANO_PER_UNIT: u128 = 1_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeriodicTraffic {
    pub transaction_time: DurationMicros,
    pub interval: DurationMicros,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeTrafficDemand {
    pub output_rate: SampleRate,
    pub samples_per_chunk: u16,
    pub measurement_transaction_time: DurationMicros,
    pub time_sync: Option<PeriodicTraffic>,
    pub health: Option<PeriodicTraffic>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionPolicy {
    /// Maximum planned steady-state channel occupancy.
    pub maximum_utilization_ppm: u32,
    /// Capacity left unused by nominal traffic for retransmission and recovery.
    pub recovery_reserve_ppm: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionReport {
    pub nominal_utilization_ppm: u32,
    pub recovery_reserve_ppm: u32,
    pub required_utilization_ppm: u32,
    pub maximum_utilization_ppm: u32,
}

impl AdmissionReport {
    pub const fn accepted(self) -> bool {
        self.required_utilization_ppm <= self.maximum_utilization_ppm
    }

    pub const fn remaining_ppm(self) -> u32 {
        self.maximum_utilization_ppm
            .saturating_sub(self.required_utilization_ppm)
    }
}

/// Performs pre-start capacity checks from measured transaction costs.
///
/// This module deliberately does not estimate UART timing itself. The caller
/// supplies conservative HIL-derived transaction times for its adapter, cable
/// and framing version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelAdmission {
    policy: AdmissionPolicy,
}

impl ChannelAdmission {
    pub fn new(policy: AdmissionPolicy) -> Result<Self, AdmissionError> {
        if policy.maximum_utilization_ppm == 0
            || u64::from(policy.maximum_utilization_ppm) > PARTS_PER_MILLION
        {
            return Err(AdmissionError::InvalidMaximumUtilization);
        }
        if policy.recovery_reserve_ppm >= policy.maximum_utilization_ppm {
            return Err(AdmissionError::RecoveryReserveConsumesBudget);
        }
        Ok(Self { policy })
    }

    pub fn evaluate(&self, nodes: &[NodeTrafficDemand]) -> Result<AdmissionReport, AdmissionError> {
        let mut nominal_ppm = 0_u128;
        for demand in nodes {
            if demand.samples_per_chunk == 0 {
                return Err(AdmissionError::ZeroSamplesPerChunk);
            }
            nominal_ppm = nominal_ppm
                .checked_add(measurement_ppm(*demand))
                .and_then(|value| checked_add_periodic(value, demand.time_sync))
                .and_then(|value| checked_add_periodic(value, demand.health))
                .ok_or(AdmissionError::ArithmeticOverflow)?;
        }

        let nominal_utilization_ppm =
            u32::try_from(nominal_ppm).map_err(|_| AdmissionError::DemandExceedsChannel)?;
        let required = nominal_ppm
            .checked_add(u128::from(self.policy.recovery_reserve_ppm))
            .ok_or(AdmissionError::ArithmeticOverflow)?;
        let required_utilization_ppm =
            u32::try_from(required).map_err(|_| AdmissionError::DemandExceedsChannel)?;

        Ok(AdmissionReport {
            nominal_utilization_ppm,
            recovery_reserve_ppm: self.policy.recovery_reserve_ppm,
            required_utilization_ppm,
            maximum_utilization_ppm: self.policy.maximum_utilization_ppm,
        })
    }
}

fn measurement_ppm(demand: NodeTrafficDemand) -> u128 {
    div_ceil(
        u128::from(demand.output_rate.nanohertz())
            * u128::from(demand.measurement_transaction_time.get()),
        u128::from(demand.samples_per_chunk) * NANO_PER_UNIT,
    )
}

fn checked_add_periodic(current: u128, traffic: Option<PeriodicTraffic>) -> Option<u128> {
    let Some(traffic) = traffic else {
        return Some(current);
    };
    let ppm = div_ceil(
        u128::from(traffic.transaction_time.get()) * u128::from(PARTS_PER_MILLION),
        u128::from(traffic.interval.get()),
    );
    current.checked_add(ppm)
}

const fn div_ceil(numerator: u128, denominator: u128) -> u128 {
    numerator / denominator + if numerator % denominator == 0 { 0 } else { 1 }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionError {
    InvalidMaximumUtilization,
    RecoveryReserveConsumesBudget,
    ZeroSamplesPerChunk,
    ArithmeticOverflow,
    DemandExceedsChannel,
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidMaximumUtilization => {
                "maximum channel utilization must be between 1 and 1,000,000 ppm"
            }
            Self::RecoveryReserveConsumesBudget => {
                "recovery reserve must be smaller than the utilization limit"
            }
            Self::ZeroSamplesPerChunk => "measurement chunk must contain at least one sample",
            Self::ArithmeticOverflow => "channel demand calculation overflowed",
            Self::DemandExceedsChannel => "channel demand exceeds the representable line capacity",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for AdmissionError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn duration(value: u64) -> DurationMicros {
        DurationMicros::new(value).unwrap()
    }

    fn demand(rate: SampleRate) -> NodeTrafficDemand {
        NodeTrafficDemand {
            output_rate: rate,
            samples_per_chunk: 32,
            measurement_transaction_time: duration(36_900),
            time_sync: Some(PeriodicTraffic {
                transaction_time: duration(4_000),
                interval: duration(30_000_000),
            }),
            health: Some(PeriodicTraffic {
                transaction_time: duration(4_000),
                interval: duration(10_000_000),
            }),
        }
    }

    fn admission() -> ChannelAdmission {
        ChannelAdmission::new(AdmissionPolicy {
            maximum_utilization_ppm: 700_000,
            recovery_reserve_ppm: 100_000,
        })
        .unwrap()
    }

    #[test]
    fn admits_four_nodes_at_125_hz_with_conservative_example_costs() {
        let report = admission()
            .evaluate(&[demand(SampleRate::HZ_125); 4])
            .unwrap();
        assert!(report.accepted());
        assert_eq!(report.nominal_utilization_ppm, 578_700);
        assert_eq!(report.required_utilization_ppm, 678_700);
    }

    #[test]
    fn reserve_is_part_of_the_admission_decision() {
        let report = ChannelAdmission::new(AdmissionPolicy {
            maximum_utilization_ppm: 600_000,
            recovery_reserve_ppm: 100_000,
        })
        .unwrap()
        .evaluate(&[demand(SampleRate::HZ_125); 4])
        .unwrap();
        assert!(!report.accepted());
        assert_eq!(report.remaining_ppm(), 0);
    }

    #[test]
    fn rejects_zero_chunk_without_dividing() {
        let mut invalid = demand(SampleRate::HZ_125);
        invalid.samples_per_chunk = 0;
        assert_eq!(
            admission().evaluate(&[invalid]),
            Err(AdmissionError::ZeroSamplesPerChunk)
        );
    }

    #[test]
    fn rounds_every_component_up_conservatively() {
        let report = ChannelAdmission::new(AdmissionPolicy {
            maximum_utilization_ppm: 1_000_000,
            recovery_reserve_ppm: 0,
        })
        .unwrap()
        .evaluate(&[NodeTrafficDemand {
            output_rate: SampleRate::from_hertz(1).unwrap(),
            samples_per_chunk: 3,
            measurement_transaction_time: duration(1),
            time_sync: Some(PeriodicTraffic {
                transaction_time: duration(1),
                interval: duration(3_000_000),
            }),
            health: None,
        }])
        .unwrap();
        assert_eq!(report.nominal_utilization_ppm, 2);
    }
}
