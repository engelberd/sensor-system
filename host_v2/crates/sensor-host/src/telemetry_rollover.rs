use core::fmt;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};

use crate::telemetry_repository::{
    TelemetryStreamKey, TelemetryStreamRollover, TelemetryStreamSpec,
};
use crate::telemetry_runtime::TelemetryRuntimeManagementError;

#[derive(Clone)]
pub struct TelemetryRolloverClient {
    requests: SyncSender<TelemetryRolloverRequest>,
}
pub struct TelemetryRolloverServer {
    requests: Receiver<TelemetryRolloverRequest>,
}
pub struct TelemetryRolloverTicket {
    result: Receiver<Result<TelemetryStreamRollover, TelemetryRuntimeManagementError>>,
}
pub struct TelemetryRolloverRequest {
    previous: TelemetryStreamKey,
    next: TelemetryStreamSpec,
    result: SyncSender<Result<TelemetryStreamRollover, TelemetryRuntimeManagementError>>,
}

pub fn telemetry_rollover_mailbox(
    capacity: usize,
) -> Result<(TelemetryRolloverClient, TelemetryRolloverServer), TelemetryRolloverMailboxError> {
    if capacity == 0 {
        return Err(TelemetryRolloverMailboxError::ZeroCapacity);
    }
    let (requests, receiver) = mpsc::sync_channel(capacity);
    Ok((
        TelemetryRolloverClient { requests },
        TelemetryRolloverServer { requests: receiver },
    ))
}

impl TelemetryRolloverClient {
    pub fn try_request(
        &self,
        previous: TelemetryStreamKey,
        next: TelemetryStreamSpec,
    ) -> Result<TelemetryRolloverTicket, TelemetryRolloverSubmitError> {
        let (result, receiver) = mpsc::sync_channel(1);
        self.requests
            .try_send(TelemetryRolloverRequest {
                previous,
                next,
                result,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => TelemetryRolloverSubmitError::QueueFull,
                TrySendError::Disconnected(_) => TelemetryRolloverSubmitError::Disconnected,
            })?;
        Ok(TelemetryRolloverTicket { result: receiver })
    }
}

impl TelemetryRolloverServer {
    pub fn try_receive(
        &self,
    ) -> Result<Option<TelemetryRolloverRequest>, TelemetryRolloverReceiveError> {
        match self.requests.try_recv() {
            Ok(request) => Ok(Some(request)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(TelemetryRolloverReceiveError::Disconnected),
        }
    }
}

impl TelemetryRolloverRequest {
    pub fn into_parts(
        self,
    ) -> (
        TelemetryStreamKey,
        TelemetryStreamSpec,
        TelemetryRolloverCompletion,
    ) {
        (
            self.previous,
            self.next,
            TelemetryRolloverCompletion {
                result: self.result,
            },
        )
    }
}

pub struct TelemetryRolloverCompletion {
    result: SyncSender<Result<TelemetryStreamRollover, TelemetryRuntimeManagementError>>,
}
impl TelemetryRolloverCompletion {
    pub fn complete(
        self,
        result: Result<TelemetryStreamRollover, TelemetryRuntimeManagementError>,
    ) -> Result<(), TelemetryRolloverCompletionError> {
        self.result
            .send(result)
            .map_err(|_| TelemetryRolloverCompletionError::RequesterDisconnected)
    }
}

impl TelemetryRolloverTicket {
    pub fn try_result(
        &self,
    ) -> Result<
        Option<Result<TelemetryStreamRollover, TelemetryRuntimeManagementError>>,
        TelemetryRolloverReceiveError,
    > {
        match self.result.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(TelemetryRolloverReceiveError::Disconnected),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryRolloverMailboxError {
    ZeroCapacity,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryRolloverSubmitError {
    QueueFull,
    Disconnected,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryRolloverReceiveError {
    Disconnected,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryRolloverCompletionError {
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
display_debug!(
    TelemetryRolloverMailboxError,
    "telemetry rollover mailbox error"
);
display_debug!(
    TelemetryRolloverSubmitError,
    "telemetry rollover submit error"
);
display_debug!(
    TelemetryRolloverReceiveError,
    "telemetry rollover receive error"
);
display_debug!(
    TelemetryRolloverCompletionError,
    "telemetry rollover completion error"
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry_wal::TelemetryWalConfig;
    use sensor_core::{BootEpoch, HardwareId};

    fn key(epoch: u64) -> TelemetryStreamKey {
        TelemetryStreamKey {
            hardware_id: HardwareId::from_bytes([1; 8]),
            node_address: 1,
            boot_epoch: BootEpoch::new(epoch),
        }
    }

    #[test]
    fn bounded_mailbox_preserves_the_epoch_request() {
        let (client, server) = telemetry_rollover_mailbox(1).unwrap();
        let ticket = client
            .try_request(
                key(7),
                TelemetryStreamSpec {
                    path: "telemetry-next.wal".into(),
                    config: TelemetryWalConfig {
                        hardware_id: key(8).hardware_id,
                        node_address: 1,
                        boot_epoch: BootEpoch::new(8),
                    },
                },
            )
            .unwrap();
        let request = server.try_receive().unwrap().unwrap();
        let (_, _, completion) = request.into_parts();
        completion
            .complete(Ok(TelemetryStreamRollover {
                stream: key(8),
                durable_record_count: 0,
            }))
            .unwrap();
        assert_eq!(
            ticket.try_result().unwrap().unwrap().unwrap().stream,
            key(8)
        );
    }
}
