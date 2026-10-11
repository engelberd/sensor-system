use core::fmt;

/// One signed integer axis in the source stream produced by a node.
///
/// The 24-bit bound matches the compact three-byte wire representation. The
/// host may keep the value in an `i32`; no precision or scale conversion is
/// performed. A native sensor code can use a narrower device-specific type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SampleAxis(i32);

impl SampleAxis {
    pub const BITS: u8 = 24;
    pub const MIN: i32 = -8_388_608;
    pub const MAX: i32 = 8_388_607;

    pub const fn new(value: i32) -> Result<Self, SampleAxisError> {
        if value < Self::MIN || value > Self::MAX {
            Err(SampleAxisError { value })
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> i32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SampleAxisError {
    value: i32,
}

impl fmt::Display for SampleAxisError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "sample axis value {} is outside signed 24-bit range",
            self.value
        )
    }
}

impl std::error::Error for SampleAxisError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SampleAxes {
    pub x: SampleAxis,
    pub y: SampleAxis,
    pub z: SampleAxis,
}

impl SampleAxes {
    pub const fn new(x: SampleAxis, y: SampleAxis, z: SampleAxis) -> Self {
        Self { x, y, z }
    }
}

#[cfg(test)]
mod tests {
    use super::SampleAxis;

    #[test]
    fn accepts_signed_24_bit_stream_boundaries() {
        assert_eq!(
            SampleAxis::new(SampleAxis::MIN).unwrap().get(),
            SampleAxis::MIN
        );
        assert_eq!(
            SampleAxis::new(SampleAxis::MAX).unwrap().get(),
            SampleAxis::MAX
        );
    }

    #[test]
    fn rejects_values_that_cannot_be_encoded_on_the_wire() {
        assert!(SampleAxis::new(SampleAxis::MIN - 1).is_err());
        assert!(SampleAxis::new(SampleAxis::MAX + 1).is_err());
    }
}
