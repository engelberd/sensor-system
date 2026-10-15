use core::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use sensor_core::{BootEpoch, HardwareId};
use sensor_protocol::{
    crc32c, decode_configuration, encode_configuration, Adxl355Odr, ObservedConfiguration,
};

const MAGIC: [u8; 4] = *b"CFG1";
const SCHEMA_VERSION: u8 = 1;
const RECORD_BYTES: usize = 56;
const CRC_OFFSET: usize = 52;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigurationSnapshot {
    pub hardware_id: HardwareId,
    pub boot_epoch: BootEpoch,
    pub observed: ObservedConfiguration,
    pub effective_sample_seq: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigurationAppend {
    Appended,
    Existing,
}

/// Reads and validates the complete durable configuration history for one
/// sensor without modifying acquisition data.
pub fn read_configuration_snapshots(
    capture_root: &Path,
    hardware_id: HardwareId,
) -> Result<Vec<ConfigurationSnapshot>, ConfigurationHistoryError> {
    let path = capture_root
        .join(".configuration-v1")
        .join(format!("{}.wal", hardware_id_hex(hardware_id)));
    let mut file = File::open(path).map_err(ConfigurationHistoryError::Io)?;
    let length = file
        .metadata()
        .map_err(ConfigurationHistoryError::Io)?
        .len();
    if length % RECORD_BYTES as u64 != 0 {
        return Err(ConfigurationHistoryError::IncompleteRecord);
    }
    let record_count = usize::try_from(length / RECORD_BYTES as u64)
        .map_err(|_| ConfigurationHistoryError::RecordCountOverflow)?;
    let mut snapshots = Vec::with_capacity(record_count);
    let mut record = [0_u8; RECORD_BYTES];
    while snapshots.len() < record_count {
        file.read_exact(&mut record)
            .map_err(ConfigurationHistoryError::Io)?;
        let snapshot = decode_record(&record)?;
        if snapshot.hardware_id != hardware_id {
            return Err(ConfigurationHistoryError::InvalidRecord);
        }
        snapshots.push(snapshot);
    }
    Ok(snapshots)
}

/// Appends one durable, fixed-size configuration snapshot outside the sample
/// hot path. A torn final record is removed during recovery; corruption of a
/// complete record fails closed.
pub fn append_configuration_snapshot(
    capture_root: &Path,
    snapshot: ConfigurationSnapshot,
) -> Result<ConfigurationAppend, ConfigurationHistoryError> {
    snapshot
        .observed
        .validate()
        .map_err(|_| ConfigurationHistoryError::InvalidSnapshot)?;
    if snapshot.boot_epoch.get() == 0 {
        return Err(ConfigurationHistoryError::InvalidSnapshot);
    }
    let directory = capture_root.join(".configuration-v1");
    fs::create_dir_all(&directory).map_err(ConfigurationHistoryError::Io)?;
    let path = directory.join(format!("{}.wal", hardware_id_hex(snapshot.hardware_id)));
    let existed = path.exists();
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(&path)
        .map_err(ConfigurationHistoryError::Io)?;
    if !existed {
        sync_directory(&directory)?;
    }
    recover_tail(&mut file)?;
    file.seek(SeekFrom::Start(0))
        .map_err(ConfigurationHistoryError::Io)?;
    let mut record = [0_u8; RECORD_BYTES];
    loop {
        match file.read_exact(&mut record) {
            Ok(()) => {
                let existing = decode_record(&record)?;
                if existing.boot_epoch == snapshot.boot_epoch
                    && existing.observed.revision == snapshot.observed.revision
                {
                    return if existing.hardware_id == snapshot.hardware_id
                        && existing.observed == snapshot.observed
                    {
                        Ok(ConfigurationAppend::Existing)
                    } else {
                        Err(ConfigurationHistoryError::ConflictingRevision {
                            boot_epoch: snapshot.boot_epoch,
                            revision: snapshot.observed.revision,
                        })
                    };
                }
            }
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(ConfigurationHistoryError::Io(error)),
        }
    }
    let encoded = encode_record(snapshot)?;
    file.seek(SeekFrom::End(0))
        .map_err(ConfigurationHistoryError::Io)?;
    file.write_all(&encoded)
        .map_err(ConfigurationHistoryError::Io)?;
    file.sync_data().map_err(ConfigurationHistoryError::Io)?;
    Ok(ConfigurationAppend::Appended)
}

fn recover_tail(file: &mut File) -> Result<(), ConfigurationHistoryError> {
    let length = file
        .metadata()
        .map_err(ConfigurationHistoryError::Io)?
        .len();
    let complete = length - length % RECORD_BYTES as u64;
    if complete != length {
        file.set_len(complete)
            .map_err(ConfigurationHistoryError::Io)?;
        file.sync_data().map_err(ConfigurationHistoryError::Io)?;
    }
    Ok(())
}

fn encode_record(
    snapshot: ConfigurationSnapshot,
) -> Result<[u8; RECORD_BYTES], ConfigurationHistoryError> {
    let mut out = [0_u8; RECORD_BYTES];
    out[0..4].copy_from_slice(&MAGIC);
    out[4] = SCHEMA_VERSION;
    out[8..16].copy_from_slice(&snapshot.hardware_id.bytes());
    out[16..24].copy_from_slice(&snapshot.boot_epoch.get().to_le_bytes());
    out[24..28].copy_from_slice(&snapshot.observed.revision.to_le_bytes());
    out[28..36].copy_from_slice(&snapshot.effective_sample_seq.to_le_bytes());
    out[36..48].copy_from_slice(
        &encode_configuration(snapshot.observed.desired)
            .map_err(|_| ConfigurationHistoryError::InvalidSnapshot)?,
    );
    out[48] = snapshot.observed.sensor_odr.register_value();
    out[49] = snapshot.observed.decimation_factor;
    let checksum = crc32c(&out[..CRC_OFFSET]);
    out[CRC_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
    Ok(out)
}

fn decode_record(
    bytes: &[u8; RECORD_BYTES],
) -> Result<ConfigurationSnapshot, ConfigurationHistoryError> {
    if bytes[0..4] != MAGIC || bytes[4] != SCHEMA_VERSION || bytes[5..8] != [0; 3] {
        return Err(ConfigurationHistoryError::InvalidRecord);
    }
    if crc32c(&bytes[..CRC_OFFSET]) != u32::from_le_bytes(bytes[CRC_OFFSET..].try_into().unwrap()) {
        return Err(ConfigurationHistoryError::ChecksumMismatch);
    }
    if bytes[50..52] != [0; 2] {
        return Err(ConfigurationHistoryError::InvalidRecord);
    }
    let boot_epoch = BootEpoch::new(u64::from_le_bytes(bytes[16..24].try_into().unwrap()));
    let observed = ObservedConfiguration {
        revision: u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
        desired: decode_configuration(&bytes[36..48])
            .map_err(|_| ConfigurationHistoryError::InvalidRecord)?,
        sensor_odr: Adxl355Odr::from_register_value(bytes[48])
            .ok_or(ConfigurationHistoryError::InvalidRecord)?,
        decimation_factor: bytes[49],
    };
    let snapshot = ConfigurationSnapshot {
        hardware_id: HardwareId::from_bytes(bytes[8..16].try_into().unwrap()),
        boot_epoch,
        observed,
        effective_sample_seq: u64::from_le_bytes(bytes[28..36].try_into().unwrap()),
    };
    if boot_epoch.get() == 0 || snapshot.observed.validate().is_err() {
        return Err(ConfigurationHistoryError::InvalidRecord);
    }
    Ok(snapshot)
}

fn sync_directory(path: &Path) -> Result<(), ConfigurationHistoryError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(ConfigurationHistoryError::Io)
}

fn hardware_id_hex(hardware_id: HardwareId) -> String {
    use core::fmt::Write as _;
    let mut output = String::with_capacity(16);
    for byte in hardware_id.bytes() {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

#[derive(Debug)]
pub enum ConfigurationHistoryError {
    Io(io::Error),
    InvalidSnapshot,
    InvalidRecord,
    ChecksumMismatch,
    IncompleteRecord,
    RecordCountOverflow,
    ConflictingRevision {
        boot_epoch: BootEpoch,
        revision: u32,
    },
}

impl fmt::Display for ConfigurationHistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "configuration history error: {self:?}")
    }
}

