use core::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sensor_core::{BootEpoch, ContinuityBreak, Gap, HardwareId, SequenceCursor};
use sensor_protocol::{crc32c, SampleChunk};

use crate::capture_wal::{
    CaptureRecord, CaptureWalBatch, CaptureWalConfig, CaptureWalError, CaptureWalHeader,
    CaptureWalReader, CaptureWalSink,
};
use crate::capture_window::{
    CaptureWindowError, CaptureWindowLedger, CaptureWindowManifest, WindowSealReason,
};
use crate::measurement_persistence::DurableMeasurementSink;

const MANIFEST_MAGIC: [u8; 8] = *b"SSCAPMAN";
const MANIFEST_VERSION: u16 = 1;
const MANIFEST_BYTES: usize = 104;
const MANIFEST_CRC_OFFSET: usize = MANIFEST_BYTES - 4;
const FIRST_SAMPLE_PRESENT: u8 = 1 << 0;
const LAST_SAMPLE_PRESENT: u8 = 1 << 1;

/// Owns physical Capture segments for one sensor boot stream.
///
/// A segment may contain exactly one logical window. The record that crosses
/// the duration threshold remains in that segment. Rotation happens before the
/// next append, after the sealed manifest is atomically published.
pub struct CaptureSegmentStream {
    directory: PathBuf,
    base_config: CaptureWalConfig,
    current_ordinal: u64,
    current: CaptureWalSink,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureSegmentValidation {
    pub manifest: CaptureWindowManifest,
    pub record_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureSegmentPaths {
    pub wal: PathBuf,
    pub manifest: PathBuf,
}

/// Derives both artifacts from the stream directory and the checkpointed
/// ordinal. Callers never supply independent paths for the WAL and manifest.
pub fn capture_segment_paths(directory: impl AsRef<Path>, ordinal: u64) -> CaptureSegmentPaths {
    let directory = directory.as_ref();
    CaptureSegmentPaths {
        wal: wal_path(directory, ordinal),
        manifest: manifest_path(directory, ordinal),
    }
}

/// Reads only the immutable origin needed to reopen an existing stream.
/// Full chain validation and tail recovery still belong to
/// `CaptureSegmentStream::open`; callers must not treat this header as the
/// current durable cursor.
pub fn inspect_capture_stream_origin(
    directory: impl AsRef<Path>,
) -> Result<Option<CaptureWalHeader>, CaptureSegmentError> {
    let directory = directory.as_ref();
    let ordinals = match discover_segment_ordinals(directory) {
        Ok(value) => value,
        Err(CaptureSegmentError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let Some(first_ordinal) = ordinals.first().copied() else {
        return Ok(None);
    };
    let reader = CaptureWalReader::open(wal_path(directory, first_ordinal))
        .map_err(CaptureSegmentError::Wal)?;
    let header = reader.header();
    if header.first_window_ordinal != first_ordinal {
        return Err(CaptureSegmentError::OriginOrdinalMismatch {
            file: first_ordinal,
            header: header.first_window_ordinal,
        });
    }
    Ok(Some(header))
}

/// Replays a sealed segment independently from the writer and proves that its
/// WAL and published manifest describe exactly the same cursor partition.
pub fn validate_capture_segment(
    wal: impl AsRef<Path>,
    manifest_path: impl AsRef<Path>,
) -> Result<CaptureSegmentValidation, CaptureSegmentError> {
    let manifest = read_capture_manifest(manifest_path.as_ref())?;
    let mut reader = CaptureWalReader::open(wal).map_err(CaptureSegmentError::Wal)?;
    let header = reader.header();
    if header.hardware_id != manifest.hardware_id
        || header.boot_epoch != manifest.boot_epoch
        || header.first_cursor != manifest.first_cursor
        || header.first_window_ordinal != manifest.window_ordinal
    {
        return Err(CaptureSegmentError::WalManifestIdentityMismatch);
    }

    let mut ledger = CaptureWindowLedger::new(
        header.window_policy,
        header.hardware_id,
        header.boot_epoch,
        header.first_cursor,
        header.first_window_ordinal,
    );
    let mut reconstructed = None;
    let mut record_count = 0_u64;
    while let Some(record) = reader.next_record().map_err(CaptureSegmentError::Wal)? {
        if reconstructed.is_some() {
            return Err(CaptureSegmentError::RecordAfterSeal);
        }
        record_count = record_count
            .checked_add(1)
            .ok_or(CaptureSegmentError::CounterOverflow)?;
        reconstructed = match record {
            CaptureRecord::SampleChunk(chunk) => {
                let plan = ledger
                    .plan_chunk(header.hardware_id, &chunk)
                    .map_err(CaptureSegmentError::Window)?;
                ledger.commit(plan).map_err(CaptureSegmentError::Window)?
            }
            CaptureRecord::Gap(gap) => {
                let plan = ledger.plan_gap(gap).map_err(CaptureSegmentError::Window)?;
                ledger.commit(plan).map_err(CaptureSegmentError::Window)?
            }
            CaptureRecord::ContinuityBreak(observed) => {
                let plan = ledger
                    .plan_continuity_break(observed)
                    .map_err(CaptureSegmentError::Window)?;
                ledger.commit(plan).map_err(CaptureSegmentError::Window)?
            }
            CaptureRecord::WindowSeal => ledger.finish().map_err(CaptureSegmentError::Window)?,
        };
    }
    let reconstructed = reconstructed.ok_or(CaptureSegmentError::MissingSeal)?;
    if reconstructed != manifest {
        return Err(CaptureSegmentError::ManifestDoesNotMatchWal);
    }
    Ok(CaptureSegmentValidation {
        manifest,
        record_count,
    })
}

impl CaptureSegmentStream {
    pub fn open(
        directory: impl AsRef<Path>,
        base_config: CaptureWalConfig,
    ) -> Result<Self, CaptureSegmentError> {
        let directory = directory.as_ref().to_path_buf();
        fs::create_dir_all(&directory).map_err(CaptureSegmentError::Io)?;
        sync_directory(&directory)?;
        if let Some(parent) = directory.parent().filter(|value| value.exists()) {
            sync_directory(parent)?;
        }

        let ordinals = discover_segment_ordinals(&directory)?;
        if ordinals.is_empty() {
            let current = CaptureWalSink::open(
                wal_path(&directory, base_config.first_window_ordinal),
                base_config,
            )
            .map_err(CaptureSegmentError::Wal)?;
            return Ok(Self {
                directory,
                base_config,
                current_ordinal: base_config.first_window_ordinal,
                current,
            });
        }

        let mut expected_ordinal = base_config.first_window_ordinal;
        let mut expected_cursor = base_config.recovered_cursor;
        let mut current = None;

        for (index, ordinal) in ordinals.iter().copied().enumerate() {
            if ordinal != expected_ordinal {
                return Err(CaptureSegmentError::NonConsecutiveSegment {
                    expected: expected_ordinal,
                    actual: ordinal,
                });
            }
            let config = CaptureWalConfig {
                recovered_cursor: expected_cursor,
                first_window_ordinal: ordinal,
                ..base_config
            };
            let sink = CaptureWalSink::open(wal_path(&directory, ordinal), config)
                .map_err(CaptureSegmentError::Wal)?;
            let recovery = sink.recovery();
            match recovery.sealed_window_count {
                0 => {
                    if index + 1 != ordinals.len() {
                        return Err(CaptureSegmentError::UnsealedSegmentBeforeSuccessor {
                            ordinal,
                        });
                    }
                    if manifest_path(&directory, ordinal).exists() {
                        return Err(CaptureSegmentError::ManifestForUnsealedSegment { ordinal });
                    }
                    current = Some(sink);
                }
                1 => {
                    let manifest = recovery
                        .last_sealed_window
                        .ok_or(CaptureSegmentError::MissingRecoveredManifest)?;
                    validate_segment_manifest(
                        manifest,
                        ordinal,
                        expected_cursor,
                        recovery.durable_cursor,
                    )?;
                    publish_manifest(&directory, manifest)?;
                    expected_cursor = manifest.end_exclusive;
                    expected_ordinal = expected_ordinal
                        .checked_add(1)
                        .ok_or(CaptureSegmentError::OrdinalOverflow)?;
                    if index + 1 == ordinals.len() {
                        let next_config = CaptureWalConfig {
                            recovered_cursor: expected_cursor,
                            first_window_ordinal: expected_ordinal,
                            ..base_config
                        };
                        current = Some(
                            CaptureWalSink::open(
                                wal_path(&directory, expected_ordinal),
                                next_config,
                            )
                            .map_err(CaptureSegmentError::Wal)?,
                        );
                    }
                }
                count => {
                    return Err(CaptureSegmentError::MultipleWindowsInSegment { ordinal, count });
                }
            }
        }

        Ok(Self {
            directory,
            base_config,
            current_ordinal: expected_ordinal,
            current: current.ok_or(CaptureSegmentError::MissingCurrentSegment)?,
        })
    }

    pub const fn hardware_id(&self) -> HardwareId {
        self.base_config.hardware_id
    }

    pub const fn boot_epoch(&self) -> BootEpoch {
        self.base_config.boot_epoch
    }

    pub fn durable_cursor(&self) -> SequenceCursor {
        self.current.durable_cursor()
    }

    pub fn current_window_ordinal(&self) -> u64 {
        self.current_ordinal
    }

    pub fn finish_window(&mut self) -> Result<Option<CaptureWindowManifest>, CaptureSegmentError> {
        self.rotate_if_needed()?;
        let manifest = self
            .current
            .finish_window()
            .map_err(CaptureSegmentError::Wal)?;
        if let Some(value) = manifest {
            publish_manifest(&self.directory, value)?;
        }
        Ok(manifest)
    }

    /// Starts a durability group in the current physical segment.
    ///
    /// Rotation is completed before the batch is created. The WAL batch itself
    /// rejects records after a record which seals this window.
    pub fn begin_batch(&mut self) -> Result<CaptureWalBatch, CaptureSegmentError> {
        self.rotate_if_needed()?;
        self.current.begin_batch().map_err(CaptureSegmentError::Wal)
    }

    pub fn commit_batch(&mut self, batch: CaptureWalBatch) -> Result<(), CaptureSegmentError> {
        self.current
            .commit_batch(batch)
            .map_err(CaptureSegmentError::Wal)
    }

    #[cfg(test)]
    pub(crate) fn inject_failure_before_next_sync(&mut self) {
        self.current.inject_failure_before_next_sync();
    }

    fn rotate_if_needed(&mut self) -> Result<(), CaptureSegmentError> {
        loop {
            let recovery = self.current.recovery();
            match recovery.sealed_window_count {
                0 => return Ok(()),
                1 => {
                    let manifest = recovery
                        .last_sealed_window
                        .ok_or(CaptureSegmentError::MissingRecoveredManifest)?;
                    validate_segment_manifest(
                        manifest,
                        manifest.window_ordinal,
                        manifest.first_cursor,
                        recovery.durable_cursor,
                    )?;
                    publish_manifest(&self.directory, manifest)?;
                    let next_ordinal = manifest
                        .window_ordinal
                        .checked_add(1)
                        .ok_or(CaptureSegmentError::OrdinalOverflow)?;
                    let config = CaptureWalConfig {
                        recovered_cursor: manifest.end_exclusive,
                        first_window_ordinal: next_ordinal,
                        ..self.base_config
                    };
                    self.current =
                        CaptureWalSink::open(wal_path(&self.directory, next_ordinal), config)
                            .map_err(CaptureSegmentError::Wal)?;
                    self.current_ordinal = next_ordinal;
                }
                count => {
                    return Err(CaptureSegmentError::MultipleWindowsInSegment {
                        ordinal: recovery
                            .last_sealed_window
                            .map_or(self.base_config.first_window_ordinal, |value| {
                                value.window_ordinal
                            }),
                        count,
                    });
                }
            }
        }
    }
}

impl DurableMeasurementSink for CaptureSegmentStream {
    type Error = CaptureSegmentError;

    fn persist_chunk(
        &mut self,
        hardware_id: HardwareId,
        chunk: &SampleChunk,
    ) -> Result<(), Self::Error> {
        self.rotate_if_needed()?;
        self.current
            .persist_chunk(hardware_id, chunk)
            .map_err(CaptureSegmentError::Wal)
    }

    fn persist_gap(&mut self, gap: Gap) -> Result<(), Self::Error> {
        self.rotate_if_needed()?;
        self.current
            .persist_gap(gap)
            .map_err(CaptureSegmentError::Wal)
    }

    fn persist_continuity_break(&mut self, observed: ContinuityBreak) -> Result<(), Self::Error> {
        self.rotate_if_needed()?;
        self.current
            .persist_continuity_break(observed)
            .map_err(CaptureSegmentError::Wal)
    }
}

fn validate_segment_manifest(
    manifest: CaptureWindowManifest,
    ordinal: u64,
    first_cursor: SequenceCursor,
    durable_cursor: SequenceCursor,
) -> Result<(), CaptureSegmentError> {
    if manifest.window_ordinal != ordinal {
        return Err(CaptureSegmentError::ManifestOrdinalMismatch {
            expected: ordinal,
            actual: manifest.window_ordinal,
        });
    }
    if manifest.first_cursor != first_cursor {
        return Err(CaptureSegmentError::ManifestFirstCursorMismatch);
    }
    if manifest.end_exclusive != durable_cursor {
        return Err(CaptureSegmentError::DataAfterSealedWindow { ordinal });
    }
    Ok(())
}

fn discover_segment_ordinals(directory: &Path) -> Result<Vec<u64>, CaptureSegmentError> {
    let mut ordinals = Vec::new();
    for entry in fs::read_dir(directory).map_err(CaptureSegmentError::Io)? {
        let entry = entry.map_err(CaptureSegmentError::Io)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(number) = name
            .strip_prefix("window-")
            .and_then(|value| value.strip_suffix(".wal"))
        else {
            continue;
        };
        if number.len() != 20 || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        ordinals.push(
            number
                .parse::<u64>()
                .map_err(|_| CaptureSegmentError::InvalidSegmentName)?,
        );
    }
    ordinals.sort_unstable();
    if ordinals.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(CaptureSegmentError::DuplicateSegmentOrdinal);
    }
    Ok(ordinals)
}

fn wal_path(directory: &Path, ordinal: u64) -> PathBuf {
    directory.join(format!("window-{ordinal:020}.wal"))
}

fn manifest_path(directory: &Path, ordinal: u64) -> PathBuf {
    directory.join(format!("window-{ordinal:020}.manifest"))
}

fn publish_manifest(
    directory: &Path,
    manifest: CaptureWindowManifest,
) -> Result<(), CaptureSegmentError> {
    let destination = manifest_path(directory, manifest.window_ordinal);
    if destination.exists() {
        let existing = read_capture_manifest(&destination)?;
        return if existing == manifest {
            Ok(())
        } else {
            Err(CaptureSegmentError::ManifestConflict {
                ordinal: manifest.window_ordinal,
            })
        };
    }

    let temporary = directory.join(format!(
        ".window-{:020}.manifest.tmp",
        manifest.window_ordinal
    ));
    let bytes = encode_manifest(manifest);
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(CaptureSegmentError::Io)?;
    file.write_all(&bytes).map_err(CaptureSegmentError::Io)?;
    file.sync_all().map_err(CaptureSegmentError::Io)?;
    fs::rename(&temporary, &destination).map_err(CaptureSegmentError::Io)?;
    sync_directory(directory)
}

fn encode_manifest(manifest: CaptureWindowManifest) -> [u8; MANIFEST_BYTES] {
    let mut output = [0_u8; MANIFEST_BYTES];
    output[0..8].copy_from_slice(&MANIFEST_MAGIC);
    output[8..10].copy_from_slice(&MANIFEST_VERSION.to_le_bytes());
    output[10..12].copy_from_slice(&(MANIFEST_BYTES as u16).to_le_bytes());
    output[12..20].copy_from_slice(&manifest.hardware_id.bytes());
    output[20..28].copy_from_slice(&manifest.boot_epoch.get().to_le_bytes());
    output[28..36].copy_from_slice(&manifest.window_ordinal.to_le_bytes());
    output[36..44].copy_from_slice(&manifest.first_cursor.get().to_le_bytes());
    output[44..52].copy_from_slice(&manifest.end_exclusive.get().to_le_bytes());
    let mut flags = 0_u8;
    if let Some(cursor) = manifest.first_sample_cursor {
        output[52..60].copy_from_slice(&cursor.get().to_le_bytes());
        flags |= FIRST_SAMPLE_PRESENT;
    }
    if let Some(cursor) = manifest.last_sample_cursor {
        output[60..68].copy_from_slice(&cursor.get().to_le_bytes());
        flags |= LAST_SAMPLE_PRESENT;
    }
    output[68..76].copy_from_slice(&manifest.sample_count.to_le_bytes());
    output[76..84].copy_from_slice(&manifest.missing_sample_count.to_le_bytes());
    output[84..92].copy_from_slice(&manifest.continuity_break_count.to_le_bytes());
    output[92] = flags;
    output[93] = encode_seal_reason(manifest.seal_reason);
    let crc = crc32c(&output[..MANIFEST_CRC_OFFSET]);
    output[MANIFEST_CRC_OFFSET..].copy_from_slice(&crc.to_le_bytes());
    output
}

pub fn read_capture_manifest(path: &Path) -> Result<CaptureWindowManifest, CaptureSegmentError> {
    let mut file = File::open(path).map_err(CaptureSegmentError::Io)?;
    let mut bytes = [0_u8; MANIFEST_BYTES];
    file.read_exact(&mut bytes)
        .map_err(CaptureSegmentError::Io)?;
    let mut trailing = [0_u8; 1];
    if file.read(&mut trailing).map_err(CaptureSegmentError::Io)? != 0 {
        return Err(CaptureSegmentError::InvalidManifestLength);
    }
    decode_manifest(&bytes)
}

fn decode_manifest(
    bytes: &[u8; MANIFEST_BYTES],
) -> Result<CaptureWindowManifest, CaptureSegmentError> {
    if bytes[0..8] != MANIFEST_MAGIC {
        return Err(CaptureSegmentError::InvalidManifestMagic);
    }
    let version = read_u16(bytes, 8);
    if version != MANIFEST_VERSION {
        return Err(CaptureSegmentError::UnsupportedManifestVersion(version));
    }
    if usize::from(read_u16(bytes, 10)) != MANIFEST_BYTES {
        return Err(CaptureSegmentError::InvalidManifestLength);
    }
    if crc32c(&bytes[..MANIFEST_CRC_OFFSET]) != read_u32(bytes, MANIFEST_CRC_OFFSET) {
        return Err(CaptureSegmentError::ManifestChecksumMismatch);
    }
    if bytes[94..MANIFEST_CRC_OFFSET]
        .iter()
        .any(|value| *value != 0)
    {
        return Err(CaptureSegmentError::NonZeroManifestReservedField);
    }
    let flags = bytes[92];
    if flags & !(FIRST_SAMPLE_PRESENT | LAST_SAMPLE_PRESENT) != 0 {
        return Err(CaptureSegmentError::UnknownManifestFlags(flags));
    }
    if flags & FIRST_SAMPLE_PRESENT == 0 && read_u64(bytes, 52) != 0 {
        return Err(CaptureSegmentError::InvalidManifestOptionalCursor);
    }
    if flags & LAST_SAMPLE_PRESENT == 0 && read_u64(bytes, 60) != 0 {
        return Err(CaptureSegmentError::InvalidManifestOptionalCursor);
    }
    let manifest = CaptureWindowManifest {
        hardware_id: HardwareId::from_bytes(bytes[12..20].try_into().expect("fixed field")),
        boot_epoch: BootEpoch::new(read_u64(bytes, 20)),
        window_ordinal: read_u64(bytes, 28),
        first_cursor: SequenceCursor::new(read_u64(bytes, 36)),
        end_exclusive: SequenceCursor::new(read_u64(bytes, 44)),
        first_sample_cursor: (flags & FIRST_SAMPLE_PRESENT != 0)
            .then(|| SequenceCursor::new(read_u64(bytes, 52))),
        last_sample_cursor: (flags & LAST_SAMPLE_PRESENT != 0)
            .then(|| SequenceCursor::new(read_u64(bytes, 60))),
        sample_count: read_u64(bytes, 68),
        missing_sample_count: read_u64(bytes, 76),
        continuity_break_count: read_u64(bytes, 84),
        seal_reason: decode_seal_reason(bytes[93])?,
    };
    validate_manifest_contract(manifest)?;
    Ok(manifest)
}

fn validate_manifest_contract(manifest: CaptureWindowManifest) -> Result<(), CaptureSegmentError> {
    if manifest.boot_epoch.get() == 0 || manifest.end_exclusive <= manifest.first_cursor {
        return Err(CaptureSegmentError::InvalidManifestContract);
    }
    let covered = manifest
        .sample_count
        .checked_add(manifest.missing_sample_count)
        .and_then(|value| value.checked_add(manifest.continuity_break_count))
        .ok_or(CaptureSegmentError::InvalidManifestContract)?;
    if manifest.first_cursor.checked_advance(covered) != Some(manifest.end_exclusive) {
        return Err(CaptureSegmentError::InvalidManifestContract);
    }
    match (
        manifest.sample_count,
        manifest.first_sample_cursor,
        manifest.last_sample_cursor,
    ) {
        (0, None, None) => {}
        (0, _, _) | (_, None, _) | (_, _, None) => {
            return Err(CaptureSegmentError::InvalidManifestContract);
        }
        (_, Some(first), Some(last))
            if first < manifest.first_cursor || last < first || last >= manifest.end_exclusive =>
        {
            return Err(CaptureSegmentError::InvalidManifestContract);
        }
        _ => {}
    }
    Ok(())
}

fn encode_seal_reason(reason: WindowSealReason) -> u8 {
    match reason {
        WindowSealReason::TargetDurationReached => 0,
        WindowSealReason::TimingSegmentChanged => 1,
        WindowSealReason::ExplicitFinish => 2,
    }
}

fn decode_seal_reason(value: u8) -> Result<WindowSealReason, CaptureSegmentError> {
    match value {
        0 => Ok(WindowSealReason::TargetDurationReached),
        1 => Ok(WindowSealReason::TimingSegmentChanged),
        2 => Ok(WindowSealReason::ExplicitFinish),
        _ => Err(CaptureSegmentError::UnknownSealReason(value)),
    }
}

fn sync_directory(path: &Path) -> Result<(), CaptureSegmentError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(CaptureSegmentError::Io)
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

#[derive(Debug)]
pub enum CaptureSegmentError {
    Io(io::Error),
    Wal(CaptureWalError),
    Window(CaptureWindowError),
    InvalidSegmentName,
    DuplicateSegmentOrdinal,
    OriginOrdinalMismatch { file: u64, header: u64 },
    NonConsecutiveSegment { expected: u64, actual: u64 },
    UnsealedSegmentBeforeSuccessor { ordinal: u64 },
    ManifestForUnsealedSegment { ordinal: u64 },
    MultipleWindowsInSegment { ordinal: u64, count: u64 },
    MissingRecoveredManifest,
    MissingCurrentSegment,
    ManifestOrdinalMismatch { expected: u64, actual: u64 },
    ManifestFirstCursorMismatch,
    DataAfterSealedWindow { ordinal: u64 },
    OrdinalOverflow,
    ManifestConflict { ordinal: u64 },
    InvalidManifestMagic,
    UnsupportedManifestVersion(u16),
    InvalidManifestLength,
    ManifestChecksumMismatch,
    NonZeroManifestReservedField,
    UnknownManifestFlags(u8),
    InvalidManifestOptionalCursor,
    InvalidManifestContract,
    UnknownSealReason(u8),
    WalManifestIdentityMismatch,
    RecordAfterSeal,
    MissingSeal,
    ManifestDoesNotMatchWal,
    CounterOverflow,
}

impl fmt::Display for CaptureSegmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "capture segment I/O failed: {error}"),
            Self::Wal(error) => write!(formatter, "capture segment WAL failed: {error}"),
            Self::Window(error) => write!(formatter, "capture segment ordering failed: {error}"),
            other => write!(formatter, "capture segment error: {other:?}"),
        }
    }
}

