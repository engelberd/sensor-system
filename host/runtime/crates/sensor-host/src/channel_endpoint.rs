use core::fmt;
use std::io::{Read, Write};
#[cfg(not(target_os = "linux"))]
use std::time::Instant;

use sensor_protocol::FrameError;

use crate::channel_agent::{ChannelAgent, ChannelAgentError, ChannelEvent};
use crate::channel_port::{ChannelPort, ChannelPortError, ReceiveEvent};
use crate::transaction::MonotonicMicros;

pub trait MonotonicClock {
    fn now(&self) -> MonotonicMicros;
}

#[derive(Clone, Debug)]
pub struct SystemMonotonicClock {
    #[cfg(not(target_os = "linux"))]
    origin: Instant,
}

impl SystemMonotonicClock {
    pub fn new() -> Self {
        Self::try_new().expect("system monotonic clock must be available")
    }

    pub fn try_new() -> Result<Self, SystemMonotonicClockError> {
        #[cfg(target_os = "linux")]
        linux_boottime_micros()?;

        Ok(Self {
            #[cfg(not(target_os = "linux"))]
            origin: Instant::now(),
        })
    }
}

impl Default for SystemMonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MonotonicClock for SystemMonotonicClock {
    fn now(&self) -> MonotonicMicros {
        #[cfg(target_os = "linux")]
        let micros = linux_boottime_micros()
            .expect("CLOCK_BOOTTIME became unavailable after clock construction");
        #[cfg(not(target_os = "linux"))]
        let micros = self.origin.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;

        MonotonicMicros::new(micros)
    }
}

#[cfg(target_os = "linux")]
fn linux_boottime_micros() -> Result<u64, SystemMonotonicClockError> {
    let value = nix::time::ClockId::CLOCK_BOOTTIME
        .now()
        .map_err(|_| SystemMonotonicClockError::ClockRead)?;
    let seconds =
        u64::try_from(value.tv_sec()).map_err(|_| SystemMonotonicClockError::InvalidClockValue)?;
    let nanoseconds =
        u64::try_from(value.tv_nsec()).map_err(|_| SystemMonotonicClockError::InvalidClockValue)?;
    seconds
        .checked_mul(1_000_000)
        .and_then(|micros| micros.checked_add(nanoseconds / 1_000))
        .ok_or(SystemMonotonicClockError::InvalidClockValue)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemMonotonicClockError {
    ClockRead,
    InvalidClockValue,
}

impl fmt::Display for SystemMonotonicClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "system monotonic clock error: {self:?}")
    }
}

impl std::error::Error for SystemMonotonicClockError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChannelEndpointEvent {
    Idle,
    TransmissionComplete { node_address: u8 },
    Agent(ChannelEvent),
    DecodeError(FrameError),
    Disconnected,
}

/// Composition boundary for one physical communication channel.
///
/// This is the only host-v2 object that owns both the byte port and its
/// `ChannelAgent`. A step performs at most one externally observable action,
/// keeping execution deterministic and preventing hidden parallel requests on
/// half-duplex RS485.
pub struct ChannelEndpoint<P, C> {
    agent: ChannelAgent,
    port: ChannelPort<P>,
    clock: C,
}

impl<P, C> ChannelEndpoint<P, C>
where
    P: Read + Write,
    C: MonotonicClock,
{
    pub fn new(agent: ChannelAgent, port: ChannelPort<P>, clock: C) -> Self {
        Self { agent, port, clock }
    }

    pub fn step(&mut self) -> Result<ChannelEndpointEvent, ChannelEndpointError> {
        let agent_event = self.agent.poll(self.clock.now())?;
        if agent_event == ChannelEvent::TransmitReady {
            let frame = self
                .agent
                .take_outbound()
                .ok_or(ChannelEndpointError::InvariantViolation)?;
            let node_address = frame.destination;
            if let Err(error) = self.port.transmit(&frame) {
                self.agent.transmit_failed(self.clock.now())?;
                return Err(error.into());
            }
            self.agent.transmit_completed(self.clock.now())?;
            return Ok(ChannelEndpointEvent::TransmissionComplete { node_address });
        }
        if agent_event != ChannelEvent::Idle {
            return Ok(ChannelEndpointEvent::Agent(agent_event));
        }

        match self.port.poll_receive()? {
            ReceiveEvent::NoData => Ok(ChannelEndpointEvent::Idle),
            ReceiveEvent::FrameReady => {
                let frame = self
                    .port
                    .take_received()
                    .ok_or(ChannelEndpointError::InvariantViolation)?;
                let event = self.agent.receive(self.clock.now(), &frame)?;
                Ok(ChannelEndpointEvent::Agent(event))
            }
            ReceiveEvent::DecodeError(error) => Ok(ChannelEndpointEvent::DecodeError(error)),
            ReceiveEvent::Disconnected => Ok(ChannelEndpointEvent::Disconnected),
        }
    }

    pub fn agent(&self) -> &ChannelAgent {
        &self.agent
    }

    pub fn agent_mut(&mut self) -> &mut ChannelAgent {
        &mut self.agent
    }

    pub fn now(&self) -> MonotonicMicros {
        self.clock.now()
    }

    pub fn into_parts(self) -> (ChannelAgent, ChannelPort<P>, C) {
        (self.agent, self.port, self.clock)
    }
}

#[derive(Debug)]
pub enum ChannelEndpointError {
    InvariantViolation,
    Agent(ChannelAgentError),
    Port(ChannelPortError),
}

