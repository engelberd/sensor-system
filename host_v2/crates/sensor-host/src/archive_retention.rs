use core::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sensor_core::{BootEpoch, HardwareId, SequenceCursor};
use sensor_protocol::crc32c;

use crate::archive_coverage::{
    read_archive_coverage_index, ArchiveCoverageError, ArchiveCoverageIndex,
};
use crate::capture_coverage_checkpoint::{
    CaptureCoverageCheckpointError, CaptureCoverageCheckpointStore,
};
use crate::retention_checkpoint::{
    ArchiveIdentity, RetentionPolicy, SegmentKey, TrustedUnixSeconds,
};

const MAGIC: [u8; 8] = *b"SSARCCP1";
const VERSION: u16 = 1;
const HEADER_BYTES: usize = 112;
const DEPENDENCY_BYTES: usize = 40;
const CRC_BYTES: usize = 4;
const MAX_DEPENDENCIES: usize = 1024;
const DELETED: u32 = 1 << 0;
const KNOWN_FLAGS: u32 = DELETED;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveRetentionState {
    archive: ArchiveIdentity,
    dependencies: Vec<SegmentKey>,
    verified_at: TrustedUnixSeconds,
    acknowledged_at: TrustedUnixSeconds,
    delete_after: TrustedUnixSeconds,
    deleted_at: Option<TrustedUnixSeconds>,
}

impl ArchiveRetentionState {
    pub fn from_verified_replication(
        index: &ArchiveCoverageIndex,
        verified_at: TrustedUnixSeconds,
        acknowledged_at: TrustedUnixSeconds,
        policy: RetentionPolicy,
    ) -> Result<Self, ArchiveRetentionError> {
        index.validate().map_err(ArchiveRetentionError::Coverage)?;
        if acknowledged_at < verified_at {
            return Err(ArchiveRetentionError::AcknowledgementBeforeVerification);
        }
        if policy.archive_retention_seconds == 0 {
            return Err(ArchiveRetentionError::ZeroRetentionDuration);
        }
        let delete_after = acknowledged_at
            .get()
            .checked_add(policy.archive_retention_seconds)
            .map(TrustedUnixSeconds::new)
            .ok_or(ArchiveRetentionError::TimestampOverflow)?;
        let mut dependencies = Vec::new();
        for input in &index.inputs {
            if !dependencies.contains(&input.segment) {
                dependencies.push(input.segment);
            }
        }
        dependencies.sort_unstable_by_key(segment_sort_key);
        validate_dependencies(&dependencies)?;
        Ok(Self {
            archive: index.archive,
            dependencies,
            verified_at,
            acknowledged_at,
            delete_after,
            deleted_at: None,
        })
    }

    pub const fn archive(&self) -> ArchiveIdentity {
        self.archive
    }

    pub fn dependencies(&self) -> &[SegmentKey] {
        &self.dependencies
    }

    pub const fn verified_at(&self) -> TrustedUnixSeconds {
        self.verified_at
    }

    pub const fn acknowledged_at(&self) -> TrustedUnixSeconds {
        self.acknowledged_at
    }

    pub const fn delete_after(&self) -> TrustedUnixSeconds {
        self.delete_after
    }

    pub const fn deleted_at(&self) -> Option<TrustedUnixSeconds> {
        self.deleted_at
    }

    pub fn deadline_elapsed(&self, now: TrustedUnixSeconds) -> bool {
        self.deleted_at.is_none() && now >= self.delete_after
    }

    fn mark_deleted(&mut self, at: TrustedUnixSeconds) -> Result<bool, ArchiveRetentionError> {
        if let Some(existing) = self.deleted_at {
            return if existing == at {
                Ok(false)
            } else {
                Err(ArchiveRetentionError::ConflictingDeletionTime)
            };
        }
        if !self.deadline_elapsed(at) {
            return Err(ArchiveRetentionError::ArchiveNotDeletable);
        }
        self.deleted_at = Some(at);
        Ok(true)
    }
}

pub struct ArchiveRetentionCheckpointStore {
    path: PathBuf,
    state: ArchiveRetentionState,
}

