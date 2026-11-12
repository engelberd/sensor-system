use core::fmt;

use sensor_protocol::{
    decode_lifecycle_response, encode_lifecycle_request, EncodedRuntimeCommand, LifecycleWireError,
    ResponseStatus, RuntimeCommand, RuntimeResult, RuntimeResultCode, LIFECYCLE_SERVICE_ID,
};

use crate::transaction::{DurationMicros, RequestSpec, TimeoutRecovery};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecyclePhase {
    ReadyToSend,
    AwaitingResponse,
    ReconcileRequired,
    Complete,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleOutcome {
    NotAccepted,
    InProgress(RuntimeResult),
    Complete(RuntimeResult),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LifecycleAttempt {
    pub request: RequestSpec,
    pub encoded: EncodedRuntimeCommand,
}

impl LifecycleAttempt {
    pub fn payload(&self) -> &[u8] {
        &self.encoded.payload[..self.encoded.payload_size]
    }
}

/// One logical lifecycle operation across any number of wire transactions.
/// A retry receives a fresh transaction ID from `TransactionCore`, while this
/// object deliberately preserves the same operation ID and command payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LifecycleOperation {
    node_address: u8,
    command: RuntimeCommand,
    phase: LifecyclePhase,
    terminal_result: Option<RuntimeResult>,
}

impl LifecycleOperation {
    pub fn new(node_address: u8, command: RuntimeCommand) -> Result<Self, LifecycleError> {
        // Validate the complete wire command once. Every later attempt reuses it.
        encode_lifecycle_request(command)?;
        Ok(Self {
            node_address,
            command,
            phase: LifecyclePhase::ReadyToSend,
            terminal_result: None,
        })
    }

    pub const fn phase(self) -> LifecyclePhase {
        self.phase
    }

    pub const fn command(self) -> RuntimeCommand {
        self.command
    }

    pub const fn node_address(self) -> u8 {
        self.node_address
    }

    pub const fn terminal_result(self) -> Option<RuntimeResult> {
        self.terminal_result
    }

    pub fn begin_attempt(
        &mut self,
        timeout: DurationMicros,
    ) -> Result<LifecycleAttempt, LifecycleError> {
        if self.phase != LifecyclePhase::ReadyToSend
            && self.phase != LifecyclePhase::ReconcileRequired
        {
            return Err(LifecycleError::UnexpectedPhase);
        }
        let encoded = encode_lifecycle_request(self.command)?;
        self.phase = LifecyclePhase::AwaitingResponse;
        Ok(LifecycleAttempt {
            request: RequestSpec {
                node_address: self.node_address,
                service: LIFECYCLE_SERVICE_ID,
                request_message: encoded.message,
                response_message: encoded.message,
                timeout,
                timeout_recovery: TimeoutRecovery::ReconcileState,
            },
            encoded,
        })
    }

    pub fn mark_timeout(&mut self) -> Result<(), LifecycleError> {
        if self.phase != LifecyclePhase::AwaitingResponse {
            return Err(LifecycleError::UnexpectedPhase);
        }
        self.phase = LifecyclePhase::ReconcileRequired;
        Ok(())
    }

    pub fn accept_response(&mut self, payload: &[u8]) -> Result<LifecycleOutcome, LifecycleError> {
        if self.phase != LifecyclePhase::AwaitingResponse {
            return Err(LifecycleError::UnexpectedPhase);
        }
        let result = match decode_lifecycle_response(payload) {
            Ok(result) => result,
            Err(LifecycleWireError::RemoteError(ResponseStatus::Busy)) => {
                self.phase = LifecyclePhase::ReadyToSend;
                return Ok(LifecycleOutcome::NotAccepted);
            }
            Err(error) => {
                self.phase = LifecyclePhase::Failed;
                return Err(error.into());
            }
        };
        if result.operation_id != self.command.operation_id {
            self.phase = LifecyclePhase::Failed;
            return Err(LifecycleError::OperationIdMismatch {
                expected: self.command.operation_id,
                received: result.operation_id,
            });
        }
        if result.code == RuntimeResultCode::InProgress {
            self.phase = LifecyclePhase::ReconcileRequired;
            return Ok(LifecycleOutcome::InProgress(result));
        }
        self.phase = LifecyclePhase::Complete;
        self.terminal_result = Some(result);
        Ok(LifecycleOutcome::Complete(result))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleError {
    UnexpectedPhase,
    Wire(LifecycleWireError),
    OperationIdMismatch { expected: u32, received: u32 },
}

impl From<LifecycleWireError> for LifecycleError {
    fn from(value: LifecycleWireError) -> Self {
        Self::Wire(value)
    }
}

impl fmt::Display for LifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedPhase => formatter.write_str("lifecycle operation used out of phase"),
            Self::Wire(error) => write!(formatter, "invalid lifecycle payload: {error}"),
            Self::OperationIdMismatch { expected, received } => write!(
                formatter,
                "lifecycle response operation ID {received} does not match {expected}"
            ),
        }
    }
}