impl std::error::Error for ConfigurationHistoryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_protocol::{DecimationFilterProfile, MeasurementConfiguration, OutputRateCode};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    fn root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "sensor-configuration-history-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn snapshot(revision: u32) -> ConfigurationSnapshot {
        ConfigurationSnapshot {
            hardware_id: HardwareId::from_bytes([1; 8]),
            boot_epoch: BootEpoch::new(9),
            observed: ObservedConfiguration {
                revision,
                desired: MeasurementConfiguration {
                    output_rate: OutputRateCode::Hz125,
                    range_g: 2,
                    high_pass_corner: 0,
                    filter_profile: DecimationFilterProfile::Balanced,
                    fifo_watermark: 30,
                    offset_x: 0,
                    offset_y: 0,
                    offset_z: 0,
                },
                sensor_odr: Adxl355Odr::Hz125,
                decimation_factor: 1,
            },
            effective_sample_seq: 100,
        }
    }

    #[test]
    fn append_is_durable_idempotent_and_rejects_conflicting_revision() {
        let root = root();
        assert_eq!(
            append_configuration_snapshot(&root, snapshot(1)).unwrap(),
            ConfigurationAppend::Appended
        );
        assert_eq!(
            append_configuration_snapshot(&root, snapshot(1)).unwrap(),
            ConfigurationAppend::Existing
        );
        let mut conflict = snapshot(1);
        conflict.observed.desired.offset_x = 1;
        assert!(matches!(
            append_configuration_snapshot(&root, conflict),
            Err(ConfigurationHistoryError::ConflictingRevision { .. })
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_removes_only_a_torn_tail() {
        let root = root();
        append_configuration_snapshot(&root, snapshot(1)).unwrap();
        let path = root.join(".configuration-v1/0101010101010101.wal");
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&[1, 2, 3])
            .unwrap();
        assert_eq!(
            append_configuration_snapshot(&root, snapshot(2)).unwrap(),
            ConfigurationAppend::Appended
        );
        assert_eq!(fs::metadata(path).unwrap().len(), (RECORD_BYTES * 2) as u64);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn read_only_history_returns_validated_snapshots() {
        let root = root();
        append_configuration_snapshot(&root, snapshot(1)).unwrap();
        append_configuration_snapshot(&root, snapshot(2)).unwrap();
        let values = read_configuration_snapshots(&root, HardwareId::from_bytes([1; 8])).unwrap();
        assert_eq!(values, vec![snapshot(1), snapshot(2)]);
        fs::remove_dir_all(root).unwrap();
    }
}
