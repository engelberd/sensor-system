use core::fmt;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};

use crate::time_repository::{TimeStreamKey, TimeStreamRollover, TimeStreamSpec};
use crate::time_runtime::TimeRuntimeManagementError;

#[derive(Clone)]
pub struct TimeRolloverClient {
    requests: SyncSender<TimeRolloverRequest>,
}

pub struct TimeRolloverServer {
    requests: Receiver<TimeRolloverRequest>,
}

pub struct TimeRolloverTicket {
    result: Receiver<Result<TimeStreamRollover, TimeRuntimeManagementError>>,
}

pub struct TimeRolloverRequest {
    previous: TimeStreamKey,
    next: TimeStreamSpec,
    result: SyncSender<Result<TimeStreamRollover, TimeRuntimeManagementError>>,
}

pub fn time_rollover_mailbox(
    capacity: usize,
) -> Result<(TimeRolloverClient, TimeRolloverServer), TimeRolloverMailboxError> {
    if capacity == 0 {
        return Err(TimeRolloverMailboxError::ZeroCapacity);
    }
    let (requests, receiver) = mpsc::sync_channel(capacity);
    Ok((
        TimeRolloverClient { requests },
        TimeRolloverServer { requests: receiver },
    ))
}

impl TimeRolloverClient {
    pub fn try_request(
        &self,
        previous: TimeStreamKey,
        next: TimeStreamSpec,
    ) -> Result<TimeRolloverTicket, TimeRolloverSubmitError> {
        let (result, receiver) = mpsc::sync_channel(1);
        let request = TimeRolloverRequest {
            previous,
            next,
            result,
        };
        self.requests
            .try_send(request)
            .map_err(|error| match error {
                TrySendError::Full(_) => TimeRolloverSubmitError::QueueFull,
                TrySendError::Disconnected(_) => TimeRolloverSubmitError::Disconnected,
            })?;
        Ok(TimeRolloverTicket { result: receiver })
    }
}

impl TimeRolloverServer {
    pub fn try_receive(&self) -> Result<Option<TimeRolloverRequest>, TimeRolloverReceiveError> {
        match self.requests.try_recv() {
            Ok(request) => Ok(Some(request)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(TimeRolloverReceiveError::Disconnected),
        }
    }
}

impl TimeRolloverRequest {
    pub const fn previous(&self) -> TimeStreamKey {
        self.previous
    }

    pub const fn next(&self) -> &TimeStreamSpec {
        &self.next
    }

    pub fn into_parts(self) -> (TimeStreamKey, TimeStreamSpec, TimeRolloverCompletion) {
        (
            self.previous,
            self.next,
            TimeRolloverCompletion {
                result: self.result,
            },
        )
    }
}

pub struct TimeRolloverCompletion {
    result: SyncSender<Result<TimeStreamRollover, TimeRuntimeManagementError>>,
}

impl TimeRolloverCompletion {
    pub fn complete(
        self,
        result: Result<TimeStreamRollover, TimeRuntimeManagementError>,
    ) -> Result<(), TimeRolloverCompletionError> {
        self.result
            .send(result)
            .map_err(|_| TimeRolloverCompletionError::RequesterDisconnected)
    }
}

impl TimeRolloverTicket {
    pub fn try_result(
        &self,
    ) -> Result<
        Option<Result<TimeStreamRollover, TimeRuntimeManagementError>>,
        TimeRolloverReceiveError,
    > {
        match self.result.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(TimeRolloverReceiveError::Disconnected),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeRolloverMailboxError {
    ZeroCapacity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeRolloverSubmitError {
    QueueFull,
    Disconnected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeRolloverReceiveError {
    Disconnected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeRolloverCompletionError {
    RequesterDisconnected,
}

macro_rules! display_debug {
    ($type:ty, $label:literal) => {
        impl fmt::Display for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, concat!($label, ": {:?}"), self)
            }
        }
        impl std::error::Error for $type {}
    };
}

display_debug!(TimeRolloverMailboxError, "time rollover mailbox error");
display_debug!(TimeRolloverSubmitError, "time rollover submit error");
display_debug!(TimeRolloverReceiveError, "time rollover receive error");
display_debug!(
    TimeRolloverCompletionError,
    "time rollover completion error"
);

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_core::{BootEpoch, HardwareId};

    use crate::time_wal::{HostClockId, TimeWalConfig};

    fn key(epoch: u64) -> TimeStreamKey {
        TimeStreamKey {
            hardware_id: HardwareId::from_bytes([1; 8]),
            node_address: 1,
            boot_epoch: BootEpoch::new(epoch),
        }
    }

    fn spec(epoch: u64) -> TimeStreamSpec {
        TimeStreamSpec {
            path: format!("time-{epoch}.wal").into(),
            config: TimeWalConfig {
                hardware_id: key(epoch).hardware_id,
                node_address: 1,
                boot_epoch: BootEpoch::new(epoch),
                host_clock_id: HostClockId::from_bytes([9; 16]),
            },
        }
    }

    #[test]
    fn bounded_mailbox_preserves_request_and_one_shot_result() {
        let (client, server) = time_rollover_mailbox(1).unwrap();
        let ticket = client.try_request(key(7), spec(8)).unwrap();
        assert!(ticket.try_result().unwrap().is_none());
        assert!(matches!(
            client.try_request(key(8), spec(9)),
            Err(TimeRolloverSubmitError::QueueFull)
        ));

        let request = server.try_receive().unwrap().unwrap();
        assert_eq!(request.previous(), key(7));
        assert_eq!(request.next().config.boot_epoch, BootEpoch::new(8));
        let (_, _, completion) = request.into_parts();
        completion
            .complete(Ok(TimeStreamRollover {
                stream: key(8),
                durable_record_count: 0,
            }))
            .unwrap();
        assert_eq!(
            ticket.try_result().unwrap().unwrap().unwrap(),
            TimeStreamRollover {
                stream: key(8),
                durable_record_count: 0
            }
        );
    }

    #[test]
    fn disconnected_ends_both_sides_without_blocking() {
        let (client, server) = time_rollover_mailbox(1).unwrap();
        drop(server);
        assert!(matches!(
            client.try_request(key(7), spec(8)),
            Err(TimeRolloverSubmitError::Disconnected)
        ));

        let (client, server) = time_rollover_mailbox(1).unwrap();
        drop(client);
        assert!(matches!(
            server.try_receive(),
            Err(TimeRolloverReceiveError::Disconnected)
        ));
    }
}
