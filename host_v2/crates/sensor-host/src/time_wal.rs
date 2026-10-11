use core::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use sensor_core::{BootEpoch, ClockObservation, HardwareId};
use sensor_protocol::{crc32c, BROADCAST_ADDRESS, HOST_ADDRESS};

pub const TIME_WAL_HEADER_BYTES: usize = 64;
pub const TIME_WAL_RECORD_BYTES: usize = 36;
pub const LINUX_BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";

const FILE_MAGIC: [u8; 8] = *b"SSTIMWAL";
const FILE_VERSION: u16 = 1;
const FILE_HEADER_CRC_OFFSET: usize = TIME_WAL_HEADER_BYTES - 4;
const RECORD_PAYLOAD_BYTES: usize = TIME_WAL_RECORD_BYTES - 4;

/// Identifies the lifetime of the host monotonic clock used for `t1` and `t4`.
///
/// On Linux this should be derived from `/proc/sys/kernel/random/boot_id`.
/// Keeping it in the file header prevents values from different host boots
/// from being fitted into one clock model.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HostClockId([u8; 16]);

impl HostClockId {
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub const fn bytes(self) -> [u8; 16] {
        self.0
    }

    pub const fn is_zero(self) -> bool {
        let bytes = self.0;
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] != 0 {
                return false;
            }
            index += 1;
        }
        true
    }

    /// Reads a Linux boot UUID, normally from
    /// `/proc/sys/kernel/random/boot_id`.
    pub fn read_linux_boot_id(path: impl AsRef<Path>) -> Result<Self, HostClockIdError> {
        let value = std::fs::read_to_string(path).map_err(HostClockIdError::Io)?;
        value.trim().parse()
    }

    pub fn read_current_linux_boot_id() -> Result<Self, HostClockIdError> {
        Self::read_linux_boot_id(LINUX_BOOT_ID_PATH)
    }
}

impl FromStr for HostClockId {
    type Err = HostClockIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = value.as_bytes();
        if bytes.len() != 36
            || bytes[8] != b'-'
            || bytes[13] != b'-'
            || bytes[18] != b'-'
            || bytes[23] != b'-'
        {
            return Err(HostClockIdError::InvalidUuid);
        }
        let mut decoded = [0_u8; 16];
        let mut source = 0;
        let mut target = 0;
        while source < bytes.len() {
            if matches!(source, 8 | 13 | 18 | 23) {
                source += 1;
                continue;
            }
            let high = decode_hex(bytes[source]).ok_or(HostClockIdError::InvalidUuid)?;
            let low = decode_hex(bytes[source + 1]).ok_or(HostClockIdError::InvalidUuid)?;
            decoded[target] = (high << 4) | low;
            source += 2;
            target += 1;
        }
        let id = Self(decoded);
        if id.is_zero() {
            Err(HostClockIdError::ZeroUuid)
        } else {
            Ok(id)
        }
    }
}

impl fmt::Display for HostClockId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.bytes();
        write!(
            formatter,
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            bytes[0],
            bytes[1],
            bytes[2],
            bytes[3],
            bytes[4],
            bytes[5],
            bytes[6],
            bytes[7],
            bytes[8],
            bytes[9],
            bytes[10],
            bytes[11],
            bytes[12],
            bytes[13],
            bytes[14],
            bytes[15]
        )
    }
}

const fn decode_hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug)]
pub enum HostClockIdError {
    Io(io::Error),
    InvalidUuid,
    ZeroUuid,
}

impl fmt::Display for HostClockIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "host clock id error: {self:?}")
    }
}

impl std::error::Error for HostClockIdError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeWalConfig {
    pub hardware_id: HardwareId,
    pub node_address: u8,
    pub boot_epoch: BootEpoch,
    pub host_clock_id: HostClockId,
}