impl std::error::Error for CaptureSegmentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Wal(error) => Some(error),
            Self::Window(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    use sensor_core::{SampleAxes, SampleAxis};
    use sensor_protocol::{SampleChunkHeader, TimingEvidence, TIMING_VALID};

    use crate::capture_window::{CaptureWindowPolicy, CaptureWindowRelation};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sensor-host-capture-segments-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const HARDWARE: HardwareId = HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);
    const BOOT: BootEpoch = BootEpoch::new(9);

    fn config() -> CaptureWalConfig {
        CaptureWalConfig {
            hardware_id: HARDWARE,
            boot_epoch: BOOT,
            recovered_cursor: SequenceCursor::ORIGIN,
            first_window_ordinal: 0,
            window_policy: CaptureWindowPolicy::new(100).unwrap(),
        }
    }

    fn chunk(first: u64, first_time: u64, last_time: u64) -> SampleChunk {
        let axis = SampleAxis::new(i32::try_from(first).unwrap()).unwrap();
        SampleChunk {
            header: SampleChunkHeader {
                boot_epoch: BOOT,
                retained_from: SequenceCursor::new(first),
                produced: SequenceCursor::new(first + 1),
                first: SequenceCursor::new(first),
                sample_count: 1,
            },
            config_revision: 1,
            timing: TimingEvidence {
                segment_id: 1,
                first_device_time_us: first_time,
                last_device_time_us: last_time,
                max_fit_residual_us: 1,
                quality_flags: TIMING_VALID,
            },
            samples: vec![SampleAxes::new(axis, axis, axis)],
        }
    }

    #[test]
    fn rotates_before_the_first_record_of_the_next_window() {
        let temp = TempDirectory::new();
        let mut stream = CaptureSegmentStream::open(&temp.0, config()).unwrap();
        stream.persist_chunk(HARDWARE, &chunk(0, 0, 0)).unwrap();
        stream.persist_chunk(HARDWARE, &chunk(1, 100, 100)).unwrap();
        assert_eq!(stream.current_window_ordinal(), 0);
        assert!(!manifest_path(&temp.0, 0).exists());

        stream.persist_chunk(HARDWARE, &chunk(2, 200, 200)).unwrap();
        assert_eq!(stream.current_window_ordinal(), 1);
        assert!(manifest_path(&temp.0, 0).exists());
        let first = read_capture_manifest(&manifest_path(&temp.0, 0)).unwrap();
        assert_eq!(first.first_cursor, SequenceCursor::ORIGIN);
        assert_eq!(first.end_exclusive, SequenceCursor::new(2));

        let second = stream.finish_window().unwrap().unwrap();
        assert_eq!(
            first.relation_to(second),
            Ok(CaptureWindowRelation::Contiguous)
        );
        assert!(manifest_path(&temp.0, 1).exists());
        let first_validation =
            validate_capture_segment(wal_path(&temp.0, 0), manifest_path(&temp.0, 0)).unwrap();
        assert_eq!(first_validation.manifest, first);
        assert_eq!(first_validation.record_count, 2);
        assert_eq!(
            validate_capture_segment(wal_path(&temp.0, 1), manifest_path(&temp.0, 1))
                .unwrap()
                .manifest,
            second
        );
        drop(stream);

        let recovered = CaptureSegmentStream::open(&temp.0, config()).unwrap();
        assert_eq!(recovered.current_window_ordinal(), 2);
        assert_eq!(recovered.durable_cursor(), SequenceCursor::new(3));
        assert!(wal_path(&temp.0, 2).exists());
    }

    #[test]
    fn origin_inspection_bootstraps_recovery_without_guessing_the_first_cursor() {
        let temp = TempDirectory::new();
        assert_eq!(inspect_capture_stream_origin(&temp.0).unwrap(), None);

        let mut initial = config();
        initial.recovered_cursor = SequenceCursor::new(500);
        initial.first_window_ordinal = 7;
        let stream = CaptureSegmentStream::open(&temp.0, initial).unwrap();
        drop(stream);

        let origin = inspect_capture_stream_origin(&temp.0).unwrap().unwrap();
        assert_eq!(origin.hardware_id, HARDWARE);
        assert_eq!(origin.boot_epoch, BOOT);
        assert_eq!(origin.first_cursor, SequenceCursor::new(500));
        assert_eq!(origin.first_window_ordinal, 7);
        assert_eq!(origin.window_policy, initial.window_policy);
    }

    #[test]
    fn missing_stream_directory_has_no_recovery_origin() {
        let temp = TempDirectory::new();
        let missing = temp.0.join("not-created");
        assert_eq!(inspect_capture_stream_origin(missing).unwrap(), None);
    }

    #[test]
    fn restart_rebuilds_a_manifest_after_crash_between_seal_and_publish() {
        let temp = TempDirectory::new();
        let mut wal = CaptureWalSink::open(wal_path(&temp.0, 0), config()).unwrap();
        wal.persist_chunk(HARDWARE, &chunk(0, 0, 0)).unwrap();
        wal.persist_chunk(HARDWARE, &chunk(1, 100, 100)).unwrap();
        assert_eq!(wal.recovery().sealed_window_count, 1);
        drop(wal);
        assert!(!manifest_path(&temp.0, 0).exists());

        let recovered = CaptureSegmentStream::open(&temp.0, config()).unwrap();
        assert!(manifest_path(&temp.0, 0).exists());
        assert_eq!(recovered.current_window_ordinal(), 1);
        assert_eq!(recovered.durable_cursor(), SequenceCursor::new(2));
    }

    #[test]
    fn manifest_codec_preserves_every_contract_field() {
        let manifest = CaptureWindowManifest {
            hardware_id: HARDWARE,
            boot_epoch: BOOT,
            window_ordinal: 7,
            first_cursor: SequenceCursor::new(10),
            end_exclusive: SequenceCursor::new(20),
            first_sample_cursor: Some(SequenceCursor::new(12)),
            last_sample_cursor: Some(SequenceCursor::new(19)),
            sample_count: 8,
            missing_sample_count: 1,
            continuity_break_count: 1,
            seal_reason: WindowSealReason::ExplicitFinish,
        };
        assert_eq!(
            decode_manifest(&encode_manifest(manifest)).unwrap(),
            manifest
        );
    }

    #[test]
    fn refuses_an_unsealed_segment_with_a_successor() {
        let temp = TempDirectory::new();
        drop(CaptureWalSink::open(wal_path(&temp.0, 0), config()).unwrap());
        let second_config = CaptureWalConfig {
            first_window_ordinal: 1,
            ..config()
        };
        drop(CaptureWalSink::open(wal_path(&temp.0, 1), second_config).unwrap());

        assert!(matches!(
            CaptureSegmentStream::open(&temp.0, config()),
            Err(CaptureSegmentError::UnsealedSegmentBeforeSuccessor { ordinal: 0 })
        ));
    }

    #[test]
    fn refuses_a_conflicting_or_corrupt_published_manifest() {
        let temp = TempDirectory::new();
        let mut stream = CaptureSegmentStream::open(&temp.0, config()).unwrap();
        stream.persist_chunk(HARDWARE, &chunk(0, 0, 0)).unwrap();
        let manifest = stream.finish_window().unwrap().unwrap();
        drop(stream);

        let path = manifest_path(&temp.0, manifest.window_ordinal);
        let mut bytes = fs::read(&path).unwrap();
        bytes[68] ^= 1;
        fs::write(&path, bytes).unwrap();
        assert!(matches!(
            CaptureSegmentStream::open(&temp.0, config()),
            Err(CaptureSegmentError::ManifestChecksumMismatch)
        ));
    }

    #[test]
    fn independent_validator_rejects_a_valid_but_false_manifest() {
        let temp = TempDirectory::new();
        let mut stream = CaptureSegmentStream::open(&temp.0, config()).unwrap();
        stream.persist_chunk(HARDWARE, &chunk(0, 0, 0)).unwrap();
        let manifest = stream.finish_window().unwrap().unwrap();
        drop(stream);

        let false_manifest = CaptureWindowManifest {
            seal_reason: WindowSealReason::TargetDurationReached,
            ..manifest
        };
        fs::write(manifest_path(&temp.0, 0), encode_manifest(false_manifest)).unwrap();
        assert!(matches!(
            validate_capture_segment(wal_path(&temp.0, 0), manifest_path(&temp.0, 0)),
            Err(CaptureSegmentError::ManifestDoesNotMatchWal)
        ));
    }
}
