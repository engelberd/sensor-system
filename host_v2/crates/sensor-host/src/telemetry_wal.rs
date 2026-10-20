use core::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use sensor_core::{BootEpoch, HardwareId};
use sensor_protocol::{
    crc32c, Adxl355TemperatureCode, Adxl355TemperatureCodeError, TelemetrySnapshot,
    BROADCAST_ADDRESS, HOST_ADDRESS,
};

pub const TELEMETRY_WAL_HEADER_BYTES: usize = 64;
pub const TELEMETRY_WAL_RECORD_BYTES: usize = 16;

const FILE_MAGIC: [u8; 8] = *b"SSTELWAL";
const FILE_VERSION: u16 = 1;
const HEADER_CRC_OFFSET: usize = TELEMETRY_WAL_HEADER_BYTES - 4;
const RECORD_CRC_OFFSET: usize = TELEMETRY_WAL_RECORD_BYTES - 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TelemetryWalConfig {
    pub hardware_id: HardwareId,
    pub node_address: u8,
    pub boot_epoch: BootEpoch,
}

impl TelemetryWalConfig {
    fn validate(self) -> Result<Self, TelemetryWalError> {
        if self.node_address == HOST_ADDRESS || self.node_address == BROADCAST_ADDRESS {
            return Err(TelemetryWalError::InvalidNodeAddress(self.node_address));
        }
        if self.boot_epoch.get() == 0 {
            return Err(TelemetryWalError::ZeroBootEpoch);
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TelemetryWalRecovery {
    pub record_count: u64,
    pub last_snapshot: Option<TelemetrySnapshot>,
    pub truncated_tail_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryAppend {
    Appended,
    Duplicate,
}

pub struct TelemetryWalSink {
    path: PathBuf,
    file: File,
    config: TelemetryWalConfig,
    recovery: TelemetryWalRecovery,
    durable_len: u64,
    poisoned: bool,
}

impl TelemetryWalSink {
    pub fn open(
        path: impl AsRef<Path>,
        config: TelemetryWalConfig,
    ) -> Result<Self, TelemetryWalError> {
        let config = config.validate()?;
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(TelemetryWalError::Io)?;
        }
        let existed = path.exists();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(TelemetryWalError::Io)?;
        if !existed || file.metadata().map_err(TelemetryWalError::Io)?.len() == 0 {
            write_header(&mut file, config)?;
            file.sync_all().map_err(TelemetryWalError::Io)?;
            sync_parent_directory(&path)?;
        }
        let (recovery, durable_len) = recover(&mut file, config)?;
        file.seek(SeekFrom::Start(durable_len))
            .map_err(TelemetryWalError::Io)?;
        Ok(Self {
            path,
            file,
            config,
            recovery,
            durable_len,
            poisoned: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub const fn config(&self) -> TelemetryWalConfig {
        self.config
    }

    pub const fn recovery(&self) -> TelemetryWalRecovery {
        self.recovery
    }

    pub const fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    pub fn append(
        &mut self,
        snapshot: TelemetrySnapshot,
    ) -> Result<TelemetryAppend, TelemetryWalError> {
        if self.poisoned {
            return Err(TelemetryWalError::Poisoned);
        }
        match validate_snapshot(self.recovery.last_snapshot, snapshot)? {
            TelemetryAppend::Duplicate => return Ok(TelemetryAppend::Duplicate),
            TelemetryAppend::Appended => {}
        }
        let durable_len = self
            .durable_len
            .checked_add(TELEMETRY_WAL_RECORD_BYTES as u64)
            .ok_or(TelemetryWalError::LengthOverflow)?;
        let record_count = self
            .recovery
            .record_count
            .checked_add(1)
            .ok_or(TelemetryWalError::LengthOverflow)?;
        let bytes = encode_record(snapshot);
        if let Err(error) = self.file.write_all(&bytes) {
            self.poisoned = true;
            return Err(TelemetryWalError::Io(error));
        }
        if let Err(error) = self.file.sync_data() {
            self.poisoned = true;
            return Err(TelemetryWalError::Io(error));
        }
        self.durable_len = durable_len;
        self.recovery.record_count = record_count;
        self.recovery.last_snapshot = Some(snapshot);
        Ok(TelemetryAppend::Appended)
    }
}

pub struct TelemetryWalReader {
    file: File,
    config: TelemetryWalConfig,
    offset: u64,
    file_len: u64,
    previous: Option<TelemetrySnapshot>,
}

impl TelemetryWalReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TelemetryWalError> {
        let mut file = File::open(path).map_err(TelemetryWalError::Io)?;
        let file_len = file.metadata().map_err(TelemetryWalError::Io)?.len();
        if file_len < TELEMETRY_WAL_HEADER_BYTES as u64 {
            return Err(TelemetryWalError::TruncatedHeader);
        }
        if (file_len - TELEMETRY_WAL_HEADER_BYTES as u64) % TELEMETRY_WAL_RECORD_BYTES as u64 != 0 {
            return Err(TelemetryWalError::IncompleteRecord);
        }
        let config = read_header(&mut file)?;
        Ok(Self {
            file,
            config,
            offset: TELEMETRY_WAL_HEADER_BYTES as u64,
            file_len,
            previous: None,
        })
    }

    pub const fn config(&self) -> TelemetryWalConfig {
        self.config
    }

    pub fn next_snapshot(&mut self) -> Result<Option<TelemetrySnapshot>, TelemetryWalError> {
        if self.offset == self.file_len {
            return Ok(None);
        }
        self.file
            .seek(SeekFrom::Start(self.offset))
            .map_err(TelemetryWalError::Io)?;
        let mut bytes = [0_u8; TELEMETRY_WAL_RECORD_BYTES];
        self.file
            .read_exact(&mut bytes)
            .map_err(TelemetryWalError::Io)?;
        let snapshot = decode_record(&bytes, self.offset)?;
        if validate_snapshot(self.previous, snapshot)? != TelemetryAppend::Appended {
            return Err(TelemetryWalError::DuplicateRecord);
        }
        self.previous = Some(snapshot);
        self.offset += TELEMETRY_WAL_RECORD_BYTES as u64;
        Ok(Some(snapshot))
    }
}

fn recover(
    file: &mut File,
    expected: TelemetryWalConfig,
) -> Result<(TelemetryWalRecovery, u64), TelemetryWalError> {
    let file_len = file.metadata().map_err(TelemetryWalError::Io)?.len();
    if file_len < TELEMETRY_WAL_HEADER_BYTES as u64 {
        return Err(TelemetryWalError::TruncatedHeader);
    }
    let actual = read_header(file)?;
    if actual.hardware_id != expected.hardware_id {
        return Err(TelemetryWalError::HardwareMismatch);
    }
    if actual.node_address != expected.node_address {
        return Err(TelemetryWalError::NodeAddressMismatch);
    }
    if actual.boot_epoch != expected.boot_epoch {
        return Err(TelemetryWalError::BootEpochMismatch);
    }
    let payload_len = file_len - TELEMETRY_WAL_HEADER_BYTES as u64;
    let complete_len =
        payload_len / TELEMETRY_WAL_RECORD_BYTES as u64 * TELEMETRY_WAL_RECORD_BYTES as u64;
    let durable_len = TELEMETRY_WAL_HEADER_BYTES as u64 + complete_len;
    let truncated_tail_bytes = file_len - durable_len;
    let mut recovery = TelemetryWalRecovery {
        record_count: 0,
        last_snapshot: None,
        truncated_tail_bytes,
    };
    let mut offset = TELEMETRY_WAL_HEADER_BYTES as u64;
    while offset < durable_len {
        file.seek(SeekFrom::Start(offset))
            .map_err(TelemetryWalError::Io)?;
        let mut bytes = [0_u8; TELEMETRY_WAL_RECORD_BYTES];
        file.read_exact(&mut bytes).map_err(TelemetryWalError::Io)?;
        let snapshot = decode_record(&bytes, offset)?;
        if validate_snapshot(recovery.last_snapshot, snapshot)? != TelemetryAppend::Appended {
            return Err(TelemetryWalError::DuplicateRecord);
        }
        recovery.record_count = recovery
            .record_count
            .checked_add(1)
            .ok_or(TelemetryWalError::LengthOverflow)?;
        recovery.last_snapshot = Some(snapshot);
        offset += TELEMETRY_WAL_RECORD_BYTES as u64;
    }
    if truncated_tail_bytes != 0 {
        file.set_len(durable_len).map_err(TelemetryWalError::Io)?;
        file.sync_data().map_err(TelemetryWalError::Io)?;
    }
    Ok((recovery, durable_len))
}

fn validate_snapshot(
    previous: Option<TelemetrySnapshot>,
    snapshot: TelemetrySnapshot,
) -> Result<TelemetryAppend, TelemetryWalError> {
    if snapshot.captured_device_time_us == 0 {
        return Err(TelemetryWalError::ZeroCapturedTime);
    }
    Adxl355TemperatureCode::new(snapshot.adxl355_temperature.get())
        .map_err(TelemetryWalError::Temperature)?;
    if let Some(previous) = previous {
        if snapshot.captured_device_time_us < previous.captured_device_time_us {
            return Err(TelemetryWalError::NonMonotonicTime);
        }
        if snapshot.captured_device_time_us == previous.captured_device_time_us {
            return if snapshot == previous {
                Ok(TelemetryAppend::Duplicate)
            } else {
                Err(TelemetryWalError::ConflictingSnapshot)
            };
        }
    }
    Ok(TelemetryAppend::Appended)
}

fn write_header(file: &mut File, config: TelemetryWalConfig) -> Result<(), TelemetryWalError> {
    let mut bytes = [0_u8; TELEMETRY_WAL_HEADER_BYTES];
    bytes[0..8].copy_from_slice(&FILE_MAGIC);
    bytes[8..10].copy_from_slice(&FILE_VERSION.to_le_bytes());
    bytes[10..12].copy_from_slice(&(TELEMETRY_WAL_HEADER_BYTES as u16).to_le_bytes());
    bytes[12..14].copy_from_slice(&(TELEMETRY_WAL_RECORD_BYTES as u16).to_le_bytes());
    bytes[16..24].copy_from_slice(&config.hardware_id.bytes());
    bytes[24..32].copy_from_slice(&config.boot_epoch.get().to_le_bytes());
    bytes[32] = config.node_address;
    let crc = crc32c(&bytes[..HEADER_CRC_OFFSET]);
    bytes[HEADER_CRC_OFFSET..].copy_from_slice(&crc.to_le_bytes());
    file.seek(SeekFrom::Start(0))
        .map_err(TelemetryWalError::Io)?;
    file.write_all(&bytes).map_err(TelemetryWalError::Io)
}

fn read_header(file: &mut File) -> Result<TelemetryWalConfig, TelemetryWalError> {
    file.seek(SeekFrom::Start(0))
        .map_err(TelemetryWalError::Io)?;
    let mut bytes = [0_u8; TELEMETRY_WAL_HEADER_BYTES];
    file.read_exact(&mut bytes).map_err(TelemetryWalError::Io)?;
    if bytes[0..8] != FILE_MAGIC {
        return Err(TelemetryWalError::InvalidMagic);
    }
    let version = read_u16(&bytes, 8);
    if version != FILE_VERSION {
        return Err(TelemetryWalError::UnsupportedVersion(version));
    }
    if usize::from(read_u16(&bytes, 10)) != TELEMETRY_WAL_HEADER_BYTES {
        return Err(TelemetryWalError::InvalidHeaderLength);
    }
    if usize::from(read_u16(&bytes, 12)) != TELEMETRY_WAL_RECORD_BYTES {
        return Err(TelemetryWalError::InvalidRecordLength);
    }
    if crc32c(&bytes[..HEADER_CRC_OFFSET]) != read_u32(&bytes, HEADER_CRC_OFFSET) {
        return Err(TelemetryWalError::HeaderChecksumMismatch);
    }
    if bytes[14..16]
        .iter()
        .chain(bytes[33..HEADER_CRC_OFFSET].iter())
        .any(|value| *value != 0)
    {
        return Err(TelemetryWalError::NonZeroReservedField);
    }
    TelemetryWalConfig {
        hardware_id: HardwareId::from_bytes(bytes[16..24].try_into().expect("fixed field")),
        node_address: bytes[32],
        boot_epoch: BootEpoch::new(read_u64(&bytes, 24)),
    }
    .validate()
}

fn encode_record(snapshot: TelemetrySnapshot) -> [u8; TELEMETRY_WAL_RECORD_BYTES] {
    let mut bytes = [0_u8; TELEMETRY_WAL_RECORD_BYTES];
    bytes[0..8].copy_from_slice(&snapshot.captured_device_time_us.to_le_bytes());
    bytes[8..10].copy_from_slice(&snapshot.adxl355_temperature.get().to_le_bytes());
    let crc = crc32c(&bytes[..RECORD_CRC_OFFSET]);
    bytes[RECORD_CRC_OFFSET..].copy_from_slice(&crc.to_le_bytes());
    bytes
}

fn decode_record(
    bytes: &[u8; TELEMETRY_WAL_RECORD_BYTES],
    offset: u64,
) -> Result<TelemetrySnapshot, TelemetryWalError> {
    if crc32c(&bytes[..RECORD_CRC_OFFSET]) != read_u32(bytes, RECORD_CRC_OFFSET) {
        return Err(TelemetryWalError::RecordChecksumMismatch { offset });
    }
    if bytes[10..12].iter().any(|value| *value != 0) {
        return Err(TelemetryWalError::NonZeroReservedField);
    }
    Ok(TelemetrySnapshot {
        captured_device_time_us: read_u64(bytes, 0),
        adxl355_temperature: Adxl355TemperatureCode::new(read_u16(bytes, 8))
            .map_err(TelemetryWalError::Temperature)?,
    })
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

fn sync_parent_directory(path: &Path) -> Result<(), TelemetryWalError> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(TelemetryWalError::Io)
}

#[derive(Debug)]
pub enum TelemetryWalError {
    Io(io::Error),
    Temperature(Adxl355TemperatureCodeError),
    InvalidNodeAddress(u8),
    ZeroBootEpoch,
    ZeroCapturedTime,
    TruncatedHeader,
    IncompleteRecord,
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
    NonMonotonicTime,
    DuplicateRecord,
    ConflictingSnapshot,
    LengthOverflow,
    Poisoned,
}

impl fmt::Display for TelemetryWalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "telemetry WAL error: {self:?}")
    }
}

impl std::error::Error for TelemetryWalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Temperature(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

    struct TempFile(PathBuf);

    impl TempFile {
        fn new() -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!(
                "sensor-telemetry-wal-{}-{id}.wal",
                std::process::id()
            )))
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn config() -> TelemetryWalConfig {
        TelemetryWalConfig {
            hardware_id: HardwareId::from_bytes([7; 8]),
            node_address: 1,
            boot_epoch: BootEpoch::new(9),
        }
    }

    fn snapshot(time: u64, raw: u16) -> TelemetrySnapshot {
        TelemetrySnapshot {
            captured_device_time_us: time,
            adxl355_temperature: Adxl355TemperatureCode::new(raw).unwrap(),
        }
    }

    #[test]
    fn appends_recovers_and_reads_compact_records() {
        let temp = TempFile::new();
        let mut sink = TelemetryWalSink::open(&temp.0, config()).unwrap();
        assert_eq!(
            sink.append(snapshot(100, 1885)).unwrap(),
            TelemetryAppend::Appended
        );
        assert_eq!(
            sink.append(snapshot(200, 1880)).unwrap(),
            TelemetryAppend::Appended
        );
        drop(sink);

        let sink = TelemetryWalSink::open(&temp.0, config()).unwrap();
        assert_eq!(sink.recovery().record_count, 2);
        assert_eq!(sink.recovery().last_snapshot, Some(snapshot(200, 1880)));
        assert_eq!(
            std::fs::metadata(&temp.0).unwrap().len(),
            (TELEMETRY_WAL_HEADER_BYTES + 2 * TELEMETRY_WAL_RECORD_BYTES) as u64
        );
        drop(sink);

        let mut reader = TelemetryWalReader::open(&temp.0).unwrap();
        assert_eq!(reader.config(), config());
        assert_eq!(reader.next_snapshot().unwrap(), Some(snapshot(100, 1885)));
        assert_eq!(reader.next_snapshot().unwrap(), Some(snapshot(200, 1880)));
        assert_eq!(reader.next_snapshot().unwrap(), None);
    }

    #[test]
    fn duplicate_cache_snapshot_is_idempotent_but_conflict_and_regression_fail() {
        let temp = TempFile::new();
        let mut sink = TelemetryWalSink::open(&temp.0, config()).unwrap();
        let first = snapshot(100, 1885);
        assert_eq!(sink.append(first).unwrap(), TelemetryAppend::Appended);
        assert_eq!(sink.append(first).unwrap(), TelemetryAppend::Duplicate);
        assert!(matches!(
            sink.append(snapshot(100, 1884)),
            Err(TelemetryWalError::ConflictingSnapshot)
        ));
        assert!(matches!(
            sink.append(snapshot(99, 1885)),
            Err(TelemetryWalError::NonMonotonicTime)
        ));
        assert_eq!(sink.recovery().record_count, 1);
    }

    #[test]
    fn recovery_truncates_only_an_incomplete_tail() {
        let temp = TempFile::new();
        let mut sink = TelemetryWalSink::open(&temp.0, config()).unwrap();
        sink.append(snapshot(100, 1885)).unwrap();
        drop(sink);
        let mut file = OpenOptions::new().append(true).open(&temp.0).unwrap();
        file.write_all(&[1, 2, 3]).unwrap();
        file.sync_data().unwrap();
        drop(file);

        let sink = TelemetryWalSink::open(&temp.0, config()).unwrap();
        assert_eq!(sink.recovery().truncated_tail_bytes, 3);
        assert_eq!(sink.recovery().record_count, 1);
        assert_eq!(
            std::fs::metadata(&temp.0).unwrap().len(),
            (TELEMETRY_WAL_HEADER_BYTES + TELEMETRY_WAL_RECORD_BYTES) as u64
        );
    }

    #[test]
    fn recovery_rejects_corruption_and_wrong_stream_identity() {
        let temp = TempFile::new();
        let mut sink = TelemetryWalSink::open(&temp.0, config()).unwrap();
        sink.append(snapshot(100, 1885)).unwrap();
        drop(sink);
        let mut file = OpenOptions::new().write(true).open(&temp.0).unwrap();
        file.seek(SeekFrom::Start(TELEMETRY_WAL_HEADER_BYTES as u64 + 1))
            .unwrap();
        file.write_all(&[0xff]).unwrap();
        file.sync_data().unwrap();
        drop(file);
        assert!(matches!(
            TelemetryWalSink::open(&temp.0, config()),
            Err(TelemetryWalError::RecordChecksumMismatch { .. })
        ));

        let other = TempFile::new();
        drop(TelemetryWalSink::open(&other.0, config()).unwrap());
        let mut wrong = config();
        wrong.boot_epoch = BootEpoch::new(10);
        assert!(matches!(
            TelemetryWalSink::open(&other.0, wrong),
            Err(TelemetryWalError::BootEpochMismatch)
        ));
    }
}
