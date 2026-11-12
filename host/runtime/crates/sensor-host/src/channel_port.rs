use core::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sensor_protocol::{Frame, FrameError, StreamDecoder, MAX_WIRE_FRAME_BYTES};
use serialport::{DataBits, FlowControl, Parity, StopBits};

const READ_BUFFER_BYTES: usize = 256;
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SerialPortConfig {
    pub device: PathBuf,
    pub baud_rate: u32,
    pub io_timeout: Duration,
}

impl SerialPortConfig {
    pub fn rs485(device: impl Into<PathBuf>, baud_rate: u32) -> Self {
        Self {
            device: device.into(),
            baud_rate,
            io_timeout: Duration::from_millis(10),
        }
    }

    pub fn validate(&self) -> Result<(), ChannelPortError> {
        if self.device.as_os_str().is_empty() {
            return Err(ChannelPortError::EmptyDevicePath);
        }
        if self.baud_rate == 0 {
            return Err(ChannelPortError::ZeroBaudRate);
        }
        if self.io_timeout.is_zero() {
            return Err(ChannelPortError::ZeroIoTimeout);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiveEvent {
    NoData,
    FrameReady,
    DecodeError(FrameError),
    Disconnected,
}

/// Bounded wire adapter for one physical half-duplex channel.
///
/// It owns stream framing and byte I/O, but deliberately knows nothing about
/// lifecycle, measurement cursors, retries or scheduling.
pub struct ChannelPort<P> {
    port: P,
    decoder: StreamDecoder,
    read_buffer: [u8; READ_BUFFER_BYTES],
    read_cursor: usize,
    read_len: usize,
    received: Option<Frame>,
    write_buffer: [u8; MAX_WIRE_FRAME_BYTES],
}

impl<P> ChannelPort<P>
where
    P: Read + Write,
{
    pub fn from_io(port: P) -> Self {
        Self {
            port,
            decoder: StreamDecoder::new(),
            read_buffer: [0; READ_BUFFER_BYTES],
            read_cursor: 0,
            read_len: 0,
            received: None,
            write_buffer: [0; MAX_WIRE_FRAME_BYTES],
        }
    }

    /// Writes one complete encoded frame and drains the OS/driver TX queue.
    /// A successful return is the boundary at which response timing may start.
    pub fn transmit(&mut self, frame: &Frame) -> Result<(), ChannelPortError> {
        let wire_len = frame.encode(&mut self.write_buffer);
        self.port.write_all(&self.write_buffer[..wire_len])?;
        self.port.flush()?;
        Ok(())
    }

    /// Advances decoding until one frame/error is available or the read call
    /// has no bytes. Remaining bytes stay in the fixed buffer for the next call.
    pub fn poll_receive(&mut self) -> Result<ReceiveEvent, ChannelPortError> {
        if self.received.is_some() {
            return Ok(ReceiveEvent::FrameReady);
        }
        if let Some(event) = self.decode_buffered() {
            return Ok(event);
        }

        let read_len = match self.port.read(&mut self.read_buffer) {
            Ok(0) => return Ok(ReceiveEvent::Disconnected),
            Ok(count) => count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Ok(ReceiveEvent::NoData);
            }
            Err(error) => return Err(error.into()),
        };
        self.read_cursor = 0;
        self.read_len = read_len;

        Ok(self.decode_buffered().unwrap_or(ReceiveEvent::NoData))
    }

    pub fn take_received(&mut self) -> Option<Frame> {
        self.received.take()
    }

    pub fn discard_partial_receive(&mut self) {
        self.decoder.discard_partial();
        self.read_cursor = self.read_len;
    }

    pub fn into_inner(self) -> P {
        self.port
    }

    fn decode_buffered(&mut self) -> Option<ReceiveEvent> {
        while self.read_cursor < self.read_len {
            let byte = self.read_buffer[self.read_cursor];
            self.read_cursor += 1;
            if let Some(decoded) = self.decoder.push(byte) {
                return Some(match decoded {
                    Ok(frame) => {
                        self.received = Some(frame);
                        ReceiveEvent::FrameReady
                    }
                    Err(error) => ReceiveEvent::DecodeError(error),
                });
            }
        }
        None
    }
}

pub type SerialChannelPort = ChannelPort<Box<dyn serialport::SerialPort>>;

impl SerialChannelPort {
    pub fn open(config: &SerialPortConfig) -> Result<Self, ChannelPortError> {
        config.validate()?;
        let port = serialport::new(path_string(&config.device)?, config.baud_rate)
            .data_bits(DataBits::Eight)
            .flow_control(FlowControl::None)
            .parity(Parity::None)
            .stop_bits(StopBits::One)
            .timeout(config.io_timeout)
            .open()?;
        Ok(Self::from_io(port))
    }
}

fn path_string(path: &Path) -> Result<&str, ChannelPortError> {
    path.to_str().ok_or(ChannelPortError::NonUtf8DevicePath)
}

#[derive(Debug)]
pub enum ChannelPortError {
    EmptyDevicePath,
    NonUtf8DevicePath,
    ZeroBaudRate,
    ZeroIoTimeout,
    Open(serialport::Error),
    Io(io::Error),
}

impl From<serialport::Error> for ChannelPortError {
    fn from(value: serialport::Error) -> Self {
        Self::Open(value)
    }
}

impl From<io::Error> for ChannelPortError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl fmt::Display for ChannelPortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDevicePath => formatter.write_str("serial device path is empty"),
            Self::NonUtf8DevicePath => formatter.write_str("serial device path is not UTF-8"),
            Self::ZeroBaudRate => formatter.write_str("serial baud rate must be non-zero"),
            Self::ZeroIoTimeout => formatter.write_str("serial I/O timeout must be non-zero"),
            Self::Open(error) => write!(formatter, "cannot open serial port: {error}"),
            Self::Io(error) => write!(formatter, "serial I/O failed: {error}"),
        }
    }
}