impl ArchiveRetentionCheckpointStore {
    pub fn create(
        path: impl AsRef<Path>,
        index: &ArchiveCoverageIndex,
        verified_at: TrustedUnixSeconds,
        acknowledged_at: TrustedUnixSeconds,
        policy: RetentionPolicy,
    ) -> Result<Self, ArchiveRetentionError> {
        let path = path.as_ref().to_path_buf();
        let state = ArchiveRetentionState::from_verified_replication(
            index,
            verified_at,
            acknowledged_at,
            policy,
        )?;
        if path.exists() {
            let existing = Self::open(&path)?;
            return if existing.state == state {
                Ok(existing)
            } else {
                Err(ArchiveRetentionError::AlreadyExists)
            };
        }
        persist_atomic(&path, &state)?;
        Ok(Self { path, state })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, ArchiveRetentionError> {
        let path = path.as_ref().to_path_buf();
        let state = read_checkpoint(&path)?;
        Ok(Self { path, state })
    }

    pub fn state(&self) -> &ArchiveRetentionState {
        &self.state
    }

    fn mark_deleted(&mut self, at: TrustedUnixSeconds) -> Result<(), ArchiveRetentionError> {
        let mut next = self.state.clone();
        if next.mark_deleted(at)? {
            persist_atomic(&self.path, &next)?;
            self.state = next;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveCleanupTarget {
    pub archive: PathBuf,
    pub coverage_index: PathBuf,
    pub checkpoint: PathBuf,
    /// Coverage checkpoints for every unique Capture segment recorded in the
    /// Archive checkpoint. Their paths are layout policy, their contents are
    /// the trusted identity and deletion evidence.
    pub capture_checkpoints: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveCleanupOutcome {
    NotDue,
    WaitingForCaptureCleanup,
    Deleted,
    AlreadyRecorded,
}

pub fn cleanup_archive_product(
    target: &ArchiveCleanupTarget,
    now: TrustedUnixSeconds,
) -> Result<ArchiveCleanupOutcome, ArchiveRetentionError> {
    cleanup_archive_product_inner(target, now, None)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FaultPoint {
    AfterArchiveRemoval,
    AfterDirectorySync,
}

fn cleanup_archive_product_inner(
    target: &ArchiveCleanupTarget,
    now: TrustedUnixSeconds,
    fault: Option<FaultPoint>,
) -> Result<ArchiveCleanupOutcome, ArchiveRetentionError> {
    let mut checkpoint = ArchiveRetentionCheckpointStore::open(&target.checkpoint)?;
    if checkpoint.state().deleted_at().is_some() {
        if target.archive.exists() || target.coverage_index.exists() {
            return Err(ArchiveRetentionError::ArtifactsPresentAfterRecordedDeletion);
        }
        return Ok(ArchiveCleanupOutcome::AlreadyRecorded);
    }
    if !checkpoint.state().deadline_elapsed(now) {
        return Ok(ArchiveCleanupOutcome::NotDue);
    }
    if !capture_dependencies_deleted(checkpoint.state(), &target.capture_checkpoints)? {
        return Ok(ArchiveCleanupOutcome::WaitingForCaptureCleanup);
    }
    let artifact_directory = common_parent(&target.archive, &target.coverage_index)?;

    match (target.archive.exists(), target.coverage_index.exists()) {
        (true, true) => {
            let index = read_archive_coverage_index(&target.coverage_index)
                .map_err(ArchiveRetentionError::Coverage)?;
            require_index(checkpoint.state(), &index)?;
        }
        (false, true) => {
            let index = read_archive_coverage_index(&target.coverage_index)
                .map_err(ArchiveRetentionError::Coverage)?;
            require_index(checkpoint.state(), &index)?;
        }
        (true, false) => return Err(ArchiveRetentionError::ArchiveWithoutCoverageIndex),
        (false, false) => {}
    }

    remove_if_present(&target.archive)?;
    if fault == Some(FaultPoint::AfterArchiveRemoval) {
        return Err(ArchiveRetentionError::InjectedFault);
    }
    remove_if_present(&target.coverage_index)?;
    sync_directory(artifact_directory)?;
    if fault == Some(FaultPoint::AfterDirectorySync) {
        return Err(ArchiveRetentionError::InjectedFault);
    }
    checkpoint.mark_deleted(now)?;
    Ok(ArchiveCleanupOutcome::Deleted)
}

fn capture_dependencies_deleted(
    state: &ArchiveRetentionState,
    paths: &[PathBuf],
) -> Result<bool, ArchiveRetentionError> {
    if paths.len() != state.dependencies.len() {
        return Err(ArchiveRetentionError::CaptureCheckpointSetMismatch);
    }
    let mut observed = Vec::with_capacity(paths.len());
    let mut all_deleted = true;
    for path in paths {
        let checkpoint = CaptureCoverageCheckpointStore::open(path)
            .map_err(ArchiveRetentionError::CaptureCheckpoint)?;
        let segment = checkpoint.state().segment();
        if observed.contains(&segment) || !state.dependencies.contains(&segment) {
            return Err(ArchiveRetentionError::CaptureCheckpointSetMismatch);
        }
        all_deleted &= checkpoint.state().deleted_at().is_some();
        observed.push(segment);
    }
    Ok(observed.len() == state.dependencies.len() && all_deleted)
}

fn require_index(
    state: &ArchiveRetentionState,
    index: &ArchiveCoverageIndex,
) -> Result<(), ArchiveRetentionError> {
    if index.archive != state.archive {
        return Err(ArchiveRetentionError::ArchiveIdentityMismatch);
    }
    let mut dependencies = Vec::new();
    for input in &index.inputs {
        if !dependencies.contains(&input.segment) {
            dependencies.push(input.segment);
        }
    }
    dependencies.sort_unstable_by_key(segment_sort_key);
    if dependencies != state.dependencies {
        return Err(ArchiveRetentionError::ArchiveDependenciesMismatch);
    }
    Ok(())
}

fn validate_dependencies(dependencies: &[SegmentKey]) -> Result<(), ArchiveRetentionError> {
    if dependencies.is_empty() {
        return Err(ArchiveRetentionError::NoCaptureDependencies);
    }
    if dependencies.len() > MAX_DEPENDENCIES {
        return Err(ArchiveRetentionError::TooManyCaptureDependencies);
    }
    for (position, segment) in dependencies.iter().copied().enumerate() {
        if segment.hardware_id.bytes() == [0; 8]
            || segment.boot_epoch.get() == 0
            || segment.end_exclusive <= segment.first_cursor
        {
            return Err(ArchiveRetentionError::InvalidCaptureDependency);
        }
        if dependencies[..position].contains(&segment) {
            return Err(ArchiveRetentionError::DuplicateCaptureDependency);
        }
    }
    Ok(())
}

fn segment_sort_key(segment: &SegmentKey) -> ([u8; 8], u64, u64, u64, u64) {
    (
        segment.hardware_id.bytes(),
        segment.boot_epoch.get(),
        segment.window_ordinal,
        segment.first_cursor.get(),
        segment.end_exclusive.get(),
    )
}

fn encoded_length(dependency_count: usize) -> Result<usize, ArchiveRetentionError> {
    if dependency_count == 0 || dependency_count > MAX_DEPENDENCIES {
        return Err(ArchiveRetentionError::InvalidLength);
    }
    HEADER_BYTES
        .checked_add(
            dependency_count
                .checked_mul(DEPENDENCY_BYTES)
                .ok_or(ArchiveRetentionError::InvalidLength)?,
        )
        .and_then(|value| value.checked_add(CRC_BYTES))
        .ok_or(ArchiveRetentionError::InvalidLength)
}

fn encode_checkpoint(state: &ArchiveRetentionState) -> Result<Vec<u8>, ArchiveRetentionError> {
    let total_length = encoded_length(state.dependencies.len())?;
    let mut output = vec![0_u8; total_length];
    output[0..8].copy_from_slice(&MAGIC);
    output[8..10].copy_from_slice(&VERSION.to_le_bytes());
    output[10..12].copy_from_slice(&(HEADER_BYTES as u16).to_le_bytes());
    output[12..16].copy_from_slice(&(total_length as u32).to_le_bytes());
    output[16..32].copy_from_slice(&state.archive.product_id);
    output[32..64].copy_from_slice(&state.archive.sha256);
    output[64..72].copy_from_slice(&state.verified_at.get().to_le_bytes());
    output[72..80].copy_from_slice(&state.acknowledged_at.get().to_le_bytes());
    output[80..88].copy_from_slice(&state.delete_after.get().to_le_bytes());
    let mut flags = 0_u32;
    if let Some(deleted_at) = state.deleted_at {
        flags |= DELETED;
        output[88..96].copy_from_slice(&deleted_at.get().to_le_bytes());
    }
    output[96..100].copy_from_slice(&flags.to_le_bytes());
    output[100..104].copy_from_slice(&(state.dependencies.len() as u32).to_le_bytes());
    for (position, segment) in state.dependencies.iter().enumerate() {
        let offset = HEADER_BYTES + position * DEPENDENCY_BYTES;
        output[offset..offset + 8].copy_from_slice(&segment.hardware_id.bytes());
        output[offset + 8..offset + 16].copy_from_slice(&segment.boot_epoch.get().to_le_bytes());
        output[offset + 16..offset + 24].copy_from_slice(&segment.window_ordinal.to_le_bytes());
        output[offset + 24..offset + 32].copy_from_slice(&segment.first_cursor.get().to_le_bytes());
        output[offset + 32..offset + 40]
            .copy_from_slice(&segment.end_exclusive.get().to_le_bytes());
    }
    let crc_offset = total_length - CRC_BYTES;
    let crc = crc32c(&output[..crc_offset]);
    output[crc_offset..].copy_from_slice(&crc.to_le_bytes());
    Ok(output)
}

fn decode_checkpoint(bytes: &[u8]) -> Result<ArchiveRetentionState, ArchiveRetentionError> {
    if bytes.len() < HEADER_BYTES + DEPENDENCY_BYTES + CRC_BYTES {
        return Err(ArchiveRetentionError::InvalidLength);
    }
    if bytes[0..8] != MAGIC {
        return Err(ArchiveRetentionError::InvalidMagic);
    }
    let version = read_u16(bytes, 8);
    if version != VERSION {
        return Err(ArchiveRetentionError::UnsupportedVersion(version));
    }
    if usize::from(read_u16(bytes, 10)) != HEADER_BYTES {
        return Err(ArchiveRetentionError::InvalidLength);
    }
    let total_length =
        usize::try_from(read_u32(bytes, 12)).map_err(|_| ArchiveRetentionError::InvalidLength)?;
    let dependency_count =
        usize::try_from(read_u32(bytes, 100)).map_err(|_| ArchiveRetentionError::InvalidLength)?;
    if total_length != bytes.len() || total_length != encoded_length(dependency_count)? {
        return Err(ArchiveRetentionError::InvalidLength);
    }
    if bytes[104..HEADER_BYTES].iter().any(|value| *value != 0) {
        return Err(ArchiveRetentionError::NonZeroReservedField);
    }
    let stored_crc = read_u32(bytes, total_length - CRC_BYTES);
    if crc32c(&bytes[..total_length - CRC_BYTES]) != stored_crc {
        return Err(ArchiveRetentionError::ChecksumMismatch);
    }
    let flags = read_u32(bytes, 96);
    if flags & !KNOWN_FLAGS != 0 {
        return Err(ArchiveRetentionError::UnknownFlags(flags));
    }
    let deleted_at = if flags & DELETED != 0 {
        Some(TrustedUnixSeconds::new(read_u64(bytes, 88)))
    } else {
        if read_u64(bytes, 88) != 0 {
            return Err(ArchiveRetentionError::NonZeroReservedField);
        }
        None
    };
    let archive = ArchiveIdentity {
        product_id: bytes[16..32].try_into().expect("fixed product ID"),
        sha256: bytes[32..64].try_into().expect("fixed SHA-256"),
    };
    if archive.product_id == [0; 16] || archive.sha256 == [0; 32] {
        return Err(ArchiveRetentionError::InvalidArchiveIdentity);
    }
    let verified_at = TrustedUnixSeconds::new(read_u64(bytes, 64));
    let acknowledged_at = TrustedUnixSeconds::new(read_u64(bytes, 72));
    let delete_after = TrustedUnixSeconds::new(read_u64(bytes, 80));
    if acknowledged_at < verified_at
        || delete_after <= acknowledged_at
        || deleted_at.is_some_and(|value| value < delete_after)
    {
        return Err(ArchiveRetentionError::InvalidStoredState);
    }
    let mut dependencies = Vec::with_capacity(dependency_count);
    for position in 0..dependency_count {
        let offset = HEADER_BYTES + position * DEPENDENCY_BYTES;
        dependencies.push(SegmentKey {
            hardware_id: HardwareId::from_bytes(
                bytes[offset..offset + 8]
                    .try_into()
                    .expect("fixed hardware ID"),
            ),
            boot_epoch: BootEpoch::new(read_u64(bytes, offset + 8)),
            window_ordinal: read_u64(bytes, offset + 16),
            first_cursor: SequenceCursor::new(read_u64(bytes, offset + 24)),
            end_exclusive: SequenceCursor::new(read_u64(bytes, offset + 32)),
        });
    }
    validate_dependencies(&dependencies)?;
    if !dependencies
        .windows(2)
        .all(|pair| segment_sort_key(&pair[0]) < segment_sort_key(&pair[1]))
    {
        return Err(ArchiveRetentionError::InvalidStoredState);
    }
    Ok(ArchiveRetentionState {
        archive,
        dependencies,
        verified_at,
        acknowledged_at,
        delete_after,
        deleted_at,
    })
}

fn persist_atomic(path: &Path, state: &ArchiveRetentionState) -> Result<(), ArchiveRetentionError> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .ok_or(ArchiveRetentionError::MissingParentDirectory)?;
    fs::create_dir_all(parent).map_err(ArchiveRetentionError::Io)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(ArchiveRetentionError::InvalidPath)?;
    let temporary = parent.join(format!(".{file_name}.tmp"));
    let bytes = encode_checkpoint(state)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(ArchiveRetentionError::Io)?;
    file.write_all(&bytes).map_err(ArchiveRetentionError::Io)?;
    file.sync_all().map_err(ArchiveRetentionError::Io)?;
    fs::rename(&temporary, path).map_err(ArchiveRetentionError::Io)?;
    sync_directory(parent)
}

fn read_checkpoint(path: &Path) -> Result<ArchiveRetentionState, ArchiveRetentionError> {
    let mut file = File::open(path).map_err(ArchiveRetentionError::Io)?;
    let mut prefix = [0_u8; 16];
    file.read_exact(&mut prefix)
        .map_err(ArchiveRetentionError::Io)?;
    let total_length =
        usize::try_from(read_u32(&prefix, 12)).map_err(|_| ArchiveRetentionError::InvalidLength)?;
    let maximum = encoded_length(MAX_DEPENDENCIES)?;
    if total_length < HEADER_BYTES + DEPENDENCY_BYTES + CRC_BYTES || total_length > maximum {
        return Err(ArchiveRetentionError::InvalidLength);
    }
    let mut bytes = vec![0_u8; total_length];
    bytes[..prefix.len()].copy_from_slice(&prefix);
    file.read_exact(&mut bytes[prefix.len()..])
        .map_err(ArchiveRetentionError::Io)?;
    let mut trailing = [0_u8; 1];
    if file
        .read(&mut trailing)
        .map_err(ArchiveRetentionError::Io)?
        != 0
    {
        return Err(ArchiveRetentionError::InvalidLength);
    }
    decode_checkpoint(&bytes)
}

fn remove_if_present(path: &Path) -> Result<(), ArchiveRetentionError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ArchiveRetentionError::Io(error)),
    }
}

fn common_parent<'a>(first: &'a Path, second: &Path) -> Result<&'a Path, ArchiveRetentionError> {
    let first_parent = first.parent().ok_or(ArchiveRetentionError::InvalidPath)?;
    let second_parent = second.parent().ok_or(ArchiveRetentionError::InvalidPath)?;
    if first_parent != second_parent {
        return Err(ArchiveRetentionError::ArtifactDirectoryMismatch);
    }
    Ok(first_parent)
}