impl TimeWalConfig {
    fn validate(self) -> Result<Self, TimeWalError> {
        if self.node_address == HOST_ADDRESS || self.node_address == BROADCAST_ADDRESS {
            return Err(TimeWalError::InvalidNodeAddress(self.node_address));
        }
        if self.boot_epoch.get() == 0 {
            return Err(TimeWalError::ZeroBootEpoch);
        }
        if self.host_clock_id.is_zero() {
            return Err(TimeWalError::ZeroHostClockId);
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeWalRecovery {
    pub record_count: u64,
    pub last_observation: Option<ClockObservation>,
    pub truncated_tail_bytes: u64,
}

pub struct TimeWalSink {
    path: PathBuf,
    file: File,
    config: TimeWalConfig,
    recovery: TimeWalRecovery,
    durable_len: u64,
    poisoned: bool,
    #[cfg(test)]
    fail_before_next_sync: bool,
}

impl TimeWalSink {
    pub fn open(path: impl AsRef<Path>, config: TimeWalConfig) -> Result<Self, TimeWalError> {
        let config = config.validate()?;
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(TimeWalError::Io)?;
        }
        let existed = path.exists();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(TimeWalError::Io)?;
        if !existed || file.metadata().map_err(TimeWalError::Io)?.len() == 0 {
            write_header(&mut file, config)?;
            file.sync_all().map_err(TimeWalError::Io)?;
            sync_parent_directory(&path)?;
        }
        let (recovery, durable_len) = recover(&mut file, config)?;
        file.seek(SeekFrom::Start(durable_len))
            .map_err(TimeWalError::Io)?;
        Ok(Self {
            path,
            file,
            config,
            recovery,
            durable_len,
            poisoned: false,
            #[cfg(test)]
            fail_before_next_sync: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub const fn config(&self) -> TimeWalConfig {
        self.config
    }

    pub const fn recovery(&self) -> TimeWalRecovery {
        self.recovery
    }

    pub const fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    #[cfg(test)]
    pub(crate) fn inject_failure_before_next_sync(&mut self) {
        self.fail_before_next_sync = true;
    }

    pub fn append(&mut self, observation: ClockObservation) -> Result<(), TimeWalError> {
        self.append_batch(core::slice::from_ref(&observation))
    }

    /// Persists a group with one durability barrier.
    ///
    /// The complete group is validated before the first byte is written.
    /// Any write or sync failure poisons this handle; callers must reopen and
    /// recover before deciding whether to retry an uncertain group.
    pub fn append_batch(&mut self, observations: &[ClockObservation]) -> Result<(), TimeWalError> {
        if self.poisoned {
            return Err(TimeWalError::Poisoned);
        }
        if observations.is_empty() {
            return Err(TimeWalError::EmptyBatch);
        }
        let mut previous = self.recovery.last_observation;
        for observation in observations {
            validate_observation(self.config.boot_epoch, previous, *observation)?;
            previous = Some(*observation);
        }
        let byte_len = observations
            .len()
            .checked_mul(TIME_WAL_RECORD_BYTES)
            .ok_or(TimeWalError::LengthOverflow)?;
        let durable_len = self
            .durable_len
            .checked_add(byte_len as u64)
            .ok_or(TimeWalError::LengthOverflow)?;
        let record_count = self
            .recovery
            .record_count
            .checked_add(observations.len() as u64)
            .ok_or(TimeWalError::LengthOverflow)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_len)
            .map_err(|_| TimeWalError::LengthOverflow)?;
        for observation in observations {
            bytes.extend_from_slice(&encode_record(*observation));
        }
        if let Err(error) = self.file.write_all(&bytes) {
            self.poisoned = true;
            return Err(TimeWalError::Io(error));
        }
        #[cfg(test)]
        if self.fail_before_next_sync {
            self.fail_before_next_sync = false;
            self.poisoned = true;
            return Err(TimeWalError::Io(io::Error::other(
                "injected failure before durability barrier",
            )));
        }
        if let Err(error) = self.file.sync_data() {
            self.poisoned = true;
            return Err(TimeWalError::Io(error));
        }
        self.durable_len = durable_len;
        self.recovery.record_count = record_count;
        self.recovery.last_observation = previous;
        Ok(())
    }
}

pub struct TimeWalReader {
    file: File,
    config: TimeWalConfig,
    offset: u64,
    file_len: u64,
    previous: Option<ClockObservation>,
}

impl TimeWalReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TimeWalError> {
        let mut file = File::open(path).map_err(TimeWalError::Io)?;
        let file_len = file.metadata().map_err(TimeWalError::Io)?.len();
        if file_len < TIME_WAL_HEADER_BYTES as u64 {
            return Err(TimeWalError::TruncatedHeader);
        }
        if (file_len - TIME_WAL_HEADER_BYTES as u64) % TIME_WAL_RECORD_BYTES as u64 != 0 {
            return Err(TimeWalError::IncompleteRecord {
                offset: file_len
                    - (file_len - TIME_WAL_HEADER_BYTES as u64) % TIME_WAL_RECORD_BYTES as u64,
            });
        }
        let config = read_header(&mut file)?;
        Ok(Self {
            file,
            config,
            offset: TIME_WAL_HEADER_BYTES as u64,
            file_len,
            previous: None,
        })
    }

    pub const fn config(&self) -> TimeWalConfig {
        self.config
    }

    pub fn next_observation(&mut self) -> Result<Option<ClockObservation>, TimeWalError> {
        if self.offset == self.file_len {
            return Ok(None);
        }
        self.file
            .seek(SeekFrom::Start(self.offset))
            .map_err(TimeWalError::Io)?;
        let mut bytes = [0_u8; TIME_WAL_RECORD_BYTES];
        self.file.read_exact(&mut bytes).map_err(TimeWalError::Io)?;
        let observation = decode_record(&bytes, self.config.boot_epoch, self.offset)?;
        validate_observation(self.config.boot_epoch, self.previous, observation)?;
        self.previous = Some(observation);
        self.offset += TIME_WAL_RECORD_BYTES as u64;
        Ok(Some(observation))
    }
}

fn recover(
    file: &mut File,
    expected: TimeWalConfig,
) -> Result<(TimeWalRecovery, u64), TimeWalError> {
    let file_len = file.metadata().map_err(TimeWalError::Io)?.len();
    if file_len < TIME_WAL_HEADER_BYTES as u64 {
        return Err(TimeWalError::TruncatedHeader);
    }
    let actual = read_header(file)?;
    validate_header_identity(expected, actual)?;

    let payload_len = file_len - TIME_WAL_HEADER_BYTES as u64;
    let complete_len = payload_len / TIME_WAL_RECORD_BYTES as u64 * TIME_WAL_RECORD_BYTES as u64;
    let durable_len = TIME_WAL_HEADER_BYTES as u64 + complete_len;
    let truncated_tail_bytes = file_len - durable_len;
    let mut recovery = TimeWalRecovery {
        record_count: 0,
        last_observation: None,
        truncated_tail_bytes,
    };
    let mut offset = TIME_WAL_HEADER_BYTES as u64;
    while offset < durable_len {
        file.seek(SeekFrom::Start(offset))
            .map_err(TimeWalError::Io)?;
        let mut bytes = [0_u8; TIME_WAL_RECORD_BYTES];
        file.read_exact(&mut bytes).map_err(TimeWalError::Io)?;
        let observation = decode_record(&bytes, expected.boot_epoch, offset)?;
        validate_observation(expected.boot_epoch, recovery.last_observation, observation)?;
        recovery.record_count = recovery
            .record_count
            .checked_add(1)
            .ok_or(TimeWalError::LengthOverflow)?;
        recovery.last_observation = Some(observation);
        offset += TIME_WAL_RECORD_BYTES as u64;
    }
    if truncated_tail_bytes != 0 {
        file.set_len(durable_len).map_err(TimeWalError::Io)?;
        file.sync_data().map_err(TimeWalError::Io)?;
    }
    Ok((recovery, durable_len))
}

fn validate_header_identity(
    expected: TimeWalConfig,
    actual: TimeWalConfig,
) -> Result<(), TimeWalError> {
    if actual.hardware_id != expected.hardware_id {
        return Err(TimeWalError::HardwareMismatch);
    }
    if actual.node_address != expected.node_address {
        return Err(TimeWalError::NodeAddressMismatch);
    }
    if actual.boot_epoch != expected.boot_epoch {
        return Err(TimeWalError::BootEpochMismatch);
    }
    if actual.host_clock_id != expected.host_clock_id {
        return Err(TimeWalError::HostClockMismatch);
    }
    Ok(())
}

fn validate_observation(
    boot_epoch: BootEpoch,
    previous: Option<ClockObservation>,
    observation: ClockObservation,
) -> Result<(), TimeWalError> {
    if observation.boot_epoch != boot_epoch {
        return Err(TimeWalError::BootEpochMismatch);
    }
    ClockObservation::new(
        observation.boot_epoch,
        observation.host_send_us,
        observation.node_receive_us,
        observation.node_send_us,
        observation.host_receive_us,
    )
    .map_err(TimeWalError::Observation)?;
    if let Some(previous) = previous {
        if observation.host_send_us <= previous.host_send_us
            || observation.node_receive_us <= previous.node_receive_us
            || observation.node_send_us <= previous.node_send_us
            || observation.host_receive_us <= previous.host_receive_us
        {
            return Err(TimeWalError::NonMonotonicObservation);
        }
    }
    Ok(())
}

fn write_header(file: &mut File, config: TimeWalConfig) -> Result<(), TimeWalError> {
    let mut bytes = [0_u8; TIME_WAL_HEADER_BYTES];
    bytes[0..8].copy_from_slice(&FILE_MAGIC);
    bytes[8..10].copy_from_slice(&FILE_VERSION.to_le_bytes());
    bytes[10..12].copy_from_slice(&(TIME_WAL_HEADER_BYTES as u16).to_le_bytes());
    bytes[12..14].copy_from_slice(&(TIME_WAL_RECORD_BYTES as u16).to_le_bytes());
    bytes[16..24].copy_from_slice(&config.hardware_id.bytes());
    bytes[24..32].copy_from_slice(&config.boot_epoch.get().to_le_bytes());
    bytes[32..48].copy_from_slice(&config.host_clock_id.bytes());
    bytes[48] = config.node_address;
    let crc = crc32c(&bytes[..FILE_HEADER_CRC_OFFSET]);
    bytes[FILE_HEADER_CRC_OFFSET..].copy_from_slice(&crc.to_le_bytes());
    file.seek(SeekFrom::Start(0)).map_err(TimeWalError::Io)?;
    file.write_all(&bytes).map_err(TimeWalError::Io)
}

fn read_header(file: &mut File) -> Result<TimeWalConfig, TimeWalError> {
    file.seek(SeekFrom::Start(0)).map_err(TimeWalError::Io)?;
    let mut bytes = [0_u8; TIME_WAL_HEADER_BYTES];
    file.read_exact(&mut bytes).map_err(TimeWalError::Io)?;
    if bytes[0..8] != FILE_MAGIC {
        return Err(TimeWalError::InvalidMagic);
    }
    let version = read_u16(&bytes, 8);
    if version != FILE_VERSION {
        return Err(TimeWalError::UnsupportedVersion(version));
    }
    if usize::from(read_u16(&bytes, 10)) != TIME_WAL_HEADER_BYTES {
        return Err(TimeWalError::InvalidHeaderLength);
    }
    if usize::from(read_u16(&bytes, 12)) != TIME_WAL_RECORD_BYTES {
        return Err(TimeWalError::InvalidRecordLength);
    }
    if crc32c(&bytes[..FILE_HEADER_CRC_OFFSET]) != read_u32(&bytes, FILE_HEADER_CRC_OFFSET) {
        return Err(TimeWalError::HeaderChecksumMismatch);
    }
    if bytes[14..16]
        .iter()
        .chain(bytes[49..FILE_HEADER_CRC_OFFSET].iter())
        .any(|value| *value != 0)
    {
        return Err(TimeWalError::NonZeroReservedField);
    }
    TimeWalConfig {
        hardware_id: HardwareId::from_bytes(bytes[16..24].try_into().expect("fixed field")),
        node_address: bytes[48],
        boot_epoch: BootEpoch::new(read_u64(&bytes, 24)),
        host_clock_id: HostClockId::from_bytes(bytes[32..48].try_into().expect("fixed field")),
    }
    .validate()
}

fn encode_record(observation: ClockObservation) -> [u8; TIME_WAL_RECORD_BYTES] {
    let mut bytes = [0_u8; TIME_WAL_RECORD_BYTES];
    bytes[0..8].copy_from_slice(&observation.host_send_us.to_le_bytes());
    bytes[8..16].copy_from_slice(&observation.node_receive_us.to_le_bytes());
    bytes[16..24].copy_from_slice(&observation.node_send_us.to_le_bytes());
    bytes[24..32].copy_from_slice(&observation.host_receive_us.to_le_bytes());
    let crc = crc32c(&bytes[..RECORD_PAYLOAD_BYTES]);
    bytes[RECORD_PAYLOAD_BYTES..].copy_from_slice(&crc.to_le_bytes());
    bytes
}

fn decode_record(
    bytes: &[u8; TIME_WAL_RECORD_BYTES],
    boot_epoch: BootEpoch,
    offset: u64,
) -> Result<ClockObservation, TimeWalError> {
    if crc32c(&bytes[..RECORD_PAYLOAD_BYTES]) != read_u32(bytes, RECORD_PAYLOAD_BYTES) {
        return Err(TimeWalError::RecordChecksumMismatch { offset });
    }
    ClockObservation::new(
        boot_epoch,
        read_u64(bytes, 0),
        read_u64(bytes, 8),
        read_u64(bytes, 16),
        read_u64(bytes, 24),
    )
    .map_err(TimeWalError::Observation)
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("fixed field"))
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("fixed field"))
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("fixed field"))
}