impl std::error::Error for ChannelPortError {}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_protocol::{FrameRole, HOST_ADDRESS};
    use std::collections::VecDeque;

    #[derive(Default)]
    struct FakeIo {
        reads: VecDeque<io::Result<Vec<u8>>>,
        written: Vec<u8>,
        max_write: usize,
        flushes: usize,
    }

    impl Read for FakeIo {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let Some(next) = self.reads.pop_front() else {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no bytes"));
            };
            let bytes = next?;
            let count = bytes.len().min(output.len());
            output[..count].copy_from_slice(&bytes[..count]);
            if count < bytes.len() {
                self.reads.push_front(Ok(bytes[count..].to_vec()));
            }
            Ok(count)
        }
    }

    impl Write for FakeIo {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let count = if self.max_write == 0 {
                bytes.len()
            } else {
                self.max_write.min(bytes.len())
            };
            self.written.extend_from_slice(&bytes[..count]);
            Ok(count)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    fn frame(transaction_id: u32, payload: &[u8]) -> Frame {
        Frame::new(
            FrameRole::Response,
            HOST_ADDRESS,
            7,
            1,
            2,
            transaction_id,
            payload,
        )
        .unwrap()
    }

    fn encoded(frame: &Frame) -> Vec<u8> {
        let mut output = [0; MAX_WIRE_FRAME_BYTES];
        let len = frame.encode(&mut output);
        output[..len].to_vec()
    }

    #[test]
    fn transmit_handles_partial_writes_and_drains_before_success() {
        let io = FakeIo {
            max_write: 3,
            ..FakeIo::default()
        };
        let mut port = ChannelPort::from_io(io);
        let expected = frame(11, b"hello");
        port.transmit(&expected).unwrap();

        let io = port.into_inner();
        assert_eq!(io.written, encoded(&expected));
        assert_eq!(io.flushes, 1);
    }

    #[test]
    fn preserves_a_second_frame_for_the_next_poll() {
        let first = frame(11, b"first");
        let second = frame(12, b"second");
        let mut bytes = encoded(&first);
        bytes.extend_from_slice(&encoded(&second));
        let mut io = FakeIo::default();
        io.reads.push_back(Ok(bytes));
        let mut port = ChannelPort::from_io(io);

        assert_eq!(port.poll_receive().unwrap(), ReceiveEvent::FrameReady);
        assert_eq!(port.take_received(), Some(first));
        assert_eq!(port.poll_receive().unwrap(), ReceiveEvent::FrameReady);
        assert_eq!(port.take_received(), Some(second));
    }

    #[test]
    fn reports_corruption_then_recovers_at_the_next_delimiter() {
        let expected = frame(12, b"valid");
        let mut bytes = vec![1, 2, 3, 0];
        bytes.extend_from_slice(&encoded(&expected));
        let mut io = FakeIo::default();
        io.reads.push_back(Ok(bytes));
        let mut port = ChannelPort::from_io(io);

        assert!(matches!(
            port.poll_receive().unwrap(),
            ReceiveEvent::DecodeError(_)
        ));
        assert_eq!(port.poll_receive().unwrap(), ReceiveEvent::FrameReady);
        assert_eq!(port.take_received(), Some(expected));
    }

    #[test]
    fn timeout_between_fragments_does_not_replay_consumed_bytes() {
        let expected = frame(13, b"fragmented response");
        let bytes = encoded(&expected);
        let split = 9;
        let mut io = FakeIo::default();
        io.reads.push_back(Ok(bytes[..split].to_vec()));
        io.reads.push_back(Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "inter-fragment timeout",
        )));
        io.reads.push_back(Ok(bytes[split..].to_vec()));
        let mut port = ChannelPort::from_io(io);

        assert_eq!(port.poll_receive().unwrap(), ReceiveEvent::NoData);
        assert_eq!(port.poll_receive().unwrap(), ReceiveEvent::NoData);
        assert_eq!(port.poll_receive().unwrap(), ReceiveEvent::FrameReady);
        assert_eq!(port.take_received(), Some(expected));
    }

    #[test]
    fn validates_configuration_without_opening_a_device() {
        let mut config = SerialPortConfig::rs485("/dev/ttyUSB0", 115_200);
        assert!(config.validate().is_ok());
        config.baud_rate = 0;
        assert!(matches!(
            config.validate(),
            Err(ChannelPortError::ZeroBaudRate)
        ));
    }
}