fn sync_directory(path: &Path) -> Result<(), ArchiveRetentionError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(ArchiveRetentionError::Io)
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
pub enum ArchiveRetentionError {
    Io(io::Error),
    Coverage(ArchiveCoverageError),
    CaptureCheckpoint(CaptureCoverageCheckpointError),
    AcknowledgementBeforeVerification,
    ZeroRetentionDuration,
    TimestampOverflow,
    NoCaptureDependencies,
    TooManyCaptureDependencies,
    InvalidCaptureDependency,
    DuplicateCaptureDependency,
    InvalidArchiveIdentity,
    ArchiveNotDeletable,
    ConflictingDeletionTime,
    AlreadyExists,
    MissingParentDirectory,
    InvalidPath,
    InvalidMagic,
    UnsupportedVersion(u16),
    InvalidLength,
    ChecksumMismatch,
    NonZeroReservedField,
    UnknownFlags(u32),
    InvalidStoredState,
    CaptureCheckpointSetMismatch,
    ArchiveIdentityMismatch,
    ArchiveDependenciesMismatch,
    ArchiveWithoutCoverageIndex,
    ArtifactsPresentAfterRecordedDeletion,
    ArtifactDirectoryMismatch,
    InjectedFault,
}

impl fmt::Display for ArchiveRetentionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "archive retention I/O failed: {error}"),
            Self::Coverage(error) => write!(formatter, "archive coverage invalid: {error}"),
            Self::CaptureCheckpoint(error) => {
                write!(formatter, "Capture cleanup evidence invalid: {error}")
            }
            other => write!(formatter, "archive retention error: {other:?}"),
        }
    }
}