fn sync_parent_directory(path: &Path) -> Result<(), TimeWalError> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(TimeWalError::Io)
}

#[derive(Debug)]
pub enum TimeWalError {
    Io(io::Error),
    Observation(sensor_core::ClockObservationError),
    InvalidNodeAddress(u8),
    ZeroBootEpoch,
    ZeroHostClockId,
    TruncatedHeader,
    InvalidMagic,
    UnsupportedVersion(u16),
    InvalidHeaderLength,
    InvalidRecordLength,
    HeaderChecksumMismatch,
    RecordChecksumMismatch { offset: u64 },
    NonZeroReservedField,
    HardwareMismatch,
    NodeAddressMismatch,
    BootEpochMismatch,
    HostClockMismatch,
    NonMonotonicObservation,
    IncompleteRecord { offset: u64 },
    EmptyBatch,
    LengthOverflow,
    Poisoned,
}

impl fmt::Display for TimeWalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "time WAL error: {self:?}")
    }
}

impl std::error::Error for TimeWalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Observation(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

    struct TempFile {
        directory: PathBuf,
        path: PathBuf,
    }

    impl TempFile {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let unique = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "sensor-time-wal-{}-{nonce}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join(name);
            Self { directory, path }
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    fn config() -> TimeWalConfig {
        TimeWalConfig {
            hardware_id: HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]),
            node_address: 7,
            boot_epoch: BootEpoch::new(11),
            host_clock_id: HostClockId::from_bytes([9; 16]),
        }
    }

    fn observation(index: u64) -> ClockObservation {
        let base = 10_000 + index * 1_000;
        ClockObservation::new(config().boot_epoch, base, base + 10, base + 20, base + 40).unwrap()
    }

    #[test]
    fn host_clock_id_parses_the_linux_boot_uuid_contract() {
        let parsed: HostClockId = "00112233-4455-6677-8899-aabbccddeeff".parse().unwrap();
        assert_eq!(
            parsed.bytes(),
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ]
        );
        assert_eq!(parsed.to_string(), "00112233-4455-6677-8899-aabbccddeeff");
        assert!(matches!(
            "00000000-0000-0000-0000-000000000000".parse::<HostClockId>(),
            Err(HostClockIdError::ZeroUuid)
        ));
        assert!(matches!(
            "001122334455-6677-8899-aabbccddeeff".parse::<HostClockId>(),
            Err(HostClockIdError::InvalidUuid)
        ));
    }

    #[test]
    fn host_clock_id_reader_accepts_the_kernel_trailing_newline() {
        let temp = TempFile::new("boot_id");
        std::fs::write(&temp.path, "00112233-4455-6677-8899-AABBCCDDEEFF\n").unwrap();
        assert_eq!(
            HostClockId::read_linux_boot_id(&temp.path).unwrap(),
            "00112233-4455-6677-8899-aabbccddeeff".parse().unwrap()
        );
    }

    #[test]
    fn fixed_records_are_compact_and_round_trip() {
        let temp = TempFile::new("time.wal");
        let mut sink = TimeWalSink::open(&temp.path, config()).unwrap();
        let observations: Vec<_> = (0..100).map(observation).collect();
        sink.append_batch(&observations).unwrap();
        assert_eq!(
            std::fs::metadata(&temp.path).unwrap().len(),
            (TIME_WAL_HEADER_BYTES + 100 * TIME_WAL_RECORD_BYTES) as u64
        );
        assert_eq!(sink.recovery().record_count, 100);
        drop(sink);

        let mut reader = TimeWalReader::open(&temp.path).unwrap();
        assert_eq!(reader.config(), config());
        for expected in observations {
            assert_eq!(reader.next_observation().unwrap(), Some(expected));
        }
        assert_eq!(reader.next_observation().unwrap(), None);
    }

    #[test]
    fn recovery_truncates_only_an_incomplete_tail() {
        let temp = TempFile::new("tail.wal");
        let mut sink = TimeWalSink::open(&temp.path, config()).unwrap();
        sink.append(observation(0)).unwrap();
        drop(sink);
        let mut file = OpenOptions::new().append(true).open(&temp.path).unwrap();
        file.write_all(&encode_record(observation(1))[..13])
            .unwrap();
        file.sync_data().unwrap();
        drop(file);

        let sink = TimeWalSink::open(&temp.path, config()).unwrap();
        assert_eq!(sink.recovery().record_count, 1);
        assert_eq!(sink.recovery().truncated_tail_bytes, 13);
        assert_eq!(
            std::fs::metadata(&temp.path).unwrap().len(),
            (TIME_WAL_HEADER_BYTES + TIME_WAL_RECORD_BYTES) as u64
        );
    }

    #[test]
    fn full_corrupt_record_fails_closed() {
        let temp = TempFile::new("corrupt.wal");
        let mut sink = TimeWalSink::open(&temp.path, config()).unwrap();
        sink.append(observation(0)).unwrap();
        drop(sink);
        let mut file = OpenOptions::new().write(true).open(&temp.path).unwrap();
        file.seek(SeekFrom::Start(TIME_WAL_HEADER_BYTES as u64 + 3))
            .unwrap();
        file.write_all(&[0xff]).unwrap();
        file.sync_data().unwrap();
        drop(file);
        match TimeWalSink::open(&temp.path, config()) {
            Err(TimeWalError::RecordChecksumMismatch { .. }) => {}
            Err(error) => panic!("unexpected recovery error: {error:?}"),
            Ok(_) => panic!("corrupt complete record was accepted"),
        }
    }

    #[test]
    fn validates_stream_identity_and_order_before_append() {
        let temp = TempFile::new("identity.wal");
        let mut sink = TimeWalSink::open(&temp.path, config()).unwrap();
        sink.append(observation(1)).unwrap();
        assert!(matches!(
            sink.append(observation(0)),
            Err(TimeWalError::NonMonotonicObservation)
        ));
        assert_eq!(sink.recovery().record_count, 1);
        drop(sink);

        let mut wrong = config();
        wrong.host_clock_id = HostClockId::from_bytes([8; 16]);
        assert!(matches!(
            TimeWalSink::open(&temp.path, wrong),
            Err(TimeWalError::HostClockMismatch)
        ));
    }
}
