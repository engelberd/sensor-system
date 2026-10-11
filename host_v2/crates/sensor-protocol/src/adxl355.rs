use core::fmt;

use sensor_core::{SampleAxis, SampleRate};

/// One native, right-aligned ADXL355 ADC code.
///
/// The three sensor registers contain a signed 20-bit two's-complement value.
/// Keeping this type separate prevents a driver decode bug from being hidden
/// by the wider source-stream container.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Adxl355SampleCode(i32);

impl Adxl355SampleCode {
    pub const MIN: i32 = -524_288;
    pub const MAX: i32 = 524_287;

    pub const fn new(value: i32) -> Result<Self, Adxl355SampleCodeError> {
        if value < Self::MIN || value > Self::MAX {
            Err(Adxl355SampleCodeError { value })
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> i32 {
        self.0
    }
}

impl From<Adxl355SampleCode> for SampleAxis {
    fn from(value: Adxl355SampleCode) -> Self {
        match Self::new(value.get()) {
            Ok(value) => value,
            Err(_) => unreachable!("signed 20-bit value must fit signed 24-bit"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Adxl355SampleCodeError {
    value: i32,
}

impl fmt::Display for Adxl355SampleCodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ADXL355 sample code {} is outside signed 20-bit range",
            self.value
        )
    }
}

impl std::error::Error for Adxl355SampleCodeError {}

/// ADXL355 FILTER.ODR_LPF settings from register 0x28.
///
/// The discriminant is the sensor register value. This is a device profile,
/// separate from the generic `SampleRate` used by scheduling and storage.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Adxl355Odr {
    Hz4000 = 0x00,
    Hz2000 = 0x01,
    Hz1000 = 0x02,
    Hz500 = 0x03,
    Hz250 = 0x04,
    Hz125 = 0x05,
    Hz62_5 = 0x06,
    Hz31_25 = 0x07,
    Hz15_625 = 0x08,
    Hz7_8125 = 0x09,
    Hz3_90625 = 0x0a,
}

impl Adxl355Odr {
    pub const ALL: [Self; 11] = [
        Self::Hz4000,
        Self::Hz2000,
        Self::Hz1000,
        Self::Hz500,
        Self::Hz250,
        Self::Hz125,
        Self::Hz62_5,
        Self::Hz31_25,
        Self::Hz15_625,
        Self::Hz7_8125,
        Self::Hz3_90625,
    ];

    pub const fn register_value(self) -> u8 {
        self as u8
    }

    pub const fn from_register_value(value: u8) -> Option<Self> {
        match value {
            0x00 => Some(Self::Hz4000),
            0x01 => Some(Self::Hz2000),
            0x02 => Some(Self::Hz1000),
            0x03 => Some(Self::Hz500),
            0x04 => Some(Self::Hz250),
            0x05 => Some(Self::Hz125),
            0x06 => Some(Self::Hz62_5),
            0x07 => Some(Self::Hz31_25),
            0x08 => Some(Self::Hz15_625),
            0x09 => Some(Self::Hz7_8125),
            0x0a => Some(Self::Hz3_90625),
            _ => None,
        }
    }

    pub const fn sample_rate(self) -> SampleRate {
        match self {
            Self::Hz4000 => SampleRate::HZ_4000,
            Self::Hz2000 => SampleRate::HZ_2000,
            Self::Hz1000 => SampleRate::HZ_1000,
            Self::Hz500 => SampleRate::HZ_500,
            Self::Hz250 => SampleRate::HZ_250,
            Self::Hz125 => SampleRate::HZ_125,
            Self::Hz62_5 => SampleRate::HZ_62_5,
            Self::Hz31_25 => SampleRate::HZ_31_25,
            Self::Hz15_625 => SampleRate::HZ_15_625,
            Self::Hz7_8125 => SampleRate::HZ_7_8125,
            Self::Hz3_90625 => SampleRate::HZ_3_90625,
        }
    }

    pub const fn low_pass_corner(self) -> SampleRate {
        match self.sample_rate().divided_by(4) {
            Ok(rate) => rate,
            Err(_) => panic!("ADXL355 rates are exactly divisible by four"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Adxl355Odr, Adxl355SampleCode};
    use sensor_core::SampleAxis;

    #[test]
    fn validates_native_20_bit_codes_and_converts_without_scaling() {
        let minimum = Adxl355SampleCode::new(Adxl355SampleCode::MIN).unwrap();
        let maximum = Adxl355SampleCode::new(Adxl355SampleCode::MAX).unwrap();

        assert_eq!(SampleAxis::from(minimum).get(), Adxl355SampleCode::MIN);
        assert_eq!(SampleAxis::from(maximum).get(), Adxl355SampleCode::MAX);
        assert!(Adxl355SampleCode::new(Adxl355SampleCode::MIN - 1).is_err());
        assert!(Adxl355SampleCode::new(Adxl355SampleCode::MAX + 1).is_err());
    }

    #[test]
    fn covers_every_defined_odr_lpf_register_setting() {
        assert_eq!(Adxl355Odr::ALL.len(), 11);
        for (register, setting) in Adxl355Odr::ALL.into_iter().enumerate() {
            assert_eq!(setting.register_value(), register as u8);
            assert_eq!(
                Adxl355Odr::from_register_value(register as u8),
                Some(setting)
            );
        }
        assert_eq!(Adxl355Odr::from_register_value(0x0b), None);
        assert_eq!(Adxl355Odr::from_register_value(0xff), None);
    }

    #[test]
    fn preserves_exact_lowest_odr_and_filter_corner() {
        let setting = Adxl355Odr::Hz3_90625;
        assert_eq!(setting.sample_rate().nanohertz(), 3_906_250_000);
        assert_eq!(setting.low_pass_corner().nanohertz(), 976_562_500);
        assert_eq!(
            setting.sample_rate().exact_period_nanoseconds(),
            Some(256_000_000)
        );
    }
}