impl From<ChannelAgentError> for ChannelEndpointError {
    fn from(value: ChannelAgentError) -> Self {
        Self::Agent(value)
    }
}

impl From<ChannelPortError> for ChannelEndpointError {
    fn from(value: ChannelPortError) -> Self {
        Self::Port(value)
    }
}

impl fmt::Display for ChannelEndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvariantViolation => formatter.write_str("channel endpoint invariant violated"),
            Self::Agent(error) => write!(formatter, "channel agent failed: {error}"),
            Self::Port(error) => write!(formatter, "channel port failed: {error}"),
        }
    }
}

impl std::error::Error for ChannelEndpointError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_agent::ChannelPolicy;
    use crate::transaction::DurationMicros;
    use sensor_protocol::{
        encode_lifecycle_response, Frame, FrameRole, ObservabilityProfile, RuntimeOperation,
        RuntimeResult, RuntimeResultCode, RuntimeState, SensorStatusCode, StreamDecoder,
        HOST_ADDRESS, MAX_WIRE_FRAME_BYTES,
    };
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::io;

    #[cfg(target_os = "linux")]
    #[test]
    fn clock_value_does_not_restart_with_each_instance() {
        let first = SystemMonotonicClock::try_new().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let before_second = first.now();
        let second = SystemMonotonicClock::try_new().unwrap();
        assert!(second.now().get() >= before_second.get());
    }

    #[derive(Default)]
    struct ScriptedIo {
        reads: VecDeque<io::Result<Vec<u8>>>,
        written: Vec<u8>,
        fail_write: bool,
    }

    impl Read for ScriptedIo {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let Some(next) = self.reads.pop_front() else {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no response"));
            };
            let bytes = next?;
            output[..bytes.len()].copy_from_slice(&bytes);
            Ok(bytes.len())
        }
    }

    impl Write for ScriptedIo {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_write {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "disconnected"));
            }
            self.written.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct ManualClock(Cell<u64>);

    impl ManualClock {
        fn new(now: u64) -> Self {
            Self(Cell::new(now))
        }

        fn set(&self, now: u64) {
            self.0.set(now);
        }
    }

    impl MonotonicClock for ManualClock {
        fn now(&self) -> MonotonicMicros {
            MonotonicMicros::new(self.0.get())
        }
    }

    fn duration(value: u64) -> DurationMicros {
        DurationMicros::new(value).unwrap()
    }

    fn agent() -> ChannelAgent {
        ChannelAgent::new(
            10,
            100,
            duration(50),
            ChannelPolicy {
                transaction_timeout: duration(100),
                retry_delay: duration(10),
                measurement_attempts_per_slot: 2,
                max_pending_work_items: 4,
            },
        )
        .unwrap()
    }

    fn encode(frame: &Frame) -> Vec<u8> {
        let mut output = [0; MAX_WIRE_FRAME_BYTES];
        let len = frame.encode(&mut output);
        output[..len].to_vec()
    }

    fn decode(bytes: &[u8]) -> Frame {
        let mut decoder = StreamDecoder::new();
        bytes
            .iter()
            .find_map(|byte| decoder.push(*byte))
            .expect("complete frame")
            .unwrap()
    }

    #[test]
    fn drives_one_lifecycle_exchange_through_real_wire_codec() {
        let mut agent = agent();
        let operation_id = agent
            .submit_lifecycle(7, RuntimeOperation::Stop, ObservabilityProfile::Production)
            .unwrap();
        let response = Frame::new(
            FrameRole::Response,
            HOST_ADDRESS,
            7,
            sensor_protocol::LIFECYCLE_SERVICE_ID,
            sensor_protocol::STOP_RECORDING_MESSAGE_ID,
            10,
            &encode_lifecycle_response(RuntimeResult {
                operation_id,
                code: RuntimeResultCode::Ok,
                state: RuntimeState::Idle,
                observability: ObservabilityProfile::Production,
                sensor_status: SensorStatusCode::Ok,
            })
            .unwrap(),
        )
        .unwrap();
        let mut io = ScriptedIo::default();
        io.reads.push_back(Ok(encode(&response)));
        let clock = ManualClock::new(0);
        let mut endpoint = ChannelEndpoint::new(agent, ChannelPort::from_io(io), clock);

        assert_eq!(
            endpoint.step().unwrap(),
            ChannelEndpointEvent::TransmissionComplete { node_address: 7 }
        );
        endpoint.clock.set(1);
        assert!(matches!(
            endpoint.step().unwrap(),
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete {
                node_address: 7,
                ..
            })
        ));

        let (_, port, _) = endpoint.into_parts();
        let request = decode(&port.into_inner().written);
        assert_eq!(request.transaction_id, 10);
        assert_eq!(request.destination, 7);
    }

    #[test]
    fn write_failure_reconciles_the_operation_before_returning_error() {
        let mut agent = agent();
        agent
            .submit_lifecycle(7, RuntimeOperation::Stop, ObservabilityProfile::Production)
            .unwrap();
        let io = ScriptedIo {
            fail_write: true,
            ..ScriptedIo::default()
        };
        let clock = ManualClock::new(0);
        let mut endpoint = ChannelEndpoint::new(agent, ChannelPort::from_io(io), clock);

        assert!(matches!(
            endpoint.step(),
            Err(ChannelEndpointError::Port(_))
        ));
        endpoint.clock.set(9);
        assert_eq!(endpoint.step().unwrap(), ChannelEndpointEvent::Idle);
        endpoint.clock.set(10);
        assert!(matches!(
            endpoint.step(),
            Err(ChannelEndpointError::Port(_))
        ));
    }
}
