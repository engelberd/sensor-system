//! Individual monotonic device timestamps, independent of wire/storage encoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SampleTimes(Vec<u64>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SampleTimesError {
    Empty,
    TooMany,
    NotIncreasing,
}

impl SampleTimes {
    // Bound independent of any particular transport's packet size.
    pub const MAX_SAMPLES: usize = 64;
    pub fn new(times: Vec<u64>) -> Result<Self, SampleTimesError> {
        if times.is_empty() {
            return Err(SampleTimesError::Empty);
        }
        if times.len() > Self::MAX_SAMPLES {
            return Err(SampleTimesError::TooMany);
        }
        if times.windows(2).any(|pair| pair[1] <= pair[0]) {
            return Err(SampleTimesError::NotIncreasing);
        }
        Ok(Self(times))
    }
    pub fn as_slice(&self) -> &[u64] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_irregular_and_slow_times() {
        let times = vec![100, 65635, 131171, 259172];
        assert_eq!(SampleTimes::new(times.clone()).unwrap().as_slice(), times);
        assert_eq!(
            SampleTimes::new(vec![1, 1]),
            Err(SampleTimesError::NotIncreasing)
        );
        assert_eq!(SampleTimes::new(vec![]), Err(SampleTimesError::Empty));
    }
}