impl std::error::Error for ArchiveRetentionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Coverage(error) => Some(error),
            Self::CaptureCheckpoint(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::archive_coverage::{prove_capture_coverage, ReplicatedArchiveCoverage};
    use crate::archive_coverage::{
        publish_archive_coverage_index, ArchiveInputSpan, ArchivePartition, ArchiveRevision,
        CaptureCoverageProof,
    };
    use crate::capture_coverage_checkpoint::CaptureCoverageCheckpointStore;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sensor-host-archive-retention-{}-{id}",
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

    fn archive() -> ArchiveIdentity {
        ArchiveIdentity {
            product_id: [3; 16],
            sha256: [4; 32],
        }
    }

    fn segment(ordinal: u64, first: u64) -> SegmentKey {
        SegmentKey {
            hardware_id: HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]),
            boot_epoch: BootEpoch::new(9),
            window_ordinal: ordinal,
            first_cursor: SequenceCursor::new(first),
            end_exclusive: SequenceCursor::new(first + 10),
        }
    }

    fn index() -> ArchiveCoverageIndex {
        ArchiveCoverageIndex::new(
            archive(),
            segment(0, 0).hardware_id,
            ArchivePartition::UtcHour {
                start: TrustedUnixSeconds::new(3_600),
            },
            ArchiveRevision {
                number: 0,
                supersedes_product_id: None,
            },
            vec![
                ArchiveInputSpan {
                    segment: segment(1, 10),
                    first_cursor: SequenceCursor::new(10),
                    end_exclusive: SequenceCursor::new(20),
                },
                ArchiveInputSpan {
                    segment: segment(0, 0),
                    first_cursor: SequenceCursor::new(0),
                    end_exclusive: SequenceCursor::new(10),
                },
            ],
        )
        .unwrap()
    }

    fn capture_proof(segment: SegmentKey, acknowledged_at: u64) -> CaptureCoverageProof {
        let coverage = ArchiveCoverageIndex::new(
            archive(),
            segment.hardware_id,
            ArchivePartition::UtcHour {
                start: TrustedUnixSeconds::new(3_600),
            },
            ArchiveRevision {
                number: 0,
                supersedes_product_id: None,
            },
            vec![ArchiveInputSpan {
                segment,
                first_cursor: segment.first_cursor,
                end_exclusive: segment.end_exclusive,
            }],
        )
        .unwrap();
        prove_capture_coverage(
            segment,
            &[ReplicatedArchiveCoverage {
                index: &coverage,
                verified_at: TrustedUnixSeconds::new(100),
                acknowledged_at: TrustedUnixSeconds::new(acknowledged_at),
            }],
        )
        .unwrap()
    }

    fn prepared_target() -> (TempDirectory, ArchiveCleanupTarget) {
        let temp = TempDirectory::new();
        let product = temp.0.join("archive/product.bin");
        let coverage = temp.0.join("archive/product.coverage");
        let checkpoint = temp.0.join("checkpoints/product.retention");
        fs::create_dir_all(product.parent().unwrap()).unwrap();
        fs::write(&product, b"verified archive bytes").unwrap();
        publish_archive_coverage_index(&coverage, &index()).unwrap();
        ArchiveRetentionCheckpointStore::create(
            &checkpoint,
            &index(),
            TrustedUnixSeconds::new(100),
            TrustedUnixSeconds::new(101),
            RetentionPolicy::new(1, 10).unwrap(),
        )
        .unwrap();

        let mut capture_checkpoints = Vec::new();
        for dependency in [segment(0, 0), segment(1, 10)] {
            let path = temp.0.join(format!(
                "checkpoints/capture-{}.coverage",
                dependency.window_ordinal
            ));
            let proof = capture_proof(dependency, 101);
            let mut store = CaptureCoverageCheckpointStore::create(
                &path,
                proof,
                RetentionPolicy::new(1, 10).unwrap(),
            )
            .unwrap();
            store.mark_deleted(TrustedUnixSeconds::new(102)).unwrap();
            capture_checkpoints.push(path);
        }
        (
            temp,
            ArchiveCleanupTarget {
                archive: product,
                coverage_index: coverage,
                checkpoint,
                capture_checkpoints,
            },
        )
    }

    #[test]
    fn checkpoint_persists_product_dependencies_and_deadline() {
        let temp = TempDirectory::new();
        let path = temp.0.join("product.retention");
        let store = ArchiveRetentionCheckpointStore::create(
            &path,
            &index(),
            TrustedUnixSeconds::new(100),
            TrustedUnixSeconds::new(101),
            RetentionPolicy::new(1, 10).unwrap(),
        )
        .unwrap();
        assert_eq!(store.state().archive(), archive());
        assert_eq!(
            store.state().dependencies(),
            &[segment(0, 0), segment(1, 10)]
        );
        assert_eq!(store.state().delete_after(), TrustedUnixSeconds::new(111));
        assert_eq!(
            ArchiveRetentionCheckpointStore::open(&path)
                .unwrap()
                .state(),
            store.state()
        );
    }

    #[test]
    fn cleanup_waits_for_deadline_and_capture_cleanup() {
        let (_temp, target) = prepared_target();
        assert_eq!(
            cleanup_archive_product(&target, TrustedUnixSeconds::new(110)).unwrap(),
            ArchiveCleanupOutcome::NotDue
        );
        let pending = &target.capture_checkpoints[0];
        let bytes = fs::read(pending).unwrap();
        fs::remove_file(pending).unwrap();
        let proof = capture_proof(segment(0, 0), 101);
        CaptureCoverageCheckpointStore::create(
            pending,
            proof,
            RetentionPolicy::new(1, 10).unwrap(),
        )
        .unwrap();
        assert_eq!(
            cleanup_archive_product(&target, TrustedUnixSeconds::new(111)).unwrap(),
            ArchiveCleanupOutcome::WaitingForCaptureCleanup
        );
        fs::write(pending, bytes).unwrap();
        assert_eq!(
            cleanup_archive_product(&target, TrustedUnixSeconds::new(111)).unwrap(),
            ArchiveCleanupOutcome::Deleted
        );
        assert_eq!(
            cleanup_archive_product(&target, TrustedUnixSeconds::new(112)).unwrap(),
            ArchiveCleanupOutcome::AlreadyRecorded
        );
    }

    #[test]
    fn crash_after_unlink_is_reconciled() {
        let (_temp, target) = prepared_target();
        assert!(matches!(
            cleanup_archive_product_inner(
                &target,
                TrustedUnixSeconds::new(111),
                Some(FaultPoint::AfterArchiveRemoval)
            ),
            Err(ArchiveRetentionError::InjectedFault)
        ));
        assert_eq!(
            cleanup_archive_product(&target, TrustedUnixSeconds::new(111)).unwrap(),
            ArchiveCleanupOutcome::Deleted
        );
    }

    #[test]
    fn checkpoint_changes_only_after_directory_sync() {
        let (_temp, target) = prepared_target();
        assert!(matches!(
            cleanup_archive_product_inner(
                &target,
                TrustedUnixSeconds::new(111),
                Some(FaultPoint::AfterDirectorySync)
            ),
            Err(ArchiveRetentionError::InjectedFault)
        ));
        assert!(ArchiveRetentionCheckpointStore::open(&target.checkpoint)
            .unwrap()
            .state()
            .deleted_at()
            .is_none());
        assert_eq!(
            cleanup_archive_product(&target, TrustedUnixSeconds::new(111)).unwrap(),
            ArchiveCleanupOutcome::Deleted
        );
    }

    #[test]
    fn wrong_capture_checkpoint_set_fails_closed() {
        let (_temp, mut target) = prepared_target();
        target.capture_checkpoints.pop();
        assert!(matches!(
            cleanup_archive_product(&target, TrustedUnixSeconds::new(111)),
            Err(ArchiveRetentionError::CaptureCheckpointSetMismatch)
        ));
    }
}
