use core::fmt;

/// Exact positive frequency stored as an integer number of nanohertz.
///
/// Nanohertz represent every ADXL355 ODR and low-pass corner exactly, including
/// 7.8125 Hz, 3.90625 Hz and 0.9765625 Hz, without floating point.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SampleRate(u64);

impl SampleRate {
    pub const HZ_4000: Self = Self(4_000_000_000_000);
    pub const HZ_2000: Self = Self(2_000_000_000_000);
    pub const HZ_1000: Self = Self(1_000_000_000_000);
    pub const HZ_500: Self = Self(500_000_000_000);
    pub const HZ_250: Self = Self(250_000_000_000);
    pub const HZ_125: Self = Self(125_000_000_000);
    pub const HZ_62_5: Self = Self(62_500_000_000);
    pub const HZ_31_25: Self = Self(31_250_000_000);
    pub const HZ_15_625: Self = Self(15_625_000_000);
    pub const HZ_7_8125: Self = Self(7_812_500_000);
    pub const HZ_3_90625: Self = Self(3_906_250_000);

    pub const fn from_nanohertz(value: u64) -> Result<Self, SampleRateError> {
        if value == 0 {
            Err(SampleRateError::Zero)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn from_microhertz(value: u64) -> Result<Self, SampleRateError> {
        match value.checked_mul(1_000) {
            Some(value) => Self::from_nanohertz(value),
            None => Err(SampleRateError::Overflow),
        }
    }

    pub const fn from_millihertz(value: u64) -> Result<Self, SampleRateError> {
        match value.checked_mul(1_000_000) {
            Some(value) => Self::from_nanohertz(value),
            None => Err(SampleRateError::Overflow),
        }
    }

    pub const fn from_hertz(value: u64) -> Result<Self, SampleRateError> {
        match value.checked_mul(1_000_000_000) {
            Some(value) => Self::from_nanohertz(value),
            None => Err(SampleRateError::Overflow),
        }
    }

    pub const fn nanohertz(self) -> u64 {
        self.0
    }

    /// Returns an integer period only when the frequency has an exact whole
    /// nanosecond period. Callers cannot silently accept truncation.
    pub const fn exact_period_nanoseconds(self) -> Option<u64> {
        const NANOHERTZ_NANOSECONDS: u64 = 1_000_000_000_000_000_000;
        if NANOHERTZ_NANOSECONDS % self.0 == 0 {
            Some(NANOHERTZ_NANOSECONDS / self.0)
        } else {
            None
        }
    }

    pub const fn divided_by(self, divisor: u32) -> Result<Self, SampleRateError> {
        if divisor == 0 {
            return Err(SampleRateError::ZeroDivisor);
        }
        let divisor = divisor as u64;
        if self.0 % divisor != 0 {
            return Err(SampleRateError::InexactDivision);
        }
        Self::from_nanohertz(self.0 / divisor)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SampleRateError {
    Zero,
    Overflow,
    ZeroDivisor,
    InexactDivision,
}

impl fmt::Display for SampleRateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Zero => "sample rate must be greater than zero",
            Self::Overflow => "sample rate exceeds the integer representation",
            Self::ZeroDivisor => "sample-rate divisor must be greater than zero",
            Self::InexactDivision => "sample rate cannot be divided exactly",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for SampleRateError {}

#[cfg(test)]
mod tests {
    use super::{SampleRate, SampleRateError};

    #[test]
    fn represents_every_fractional_adxl355_rate_exactly() {
        assert_eq!(SampleRate::HZ_7_8125.nanohertz(), 7_812_500_000);
        assert_eq!(SampleRate::HZ_3_90625.nanohertz(), 3_906_250_000);
        assert_eq!(
            SampleRate::HZ_3_90625.exact_period_nanoseconds(),
            Some(256_000_000)
        );
    }

    #[test]
    fn reports_inexact_period_instead_of_truncating_it() {
        let rate = SampleRate::from_hertz(3).unwrap();
        assert_eq!(rate.exact_period_nanoseconds(), None);
    }

    #[test]
    fn validates_zero_overflow_and_division() {
        assert_eq!(SampleRate::from_nanohertz(0), Err(SampleRateError::Zero));
        assert_eq!(
            SampleRate::from_hertz(u64::MAX),
            Err(SampleRateError::Overflow)
        );
        assert_eq!(
            SampleRate::HZ_125.divided_by(0),
            Err(SampleRateError::ZeroDivisor)
        );
        assert_eq!(
            SampleRate::from_nanohertz(10).unwrap().divided_by(3),
            Err(SampleRateError::InexactDivision)
        );
    }
}
