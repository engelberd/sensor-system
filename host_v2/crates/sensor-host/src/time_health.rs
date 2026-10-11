use core::fmt;

use crate::time_persistence::{
    TimeObservationRecord, TimePersistenceCompletion, TimePersistenceRecoveryDisposition,
};
use crate::time_repository::TimeStreamKey;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimePersistenceHealthState {
    Healthy,
    RecoveryRequired,
    Recovering,
    IntegrityFault,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeStreamHealth {
    pub stream: TimeStreamKey,
    pub durable_record_count: u64,
}

/// Coordinator-side projection of Time persistence completions.
///
/// It performs no I/O and owns no retry policy. Its only job is to make a
/// storage fault and its exact uncertain record impossible to hide in logs.
pub struct TimePersistenceHealth {
    streams: Vec<TimeStreamHealth>,
    state: TimePersistenceHealthState,
    uncertain: Option<TimeObservationRecord>,
    integrity_error: Option<TimeHealthIntegrityError>,
}

impl TimePersistenceHealth {
    pub fn from_recovery(
        recovered: impl IntoIterator<Item = (TimeStreamKey, u64)>,
    ) -> Result<Self, TimeHealthIntegrityError> {
        let mut streams = Vec::new();
        for (stream, durable_record_count) in recovered {
            if streams
                .iter()
                .any(|existing: &TimeStreamHealth| existing.stream == stream)
            {
                return Err(TimeHealthIntegrityError::DuplicateStream(stream));
            }
            streams.push(TimeStreamHealth {
                stream,
                durable_record_count,
            });
        }
        if streams.is_empty() {
            return Err(TimeHealthIntegrityError::NoStreams);
        }
        Ok(Self {
            streams,
            state: TimePersistenceHealthState::Healthy,
            uncertain: None,
            integrity_error: None,
        })
    }

    pub const fn state(&self) -> TimePersistenceHealthState {
        self.state
    }

    pub const fn uncertain_record(&self) -> Option<TimeObservationRecord> {
        self.uncertain
    }

    pub const fn integrity_error(&self) -> Option<TimeHealthIntegrityError> {
        self.integrity_error
    }

    pub fn streams(&self) -> &[TimeStreamHealth] {
        &self.streams
    }

    pub fn observe_completion(
        &mut self,
        completion: TimePersistenceCompletion,
    ) -> Result<(), TimeHealthIntegrityError> {
        if self.state == TimePersistenceHealthState::IntegrityFault {
            return Err(self
                .integrity_error
                .unwrap_or(TimeHealthIntegrityError::AlreadyFaulted));
        }
        let record = completion.record;
        let stream = TimeStreamKey::from_record(record);
        let index = match self.streams.iter().position(|item| item.stream == stream) {
            Some(index) => index,
            None => return self.fail(TimeHealthIntegrityError::UnknownStream(stream)),
        };

        match completion.result {
            Err(_) => {
                if self.state != TimePersistenceHealthState::Healthy {
                    return self.fail(TimeHealthIntegrityError::UnexpectedFailure);
                }
                self.uncertain = Some(record);
                self.state = TimePersistenceHealthState::RecoveryRequired;
                Ok(())
            }
            Ok(receipt) => {
                if receipt.stream != stream {
                    return self.fail(TimeHealthIntegrityError::ReceiptStreamMismatch);
                }
                if let Some(uncertain) = self.uncertain {
                    if uncertain != record || self.state != TimePersistenceHealthState::Recovering {
                        return self.fail(TimeHealthIntegrityError::UnexpectedRecoveryCompletion);
                    }
                } else if self.state != TimePersistenceHealthState::Healthy {
                    return self.fail(TimeHealthIntegrityError::UnexpectedSuccess);
                }
                let expected = match self.streams[index].durable_record_count.checked_add(1) {
                    Some(value) => value,
                    None => return self.fail(TimeHealthIntegrityError::RecordCountOverflow),
                };
                if receipt.record_count != expected {
                    return self.fail(TimeHealthIntegrityError::NonSequentialRecordCount {
                        expected,
                        received: receipt.record_count,
                    });
                }
                self.streams[index].durable_record_count = receipt.record_count;
                self.uncertain = None;
                self.state = TimePersistenceHealthState::Healthy;
                Ok(())
            }
        }
    }

    pub fn observe_recovery(
        &mut self,
        _disposition: TimePersistenceRecoveryDisposition,
    ) -> Result<(), TimeHealthIntegrityError> {
        if self.state != TimePersistenceHealthState::RecoveryRequired || self.uncertain.is_none() {
            return self.fail(TimeHealthIntegrityError::UnexpectedRecovery);
        }
        self.state = TimePersistenceHealthState::Recovering;
        Ok(())
    }

    fn fail<T>(&mut self, error: TimeHealthIntegrityError) -> Result<T, TimeHealthIntegrityError> {
        self.state = TimePersistenceHealthState::IntegrityFault;
        self.integrity_error = Some(error);
        Err(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeHealthIntegrityError {
    NoStreams,
    DuplicateStream(TimeStreamKey),
    UnknownStream(TimeStreamKey),
    ReceiptStreamMismatch,
    UnexpectedFailure,
    UnexpectedSuccess,
    UnexpectedRecovery,
    UnexpectedRecoveryCompletion,
    RecordCountOverflow,
    NonSequentialRecordCount { expected: u64, received: u64 },
    AlreadyFaulted,
}

impl fmt::Display for TimeHealthIntegrityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "time persistence health integrity error: {self:?}"
        )
    }
}

impl std::error::Error for TimeHealthIntegrityError {}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_core::{BootEpoch, ClockObservation, HardwareId};

    use crate::time_persistence::TimeObservationRecord;
    use crate::time_repository::{TimeDurableReceipt, TimeRepositoryError};
    use crate::time_wal::TimeWalError;

    fn record(index: u64) -> TimeObservationRecord {
        TimeObservationRecord::new(
            HardwareId::from_bytes([1; 8]),
            1,
            ClockObservation::new(
                BootEpoch::new(7),
                10 + index * 100,
                20 + index * 100,
                30 + index * 100,
                40 + index * 100,
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn health() -> TimePersistenceHealth {
        TimePersistenceHealth::from_recovery([(TimeStreamKey::from_record(record(0)), 4)]).unwrap()
    }

    #[test]
    fn ordinary_completion_advances_only_the_matching_stream() {
        let mut health = health();
        let value = record(0);
        health
            .observe_completion(TimePersistenceCompletion {
                record: value,
                result: Ok(TimeDurableReceipt {
                    stream: TimeStreamKey::from_record(value),
                    record_count: 5,
                }),
            })
            .unwrap();
        assert_eq!(health.state(), TimePersistenceHealthState::Healthy);
        assert_eq!(health.streams()[0].durable_record_count, 5);
    }

    #[test]
    fn storage_failure_stays_visible_until_recovery_and_final_durable_receipt() {
        let mut health = health();
        let value = record(0);
        health
            .observe_completion(TimePersistenceCompletion {
                record: value,
                result: Err(TimeRepositoryError::Wal(TimeWalError::Poisoned)),
            })
            .unwrap();
        assert_eq!(health.state(), TimePersistenceHealthState::RecoveryRequired);
        assert_eq!(health.uncertain_record(), Some(value));

        health
            .observe_recovery(TimePersistenceRecoveryDisposition::AlreadyDurable)
            .unwrap();
        assert_eq!(health.state(), TimePersistenceHealthState::Recovering);
        health
            .observe_completion(TimePersistenceCompletion {
                record: value,
                result: Ok(TimeDurableReceipt {
                    stream: TimeStreamKey::from_record(value),
                    record_count: 5,
                }),
            })
            .unwrap();
        assert_eq!(health.state(), TimePersistenceHealthState::Healthy);
        assert_eq!(health.uncertain_record(), None);
    }

    #[test]
    fn impossible_receipt_faults_health_instead_of_hiding_it() {
        let mut health = health();
        let value = record(0);
        assert!(matches!(
            health.observe_completion(TimePersistenceCompletion {
                record: value,
                result: Ok(TimeDurableReceipt {
                    stream: TimeStreamKey::from_record(value),
                    record_count: 9,
                }),
            }),
            Err(TimeHealthIntegrityError::NonSequentialRecordCount { .. })
        ));
        assert_eq!(health.state(), TimePersistenceHealthState::IntegrityFault);
    }
}
