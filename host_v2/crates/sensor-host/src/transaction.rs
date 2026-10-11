use core::fmt;

use sensor_protocol::{Frame, FrameRole, BROADCAST_ADDRESS, HOST_ADDRESS};

/// Monotonic time supplied by the runtime clock.
///
/// The transaction state machine deliberately does not read the system clock,
/// which keeps timeout and late-response behavior deterministic in tests.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonotonicMicros(u64);

impl MonotonicMicros {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub(crate) fn checked_add(self, duration: DurationMicros) -> Option<Self> {
        self.0.checked_add(duration.0).map(Self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DurationMicros(u64);

impl DurationMicros {
    pub const fn new(value: u64) -> Result<Self, TransactionError> {
        if value == 0 {
            Err(TransactionError::ZeroDuration)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Meaning of a missing response for the application service.
///
/// This class does not decide how many retries are allowed. `NodeSession` and
/// the channel scheduler own that bounded policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeoutRecovery {
    /// Repeating the logical request is safe. A new wire attempt still receives
    /// a fresh transaction ID.
    RetrySafe,
    /// The request may already have changed node state. The service must read
    /// and reconcile authoritative state before deciding what to do next.
    ReconcileState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestSpec {
    pub node_address: u8,
    pub service: u8,
    pub request_message: u8,
    pub response_message: u8,
    pub timeout: DurationMicros,
    pub timeout_recovery: TimeoutRecovery,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActiveTransaction {
    pub transaction_id: u32,
    pub node_address: u8,
    pub service: u8,
    pub request_message: u8,
    pub response_message: u8,
    pub deadline: MonotonicMicros,
    pub timeout_recovery: TimeoutRecovery,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedTransaction {
    pub transaction_id: u32,
    pub node_address: u8,
    pub service: u8,
    pub request_message: u8,
    pub response_message: u8,
    pub timeout: DurationMicros,
    pub timeout_recovery: TimeoutRecovery,
}

impl PreparedTransaction {
    pub fn request_frame(self, payload: &[u8]) -> Result<Frame, sensor_protocol::FrameError> {
        Frame::new(
            FrameRole::Request,
            self.node_address,
            HOST_ADDRESS,
            self.service,
            self.request_message,
            self.transaction_id,
            payload,
        )
    }
}

impl ActiveTransaction {
    pub fn request_frame(self, payload: &[u8]) -> Result<Frame, sensor_protocol::FrameError> {
        Frame::new(
            FrameRole::Request,
            self.node_address,
            HOST_ADDRESS,
            self.service,
            self.request_message,
            self.transaction_id,
            payload,
        )
    }

    fn matches(self, frame: &Frame) -> bool {
        frame.role == FrameRole::Response
            && frame.destination == HOST_ADDRESS
            && frame.source == self.node_address
            && frame.service == self.service
            && frame.message == self.response_message
            && frame.transaction_id == self.transaction_id
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseDisposition {
    Accepted,
    /// A valid response did not belong to the currently active transaction.
    /// It is observable diagnostics and never completes the request.
    Unrelated,
    /// The response matched the expired request but arrived after its deadline.
    /// It is discarded even if the service payload would otherwise be valid.
    Late,
    /// There is no active request. Responses are never promoted to unsolicited
    /// events by the transaction layer.
    Unsolicited,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerEvent {
    None,
    TimedOut {
        transaction_id: u32,
        recovery: TimeoutRecovery,
        quarantine_until: MonotonicMicros,
    },
    QuarantineEnded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Idle,
    Transmitting(PreparedTransaction),
    Awaiting(ActiveTransaction),
    Quarantine {
        expired: ActiveTransaction,
        until: MonotonicMicros,
    },
}

/// Correlates one request/response exchange on a half-duplex channel.
///
/// Only this state machine may declare a wire attempt complete. Retry budgets,
/// durable sample cursors and service-specific reconciliation live above it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransactionCore {
    next_transaction_id: u32,
    quarantine_duration: DurationMicros,
    state: State,
}

impl TransactionCore {
    pub const fn new(seed: u32, quarantine_duration: DurationMicros) -> Self {
        Self {
            next_transaction_id: seed,
            quarantine_duration,
            state: State::Idle,
        }
    }

    pub const fn is_idle(self) -> bool {
        matches!(self.state, State::Idle)
    }

    pub fn begin(
        &mut self,
        now: MonotonicMicros,
        request: RequestSpec,
    ) -> Result<ActiveTransaction, TransactionError> {
        self.prepare(request)?;
        match self.mark_transmitted(now) {
            Ok(active) => Ok(active),
            Err(error) => {
                self.state = State::Idle;
                Err(error)
            }
        }
    }

    pub fn prepare(
        &mut self,
        request: RequestSpec,
    ) -> Result<PreparedTransaction, TransactionError> {
        if !matches!(self.state, State::Idle) {
            return Err(TransactionError::ChannelBusy);
        }
        if request.node_address == HOST_ADDRESS || request.node_address == BROADCAST_ADDRESS {
            return Err(TransactionError::RequestRequiresAddressedNode);
        }
        let transaction = PreparedTransaction {
            transaction_id: self.next_transaction_id,
            node_address: request.node_address,
            service: request.service,
            request_message: request.request_message,
            response_message: request.response_message,
            timeout: request.timeout,
            timeout_recovery: request.timeout_recovery,
        };
        self.next_transaction_id = self.next_transaction_id.wrapping_add(1);
        self.state = State::Transmitting(transaction);
        Ok(transaction)
    }

    pub fn mark_transmitted(
        &mut self,
        now: MonotonicMicros,
    ) -> Result<ActiveTransaction, TransactionError> {
        let State::Transmitting(prepared) = self.state else {
            return Err(TransactionError::NotTransmitting);
        };
        let deadline = now
            .checked_add(prepared.timeout)
            .ok_or(TransactionError::TimeOverflow)?;
        let active = ActiveTransaction {
            transaction_id: prepared.transaction_id,
            node_address: prepared.node_address,
            service: prepared.service,
            request_message: prepared.request_message,
            response_message: prepared.response_message,
            deadline,
            timeout_recovery: prepared.timeout_recovery,
        };
        self.state = State::Awaiting(active);
        Ok(active)
    }

    pub fn cancel_transmit(&mut self) -> Result<(), TransactionError> {
        if !matches!(self.state, State::Transmitting(_)) {
            return Err(TransactionError::NotTransmitting);
        }
        self.state = State::Idle;
        Ok(())
    }

    pub fn receive(
        &mut self,
        now: MonotonicMicros,
        frame: &Frame,
    ) -> Result<ResponseDisposition, TransactionError> {
        match self.state {
            State::Idle => Ok(ResponseDisposition::Unsolicited),
            State::Transmitting(_) => Ok(ResponseDisposition::Unrelated),
            State::Awaiting(active) if now <= active.deadline => {
                if active.matches(frame) {
                    self.state = State::Idle;
                    Ok(ResponseDisposition::Accepted)
                } else {
                    Ok(ResponseDisposition::Unrelated)
                }
            }
            State::Awaiting(active) => {
                self.enter_quarantine(active)?;
                if active.matches(frame) {
                    Ok(ResponseDisposition::Late)
                } else {
                    Ok(ResponseDisposition::Unrelated)
                }
            }
            State::Quarantine { expired, .. } => {
                if expired.matches(frame) {
                    Ok(ResponseDisposition::Late)
                } else {
                    Ok(ResponseDisposition::Unrelated)
                }
            }
        }
    }

    pub fn on_timer(&mut self, now: MonotonicMicros) -> Result<TimerEvent, TransactionError> {
        match self.state {
            State::Idle => Ok(TimerEvent::None),
            State::Transmitting(_) => Ok(TimerEvent::None),
            State::Awaiting(active) if now <= active.deadline => Ok(TimerEvent::None),
            State::Awaiting(active) => {
                let quarantine_until = self.enter_quarantine(active)?;
                Ok(TimerEvent::TimedOut {
                    transaction_id: active.transaction_id,
                    recovery: active.timeout_recovery,
                    quarantine_until,
                })
            }
            State::Quarantine { until, .. } if now < until => Ok(TimerEvent::None),
            State::Quarantine { .. } => {
                self.state = State::Idle;
                Ok(TimerEvent::QuarantineEnded)
            }
        }
    }

    fn enter_quarantine(
        &mut self,
        expired: ActiveTransaction,
    ) -> Result<MonotonicMicros, TransactionError> {
        let until = expired
            .deadline
            .checked_add(self.quarantine_duration)
            .ok_or(TransactionError::TimeOverflow)?;
        self.state = State::Quarantine { expired, until };
        Ok(until)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionError {
    ZeroDuration,
    ChannelBusy,
    RequestRequiresAddressedNode,
    NotTransmitting,
    TimeOverflow,
}

impl fmt::Display for TransactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ZeroDuration => "transaction durations must be greater than zero",
            Self::ChannelBusy => "channel already has an active transaction or quarantine",
            Self::RequestRequiresAddressedNode => {
                "request/response transactions require one addressed node"
            }
            Self::NotTransmitting => "channel has no prepared transmission",
            Self::TimeOverflow => "transaction deadline exceeds monotonic time range",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for TransactionError {}

#[cfg(test)]
mod tests {
    use super::{
        DurationMicros, MonotonicMicros, RequestSpec, ResponseDisposition, TimeoutRecovery,
        TimerEvent, TransactionCore, TransactionError,
    };
    use sensor_protocol::{Frame, FrameRole, HOST_ADDRESS};

    fn duration(value: u64) -> DurationMicros {
        DurationMicros::new(value).unwrap()
    }

    fn request(recovery: TimeoutRecovery) -> RequestSpec {
        RequestSpec {
            node_address: 7,
            service: 2,
            request_message: 3,
            response_message: 4,
            timeout: duration(1_000),
            timeout_recovery: recovery,
        }
    }

    fn response(transaction_id: u32) -> Frame {
        Frame::new(
            FrameRole::Response,
            HOST_ADDRESS,
            7,
            2,
            4,
            transaction_id,
            b"ok",
        )
        .unwrap()
    }

    #[test]
    fn accepts_only_the_exact_response_before_or_at_the_deadline() {
        let mut core = TransactionCore::new(41, duration(50_000));
        let active = core
            .begin(
                MonotonicMicros::new(10),
                request(TimeoutRecovery::RetrySafe),
            )
            .unwrap();

        assert_eq!(
            core.receive(
                MonotonicMicros::new(1_010),
                &response(active.transaction_id)
            ),
            Ok(ResponseDisposition::Accepted)
        );
        assert!(core.is_idle());
    }

    #[test]
    fn response_timeout_starts_only_after_transmission_completes() {
        let mut core = TransactionCore::new(41, duration(50_000));
        let prepared = core.prepare(request(TimeoutRecovery::RetrySafe)).unwrap();

        assert_eq!(
            core.on_timer(MonotonicMicros::new(u64::MAX)).unwrap(),
            TimerEvent::None
        );
        let active = core.mark_transmitted(MonotonicMicros::new(1_000)).unwrap();
        assert_eq!(active.transaction_id, prepared.transaction_id);
        assert_eq!(
            core.on_timer(MonotonicMicros::new(2_001)).unwrap(),
            TimerEvent::TimedOut {
                transaction_id: prepared.transaction_id,
                recovery: TimeoutRecovery::RetrySafe,
                quarantine_until: MonotonicMicros::new(52_000),
            }
        );
    }

    #[test]
    fn unrelated_response_does_not_complete_the_active_transaction() {
        let mut core = TransactionCore::new(100, duration(50_000));
        let active = core
            .begin(MonotonicMicros::new(0), request(TimeoutRecovery::RetrySafe))
            .unwrap();

        assert_eq!(
            core.receive(
                MonotonicMicros::new(100),
                &response(active.transaction_id + 1)
            ),
            Ok(ResponseDisposition::Unrelated)
        );
        assert_eq!(
            core.receive(MonotonicMicros::new(200), &response(active.transaction_id)),
            Ok(ResponseDisposition::Accepted)
        );
    }

    #[test]
    fn timeout_quarantines_the_bus_and_late_response_is_discarded() {
        let mut core = TransactionCore::new(5, duration(50_000));
        let active = core
            .begin(
                MonotonicMicros::new(100),
                request(TimeoutRecovery::RetrySafe),
            )
            .unwrap();

        assert_eq!(
            core.on_timer(MonotonicMicros::new(1_101)),
            Ok(TimerEvent::TimedOut {
                transaction_id: 5,
                recovery: TimeoutRecovery::RetrySafe,
                quarantine_until: MonotonicMicros::new(51_100),
            })
        );
        assert_eq!(
            core.receive(
                MonotonicMicros::new(2_000),
                &response(active.transaction_id)
            ),
            Ok(ResponseDisposition::Late)
        );
        assert_eq!(
            core.begin(
                MonotonicMicros::new(2_000),
                request(TimeoutRecovery::RetrySafe)
            ),
            Err(TransactionError::ChannelBusy)
        );
        assert_eq!(
            core.on_timer(MonotonicMicros::new(51_100)),
            Ok(TimerEvent::QuarantineEnded)
        );

        let retry = core
            .begin(
                MonotonicMicros::new(51_100),
                request(TimeoutRecovery::RetrySafe),
            )
            .unwrap();
        assert_eq!(retry.transaction_id, 6);
    }

    #[test]
    fn timeout_preserves_the_services_reconciliation_requirement() {
        let mut core = TransactionCore::new(9, duration(50_000));
        core.begin(
            MonotonicMicros::new(0),
            request(TimeoutRecovery::ReconcileState),
        )
        .unwrap();

        assert_eq!(
            core.on_timer(MonotonicMicros::new(1_001)),
            Ok(TimerEvent::TimedOut {
                transaction_id: 9,
                recovery: TimeoutRecovery::ReconcileState,
                quarantine_until: MonotonicMicros::new(51_000),
            })
        );
    }

    #[test]
    fn rejects_broadcast_and_zero_durations() {
        assert_eq!(DurationMicros::new(0), Err(TransactionError::ZeroDuration));

        let mut core = TransactionCore::new(0, duration(1));
        let mut broadcast = request(TimeoutRecovery::RetrySafe);
        broadcast.node_address = 255;
        assert_eq!(
            core.begin(MonotonicMicros::new(0), broadcast),
            Err(TransactionError::RequestRequiresAddressedNode)
        );
    }
}