impl std::error::Error for LifecycleError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::{MonotonicMicros, ResponseDisposition, TimerEvent, TransactionCore};
    use sensor_protocol::{
        encode_lifecycle_response, Frame, FrameRole, ObservabilityProfile, RuntimeOperation,
        RuntimeState, SensorStatusCode, HOST_ADDRESS,
    };

    fn duration(value: u64) -> DurationMicros {
        DurationMicros::new(value).unwrap()
    }

    fn response_frame(
        active: crate::transaction::ActiveTransaction,
        result: RuntimeResult,
    ) -> Frame {
        Frame::new(
            FrameRole::Response,
            HOST_ADDRESS,
            active.node_address,
            active.service,
            active.response_message,
            active.transaction_id,
            &encode_lifecycle_response(result).unwrap(),
        )
        .unwrap()
    }

    fn start_command() -> RuntimeCommand {
        RuntimeCommand {
            operation_id: 77,
            operation: RuntimeOperation::Start,
            requested_profile: ObservabilityProfile::Production,
        }
    }

    #[test]
    fn timeout_and_in_progress_preserve_operation_id_across_wire_attempts() {
        let mut operation = LifecycleOperation::new(7, start_command()).unwrap();
        let mut transactions = TransactionCore::new(10, duration(50));

        let first = operation.begin_attempt(duration(100)).unwrap();
        let first_tx = transactions
            .begin(MonotonicMicros::new(0), first.request)
            .unwrap();
        assert_eq!(u32::from_le_bytes(first.payload().try_into().unwrap()), 77);
        assert_eq!(
            transactions.on_timer(MonotonicMicros::new(101)).unwrap(),
            TimerEvent::TimedOut {
                transaction_id: first_tx.transaction_id,
                recovery: TimeoutRecovery::ReconcileState,
                quarantine_until: MonotonicMicros::new(150),
            }
        );
        operation.mark_timeout().unwrap();
        assert_eq!(
            transactions.on_timer(MonotonicMicros::new(150)).unwrap(),
            TimerEvent::QuarantineEnded
        );

        let second = operation.begin_attempt(duration(100)).unwrap();
        let second_tx = transactions
            .begin(MonotonicMicros::new(150), second.request)
            .unwrap();
        assert_ne!(first_tx.transaction_id, second_tx.transaction_id);
        assert_eq!(first.payload(), second.payload());
        let in_progress = response_frame(
            second_tx,
            RuntimeResult {
                operation_id: 77,
                code: RuntimeResultCode::InProgress,
                state: RuntimeState::Recording,
                observability: ObservabilityProfile::Production,
                sensor_status: SensorStatusCode::Ok,
            },
        );
        assert_eq!(
            transactions
                .receive(MonotonicMicros::new(151), &in_progress)
                .unwrap(),
            ResponseDisposition::Accepted
        );
        assert!(matches!(
            operation.accept_response(in_progress.payload()).unwrap(),
            LifecycleOutcome::InProgress(_)
        ));

        let third = operation.begin_attempt(duration(100)).unwrap();
        let third_tx = transactions
            .begin(MonotonicMicros::new(152), third.request)
            .unwrap();
        let complete = response_frame(
            third_tx,
            RuntimeResult {
                operation_id: 77,
                code: RuntimeResultCode::Ok,
                state: RuntimeState::Recording,
                observability: ObservabilityProfile::Production,
                sensor_status: SensorStatusCode::Ok,
            },
        );
        assert_eq!(
            transactions
                .receive(MonotonicMicros::new(153), &complete)
                .unwrap(),
            ResponseDisposition::Accepted
        );
        assert!(matches!(
            operation.accept_response(complete.payload()).unwrap(),
            LifecycleOutcome::Complete(_)
        ));
        assert_eq!(operation.phase(), LifecyclePhase::Complete);
    }

    #[test]
    fn remote_busy_means_the_operation_was_not_accepted() {
        let mut operation = LifecycleOperation::new(7, start_command()).unwrap();
        operation.begin_attempt(duration(100)).unwrap();
        assert_eq!(
            operation.accept_response(&[ResponseStatus::Busy as u8]),
            Ok(LifecycleOutcome::NotAccepted)
        );
        assert_eq!(operation.phase(), LifecyclePhase::ReadyToSend);
    }

    #[test]
    fn mismatched_operation_id_fails_closed() {
        let mut operation = LifecycleOperation::new(7, start_command()).unwrap();
        operation.begin_attempt(duration(100)).unwrap();
        let payload = encode_lifecycle_response(RuntimeResult {
            operation_id: 78,
            code: RuntimeResultCode::Ok,
            state: RuntimeState::Recording,
            observability: ObservabilityProfile::Production,
            sensor_status: SensorStatusCode::Ok,
        })
        .unwrap();
        assert_eq!(
            operation.accept_response(&payload),
            Err(LifecycleError::OperationIdMismatch {
                expected: 77,
                received: 78,
            })
        );
        assert_eq!(operation.phase(), LifecyclePhase::Failed);
    }
}
