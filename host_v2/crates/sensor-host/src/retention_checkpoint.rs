use core::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sensor_core::{BootEpoch, HardwareId, SequenceCursor};
use sensor_protocol::crc32c;

use crate::capture_window::CaptureWindowManifest;

const MAGIC: [u8; 8] = *b"SSRETCP1";
const VERSION: u16 = 1;
const CHECKPOINT_BYTES: usize = 176;
const CRC_OFFSET: usize = CHECKPOINT_BYTES - 4;

const ARCHIVE_BOUND: u16 = 1 << 0;
const ARCHIVE_VERIFIED: u16 = 1 << 1;
const REPLICATION_ACKNOWLEDGED: u16 = 1 << 2;
const CAPTURE_DELETED: u16 = 1 << 3;
const ARCHIVE_DELETED: u16 = 1 << 4;
const KNOWN_FLAGS: u16 =
    ARCHIVE_BOUND | ARCHIVE_VERIFIED | REPLICATION_ACKNOWLEDGED | CAPTURE_DELETED | ARCHIVE_DELETED;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TrustedUnixSeconds(u64);

impl TrustedUnixSeconds {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionPolicy {
    pub capture_grace_seconds: u64,
    pub archive_retention_seconds: u64,
}

impl RetentionPolicy {
    pub const DEFAULT: Self = Self {
        capture_grace_seconds: 48 * 60 * 60,
        archive_retention_seconds: 30 * 24 * 60 * 60,
    };

    pub const fn new(
        capture_grace_seconds: u64,
        archive_retention_seconds: u64,
    ) -> Result<Self, RetentionCheckpointError> {
        if capture_grace_seconds == 0 || archive_retention_seconds == 0 {
            Err(RetentionCheckpointError::ZeroRetentionDuration)
        } else {
            Ok(Self {
                capture_grace_seconds,
                archive_retention_seconds,
            })
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentKey {
    pub hardware_id: HardwareId,
    pub boot_epoch: BootEpoch,
    pub window_ordinal: u64,
    pub first_cursor: SequenceCursor,
    pub end_exclusive: SequenceCursor,
}

impl From<CaptureWindowManifest> for SegmentKey {
    fn from(value: CaptureWindowManifest) -> Self {
        Self {
            hardware_id: value.hardware_id,
            boot_epoch: value.boot_epoch,
            window_ordinal: value.window_ordinal,
            first_cursor: value.first_cursor,
            end_exclusive: value.end_exclusive,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArchiveIdentity {
    pub product_id: [u8; 16],
    pub sha256: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionDecision {
    pub capture_deletable: bool,
    pub archive_deletable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionState {
    key: SegmentKey,
    validated_at: TrustedUnixSeconds,
    archive: Option<ArchiveIdentity>,
    archive_verified_at: Option<TrustedUnixSeconds>,
    replication_acknowledged_at: Option<TrustedUnixSeconds>,
    capture_delete_after: Option<TrustedUnixSeconds>,
    archive_delete_after: Option<TrustedUnixSeconds>,
    capture_deleted_at: Option<TrustedUnixSeconds>,
    archive_deleted_at: Option<TrustedUnixSeconds>,
}

impl RetentionState {
    pub fn new(
        manifest: CaptureWindowManifest,
        validated_at: TrustedUnixSeconds,
    ) -> Result<Self, RetentionCheckpointError> {
        let key = SegmentKey::from(manifest);
        if key.boot_epoch.get() == 0 || key.end_exclusive <= key.first_cursor {
            return Err(RetentionCheckpointError::InvalidSegmentKey);
        }
        Ok(Self {
            key,
            validated_at,
            archive: None,
            archive_verified_at: None,
            replication_acknowledged_at: None,
            capture_delete_after: None,
            archive_delete_after: None,
            capture_deleted_at: None,
            archive_deleted_at: None,
        })
    }

    pub const fn key(self) -> SegmentKey {
        self.key
    }

    pub const fn archive(self) -> Option<ArchiveIdentity> {
        self.archive
    }

    pub const fn replication_acknowledged_at(self) -> Option<TrustedUnixSeconds> {
        self.replication_acknowledged_at
    }

    pub const fn capture_delete_after(self) -> Option<TrustedUnixSeconds> {
        self.capture_delete_after
    }

    pub const fn archive_delete_after(self) -> Option<TrustedUnixSeconds> {
        self.archive_delete_after
    }

    pub const fn capture_deleted_at(self) -> Option<TrustedUnixSeconds> {
        self.capture_deleted_at
    }

    pub const fn archive_deleted_at(self) -> Option<TrustedUnixSeconds> {
        self.archive_deleted_at
    }

    pub fn bind_archive(
        &mut self,
        archive: ArchiveIdentity,
    ) -> Result<bool, RetentionCheckpointError> {
        validate_archive_identity(archive)?;
        match self.archive {
            None => {
                self.archive = Some(archive);
                Ok(true)
            }
            Some(existing) if existing == archive => Ok(false),
            Some(_) => Err(RetentionCheckpointError::ArchiveIdentityConflict),
        }
    }

    pub fn mark_archive_verified(
        &mut self,
        archive: ArchiveIdentity,
        at: TrustedUnixSeconds,
    ) -> Result<bool, RetentionCheckpointError> {
        self.require_archive(archive)?;
        require_not_before(at, self.validated_at)?;
        match self.archive_verified_at {
            None => {
                self.archive_verified_at = Some(at);
                Ok(true)
            }
            Some(existing) if existing == at => Ok(false),
            Some(_) => Err(RetentionCheckpointError::ConflictingEventTime),
        }
    }

    pub fn mark_replication_acknowledged(
        &mut self,
        archive: ArchiveIdentity,
        at: TrustedUnixSeconds,
        policy: RetentionPolicy,
    ) -> Result<bool, RetentionCheckpointError> {
        self.require_archive(archive)?;
        let verified_at = self
            .archive_verified_at
            .ok_or(RetentionCheckpointError::ArchiveNotVerified)?;
        require_not_before(at, verified_at)?;
        let capture_delete_after = add_duration(at, policy.capture_grace_seconds)?;
        let archive_delete_after = add_duration(at, policy.archive_retention_seconds)?;
        match self.replication_acknowledged_at {
            None => {
                self.replication_acknowledged_at = Some(at);
                self.capture_delete_after = Some(capture_delete_after);
                self.archive_delete_after = Some(archive_delete_after);
                Ok(true)
            }
            Some(existing)
                if existing == at
                    && self.capture_delete_after == Some(capture_delete_after)
                    && self.archive_delete_after == Some(archive_delete_after) =>
            {
                Ok(false)
            }
            Some(_) => Err(RetentionCheckpointError::ConflictingReplicationAck),
        }
    }

    pub fn decision(self, now: TrustedUnixSeconds) -> RetentionDecision {
        let capture_deletable = self.capture_deleted_at.is_none()
            && self
                .capture_delete_after
                .is_some_and(|deadline| now >= deadline);
        let archive_deletable = self.archive_deleted_at.is_none()
            && self.capture_deleted_at.is_some()
            && self
                .archive_delete_after
                .is_some_and(|deadline| now >= deadline);
        RetentionDecision {
            capture_deletable,
            archive_deletable,
        }
    }

    pub fn mark_capture_deleted(
        &mut self,
        at: TrustedUnixSeconds,
    ) -> Result<bool, RetentionCheckpointError> {
        if let Some(existing) = self.capture_deleted_at {
            return if existing == at {
                Ok(false)
            } else {
                Err(RetentionCheckpointError::ConflictingEventTime)
            };
        }
        if !self.decision(at).capture_deletable {
            return Err(RetentionCheckpointError::CaptureNotDeletable);
        }
        self.capture_deleted_at = Some(at);
        Ok(true)
    }

    pub fn mark_archive_deleted(
        &mut self,
        at: TrustedUnixSeconds,
    ) -> Result<bool, RetentionCheckpointError> {
        if let Some(existing) = self.archive_deleted_at {
            return if existing == at {
                Ok(false)
            } else {
                Err(RetentionCheckpointError::ConflictingEventTime)
            };
        }
        if !self.decision(at).archive_deletable {
            return Err(RetentionCheckpointError::ArchiveNotDeletable);
        }
        self.archive_deleted_at = Some(at);
        Ok(true)
    }

    fn require_archive(self, archive: ArchiveIdentity) -> Result<(), RetentionCheckpointError> {
        match self.archive {
            Some(existing) if existing == archive => Ok(()),
            Some(_) => Err(RetentionCheckpointError::ArchiveIdentityConflict),
            None => Err(RetentionCheckpointError::ArchiveNotBound),
        }
    }
}

pub struct RetentionCheckpointStore {
    path: PathBuf,
    state: RetentionState,
}

impl RetentionCheckpointStore {
    pub fn create(
        path: impl AsRef<Path>,
        manifest: CaptureWindowManifest,
        validated_at: TrustedUnixSeconds,
    ) -> Result<Self, RetentionCheckpointError> {
        let path = path.as_ref().to_path_buf();
        if path.exists() {
            return Err(RetentionCheckpointError::AlreadyExists);
        }
        let state = RetentionState::new(manifest, validated_at)?;
        persist_atomic(&path, state)?;
        Ok(Self { path, state })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, RetentionCheckpointError> {
        let path = path.as_ref().to_path_buf();
        let state = read_checkpoint(&path)?;
        Ok(Self { path, state })
    }

    pub const fn state(&self) -> RetentionState {
        self.state
    }

    pub fn bind_archive(
        &mut self,
        archive: ArchiveIdentity,
    ) -> Result<(), RetentionCheckpointError> {
        self.update(|state| state.bind_archive(archive))
    }

    pub fn mark_archive_verified(
        &mut self,
        archive: ArchiveIdentity,
        at: TrustedUnixSeconds,
    ) -> Result<(), RetentionCheckpointError> {
        self.update(|state| state.mark_archive_verified(archive, at))
    }

    pub fn mark_replication_acknowledged(
        &mut self,
        archive: ArchiveIdentity,
        at: TrustedUnixSeconds,
        policy: RetentionPolicy,
    ) -> Result<(), RetentionCheckpointError> {
        self.update(|state| state.mark_replication_acknowledged(archive, at, policy))
    }

    pub fn mark_capture_deleted(
        &mut self,
        at: TrustedUnixSeconds,
    ) -> Result<(), RetentionCheckpointError> {
        self.update(|state| state.mark_capture_deleted(at))
    }

    pub fn mark_archive_deleted(
        &mut self,
        at: TrustedUnixSeconds,
    ) -> Result<(), RetentionCheckpointError> {
        self.update(|state| state.mark_archive_deleted(at))
    }

    fn update(
        &mut self,
        operation: impl FnOnce(&mut RetentionState) -> Result<bool, RetentionCheckpointError>,
    ) -> Result<(), RetentionCheckpointError> {
        let mut next = self.state;
        if operation(&mut next)? {
            persist_atomic(&self.path, next)?;
            self.state = next;
        }
        Ok(())
    }
}

fn require_not_before(
    value: TrustedUnixSeconds,
    lower_bound: TrustedUnixSeconds,
) -> Result<(), RetentionCheckpointError> {
    if value < lower_bound {
        Err(RetentionCheckpointError::EventTimeRegression)
    } else {
        Ok(())
    }
}

fn add_duration(
    value: TrustedUnixSeconds,
    duration: u64,
) -> Result<TrustedUnixSeconds, RetentionCheckpointError> {
    if duration == 0 {
        return Err(RetentionCheckpointError::ZeroRetentionDuration);
    }
    value
        .get()
        .checked_add(duration)
        .map(TrustedUnixSeconds::new)
        .ok_or(RetentionCheckpointError::TimestampOverflow)
}

fn persist_atomic(path: &Path, state: RetentionState) -> Result<(), RetentionCheckpointError> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .ok_or(RetentionCheckpointError::MissingParentDirectory)?;
    fs::create_dir_all(parent).map_err(RetentionCheckpointError::Io)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(RetentionCheckpointError::InvalidPath)?;
    let temporary = parent.join(format!(".{file_name}.tmp"));
    let bytes = encode_checkpoint(state);
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(RetentionCheckpointError::Io)?;
    file.write_all(&bytes)
        .map_err(RetentionCheckpointError::Io)?;
    file.sync_all().map_err(RetentionCheckpointError::Io)?;
    fs::rename(&temporary, path).map_err(RetentionCheckpointError::Io)?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(RetentionCheckpointError::Io)
}

fn read_checkpoint(path: &Path) -> Result<RetentionState, RetentionCheckpointError> {
    let mut file = File::open(path).map_err(RetentionCheckpointError::Io)?;
    let mut bytes = [0_u8; CHECKPOINT_BYTES];
    file.read_exact(&mut bytes)
        .map_err(RetentionCheckpointError::Io)?;
    let mut trailing = [0_u8; 1];
    if file
        .read(&mut trailing)
        .map_err(RetentionCheckpointError::Io)?
        != 0
    {
        return Err(RetentionCheckpointError::InvalidLength);
    }
    decode_checkpoint(&bytes)
}

fn encode_checkpoint(state: RetentionState) -> [u8; CHECKPOINT_BYTES] {
    let mut output = [0_u8; CHECKPOINT_BYTES];
    output[0..8].copy_from_slice(&MAGIC);
    output[8..10].copy_from_slice(&VERSION.to_le_bytes());
    output[10..12].copy_from_slice(&(CHECKPOINT_BYTES as u16).to_le_bytes());
    output[12..20].copy_from_slice(&state.key.hardware_id.bytes());
    output[20..28].copy_from_slice(&state.key.boot_epoch.get().to_le_bytes());
    output[28..36].copy_from_slice(&state.key.window_ordinal.to_le_bytes());
    output[36..44].copy_from_slice(&state.key.first_cursor.get().to_le_bytes());
    output[44..52].copy_from_slice(&state.key.end_exclusive.get().to_le_bytes());
    output[52..60].copy_from_slice(&state.validated_at.get().to_le_bytes());
    let mut flags = 0_u16;
    if let Some(archive) = state.archive {
        flags |= ARCHIVE_BOUND;
        output[64..80].copy_from_slice(&archive.product_id);
        output[80..112].copy_from_slice(&archive.sha256);
    }
    if let Some(value) = state.archive_verified_at {
        flags |= ARCHIVE_VERIFIED;
        output[112..120].copy_from_slice(&value.get().to_le_bytes());
    }
    if let Some(value) = state.replication_acknowledged_at {
        flags |= REPLICATION_ACKNOWLEDGED;
        output[120..128].copy_from_slice(&value.get().to_le_bytes());
        output[128..136].copy_from_slice(
            &state
                .capture_delete_after
                .expect("acknowledgement sets capture deadline")
                .get()
                .to_le_bytes(),
        );
        output[136..144].copy_from_slice(
            &state
                .archive_delete_after
                .expect("acknowledgement sets archive deadline")
                .get()
                .to_le_bytes(),
        );
    }
    if let Some(value) = state.capture_deleted_at {
        flags |= CAPTURE_DELETED;
        output[144..152].copy_from_slice(&value.get().to_le_bytes());
    }
    if let Some(value) = state.archive_deleted_at {
        flags |= ARCHIVE_DELETED;
        output[152..160].copy_from_slice(&value.get().to_le_bytes());
    }
    output[60..62].copy_from_slice(&flags.to_le_bytes());
    let crc = crc32c(&output[..CRC_OFFSET]);
    output[CRC_OFFSET..].copy_from_slice(&crc.to_le_bytes());
    output
}

fn decode_checkpoint(
    bytes: &[u8; CHECKPOINT_BYTES],
) -> Result<RetentionState, RetentionCheckpointError> {
    if bytes[0..8] != MAGIC {
        return Err(RetentionCheckpointError::InvalidMagic);
    }
    if read_u16(bytes, 8) != VERSION {
        return Err(RetentionCheckpointError::UnsupportedVersion(read_u16(
            bytes, 8,
        )));
    }
    if usize::from(read_u16(bytes, 10)) != CHECKPOINT_BYTES {
        return Err(RetentionCheckpointError::InvalidLength);
    }
    if crc32c(&bytes[..CRC_OFFSET]) != read_u32(bytes, CRC_OFFSET) {
        return Err(RetentionCheckpointError::ChecksumMismatch);
    }
    let flags = read_u16(bytes, 60);
    if flags & !KNOWN_FLAGS != 0 {
        return Err(RetentionCheckpointError::UnknownFlags(flags));
    }
    if bytes[62..64].iter().any(|value| *value != 0)
        || bytes[160..CRC_OFFSET].iter().any(|value| *value != 0)
    {
        return Err(RetentionCheckpointError::NonZeroReservedField);
    }
    let key = SegmentKey {
        hardware_id: HardwareId::from_bytes(bytes[12..20].try_into().expect("fixed field")),
        boot_epoch: BootEpoch::new(read_u64(bytes, 20)),
        window_ordinal: read_u64(bytes, 28),
        first_cursor: SequenceCursor::new(read_u64(bytes, 36)),
        end_exclusive: SequenceCursor::new(read_u64(bytes, 44)),
    };
    if key.boot_epoch.get() == 0 || key.end_exclusive <= key.first_cursor {
        return Err(RetentionCheckpointError::InvalidSegmentKey);
    }
    let archive = if flags & ARCHIVE_BOUND != 0 {
        Some(ArchiveIdentity {
            product_id: bytes[64..80].try_into().expect("fixed field"),
            sha256: bytes[80..112].try_into().expect("fixed field"),
        })
    } else {
        require_zero(bytes, 64, 112)?;
        None
    };
    let archive_verified_at = optional_time(bytes, 112, flags, ARCHIVE_VERIFIED)?;
    let replication_acknowledged_at = optional_time(bytes, 120, flags, REPLICATION_ACKNOWLEDGED)?;
    let capture_delete_after = if replication_acknowledged_at.is_some() {
        Some(TrustedUnixSeconds::new(read_u64(bytes, 128)))
    } else {
        require_zero(bytes, 128, 144)?;
        None
    };
    let archive_delete_after =
        replication_acknowledged_at.map(|_| TrustedUnixSeconds::new(read_u64(bytes, 136)));
    let capture_deleted_at = optional_time(bytes, 144, flags, CAPTURE_DELETED)?;
    let archive_deleted_at = optional_time(bytes, 152, flags, ARCHIVE_DELETED)?;
    let state = RetentionState {
        key,
        validated_at: TrustedUnixSeconds::new(read_u64(bytes, 52)),
        archive,
        archive_verified_at,
        replication_acknowledged_at,
        capture_delete_after,
        archive_delete_after,
        capture_deleted_at,
        archive_deleted_at,
    };
    validate_decoded_state(state)?;
    Ok(state)
}

fn validate_decoded_state(state: RetentionState) -> Result<(), RetentionCheckpointError> {
    if let Some(archive) = state.archive {
        validate_archive_identity(archive)?;
    }
    if state.archive.is_none()
        && (state.archive_verified_at.is_some()
            || state.replication_acknowledged_at.is_some()
            || state.capture_deleted_at.is_some()
            || state.archive_deleted_at.is_some())
    {
        return Err(RetentionCheckpointError::InvalidStoredState);
    }
    if let Some(verified) = state.archive_verified_at {
        require_not_before(verified, state.validated_at)?;
    }
    if let Some(acknowledged) = state.replication_acknowledged_at {
        let verified = state
            .archive_verified_at
            .ok_or(RetentionCheckpointError::InvalidStoredState)?;
        require_not_before(acknowledged, verified)?;
        let capture_after = state
            .capture_delete_after
            .ok_or(RetentionCheckpointError::InvalidStoredState)?;
        let archive_after = state
            .archive_delete_after
            .ok_or(RetentionCheckpointError::InvalidStoredState)?;
        if capture_after <= acknowledged || archive_after <= acknowledged {
            return Err(RetentionCheckpointError::InvalidStoredState);
        }
    } else if state.capture_delete_after.is_some()
        || state.archive_delete_after.is_some()
        || state.capture_deleted_at.is_some()
        || state.archive_deleted_at.is_some()
    {
        return Err(RetentionCheckpointError::InvalidStoredState);
    }
    if let Some(deleted) = state.capture_deleted_at {
        let deadline = state
            .capture_delete_after
            .ok_or(RetentionCheckpointError::InvalidStoredState)?;
        require_not_before(deleted, deadline)?;
    }
    if let Some(deleted) = state.archive_deleted_at {
        let capture_deleted = state
            .capture_deleted_at
            .ok_or(RetentionCheckpointError::InvalidStoredState)?;
        let deadline = state
            .archive_delete_after
            .ok_or(RetentionCheckpointError::InvalidStoredState)?;
        require_not_before(deleted, deadline)?;
        require_not_before(deleted, capture_deleted)?;
    }
    Ok(())
}

fn validate_archive_identity(archive: ArchiveIdentity) -> Result<(), RetentionCheckpointError> {
    if archive.product_id.iter().all(|value| *value == 0)
        || archive.sha256.iter().all(|value| *value == 0)
    {
        Err(RetentionCheckpointError::InvalidArchiveIdentity)
    } else {
        Ok(())
    }
}

fn optional_time(
    bytes: &[u8],
    offset: usize,
    flags: u16,
    flag: u16,
) -> Result<Option<TrustedUnixSeconds>, RetentionCheckpointError> {
    if flags & flag != 0 {
        Ok(Some(TrustedUnixSeconds::new(read_u64(bytes, offset))))
    } else {
        require_zero(bytes, offset, offset + 8)?;
        Ok(None)
    }
}

fn require_zero(bytes: &[u8], start: usize, end: usize) -> Result<(), RetentionCheckpointError> {
    if bytes[start..end].iter().any(|value| *value != 0) {
        Err(RetentionCheckpointError::InvalidStoredState)
    } else {
        Ok(())
    }
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
pub enum RetentionCheckpointError {
    Io(io::Error),
    ZeroRetentionDuration,
    InvalidSegmentKey,
    InvalidArchiveIdentity,
    ArchiveIdentityConflict,
    ArchiveNotBound,
    ArchiveNotVerified,
    EventTimeRegression,
    ConflictingEventTime,
    ConflictingReplicationAck,
    TimestampOverflow,
    CaptureNotDeletable,
    ArchiveNotDeletable,
    AlreadyExists,
    MissingParentDirectory,
    InvalidPath,
    InvalidMagic,
    UnsupportedVersion(u16),
    InvalidLength,
    ChecksumMismatch,
    UnknownFlags(u16),
    NonZeroReservedField,
    InvalidStoredState,
}

impl fmt::Display for RetentionCheckpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "retention checkpoint I/O failed: {error}"),
            other => write!(formatter, "retention checkpoint error: {other:?}"),
        }
    }
}

impl std::error::Error for RetentionCheckpointError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::capture_window::WindowSealReason;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempCheckpoint {
        directory: PathBuf,
        path: PathBuf,
    }

    impl TempCheckpoint {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "sensor-host-retention-checkpoint-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&directory).unwrap();
            let path = directory.join("window.checkpoint");
            Self { directory, path }
        }
    }

    impl Drop for TempCheckpoint {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    fn manifest() -> CaptureWindowManifest {
        CaptureWindowManifest {
            hardware_id: HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]),
            boot_epoch: BootEpoch::new(9),
            window_ordinal: 3,
            first_cursor: SequenceCursor::new(100),
            end_exclusive: SequenceCursor::new(200),
            first_sample_cursor: Some(SequenceCursor::new(100)),
            last_sample_cursor: Some(SequenceCursor::new(199)),
            sample_count: 100,
            missing_sample_count: 0,
            continuity_break_count: 0,
            seal_reason: WindowSealReason::TargetDurationReached,
        }
    }

    fn archive() -> ArchiveIdentity {
        ArchiveIdentity {
            product_id: [7; 16],
            sha256: [9; 32],
        }
    }

    #[test]
    fn default_policy_is_two_days_for_capture_and_thirty_days_for_archive() {
        assert_eq!(RetentionPolicy::DEFAULT.capture_grace_seconds, 172_800);
        assert_eq!(
            RetentionPolicy::DEFAULT.archive_retention_seconds,
            2_592_000
        );
    }

    #[test]
    fn deletion_requires_verified_archive_ack_and_both_deadlines() {
        let mut state = RetentionState::new(manifest(), TrustedUnixSeconds::new(100)).unwrap();
        assert_eq!(
            state.decision(TrustedUnixSeconds::new(u64::MAX)),
            RetentionDecision {
                capture_deletable: false,
                archive_deletable: false,
            }
        );
        state.bind_archive(archive()).unwrap();
        assert!(matches!(
            state.mark_replication_acknowledged(
                archive(),
                TrustedUnixSeconds::new(120),
                RetentionPolicy::DEFAULT,
            ),
            Err(RetentionCheckpointError::ArchiveNotVerified)
        ));
        state
            .mark_archive_verified(archive(), TrustedUnixSeconds::new(110))
            .unwrap();
        state
            .mark_replication_acknowledged(
                archive(),
                TrustedUnixSeconds::new(120),
                RetentionPolicy::DEFAULT,
            )
            .unwrap();
        assert_eq!(
            state.capture_delete_after(),
            Some(TrustedUnixSeconds::new(172_920))
        );
        assert_eq!(
            state.archive_delete_after(),
            Some(TrustedUnixSeconds::new(2_592_120))
        );
        assert!(
            !state
                .decision(TrustedUnixSeconds::new(172_919))
                .capture_deletable
        );
        assert!(
            state
                .decision(TrustedUnixSeconds::new(172_920))
                .capture_deletable
        );
        assert!(matches!(
            state.mark_archive_deleted(TrustedUnixSeconds::new(2_592_120)),
            Err(RetentionCheckpointError::ArchiveNotDeletable)
        ));
        state
            .mark_capture_deleted(TrustedUnixSeconds::new(172_920))
            .unwrap();
        assert!(
            state
                .decision(TrustedUnixSeconds::new(2_592_120))
                .archive_deletable
        );
        state
            .mark_archive_deleted(TrustedUnixSeconds::new(2_592_120))
            .unwrap();
    }

    #[test]
    fn transitions_are_idempotent_but_conflicts_fail_closed() {
        let mut state = RetentionState::new(manifest(), TrustedUnixSeconds::new(100)).unwrap();
        assert!(matches!(state.bind_archive(archive()), Ok(true)));
        assert!(matches!(state.bind_archive(archive()), Ok(false)));
        let different = ArchiveIdentity {
            product_id: [8; 16],
            sha256: [9; 32],
        };
        assert!(matches!(
            state.bind_archive(different),
            Err(RetentionCheckpointError::ArchiveIdentityConflict)
        ));
        assert!(matches!(
            state.mark_archive_verified(archive(), TrustedUnixSeconds::new(99)),
            Err(RetentionCheckpointError::EventTimeRegression)
        ));
        assert!(matches!(
            RetentionPolicy::new(0, 1),
            Err(RetentionCheckpointError::ZeroRetentionDuration)
        ));
    }

    #[test]
    fn atomic_store_round_trips_every_state_transition() {
        let temp = TempCheckpoint::new();
        let mut store =
            RetentionCheckpointStore::create(&temp.path, manifest(), TrustedUnixSeconds::new(100))
                .unwrap();
        store.bind_archive(archive()).unwrap();
        store
            .mark_archive_verified(archive(), TrustedUnixSeconds::new(110))
            .unwrap();
        store
            .mark_replication_acknowledged(
                archive(),
                TrustedUnixSeconds::new(120),
                RetentionPolicy::DEFAULT,
            )
            .unwrap();
        drop(store);

        let recovered = RetentionCheckpointStore::open(&temp.path).unwrap();
        assert_eq!(recovered.state().archive(), Some(archive()));
        assert_eq!(
            recovered.state().replication_acknowledged_at(),
            Some(TrustedUnixSeconds::new(120))
        );
        assert_eq!(
            recovered.state().capture_delete_after(),
            Some(TrustedUnixSeconds::new(172_920))
        );
    }

    #[test]
    fn corrupted_checkpoint_is_never_used_for_deletion() {
        let temp = TempCheckpoint::new();
        drop(
            RetentionCheckpointStore::create(&temp.path, manifest(), TrustedUnixSeconds::new(100))
                .unwrap(),
        );
        let mut bytes = fs::read(&temp.path).unwrap();
        bytes[52] ^= 1;
        fs::write(&temp.path, bytes).unwrap();
        assert!(matches!(
            RetentionCheckpointStore::open(&temp.path),
            Err(RetentionCheckpointError::ChecksumMismatch)
        ));
    }

    #[test]
    fn timestamp_overflow_cannot_make_data_deletable() {
        let mut state =
            RetentionState::new(manifest(), TrustedUnixSeconds::new(u64::MAX - 10)).unwrap();
        state.bind_archive(archive()).unwrap();
        state
            .mark_archive_verified(archive(), TrustedUnixSeconds::new(u64::MAX - 9))
            .unwrap();
        assert!(matches!(
            state.mark_replication_acknowledged(
                archive(),
                TrustedUnixSeconds::new(u64::MAX - 8),
                RetentionPolicy::DEFAULT,
            ),
            Err(RetentionCheckpointError::TimestampOverflow)
        ));
        assert!(
            !state
                .decision(TrustedUnixSeconds::new(u64::MAX))
                .capture_deletable
        );
    }

    #[test]
    fn valid_crc_cannot_hide_a_deadline_before_the_server_ack() {
        let mut state = RetentionState::new(manifest(), TrustedUnixSeconds::new(100)).unwrap();
        state.bind_archive(archive()).unwrap();
        state
            .mark_archive_verified(archive(), TrustedUnixSeconds::new(110))
            .unwrap();
        state
            .mark_replication_acknowledged(
                archive(),
                TrustedUnixSeconds::new(120),
                RetentionPolicy::DEFAULT,
            )
            .unwrap();
        let mut bytes = encode_checkpoint(state);
        bytes[128..136].copy_from_slice(&119_u64.to_le_bytes());
        let crc = crc32c(&bytes[..CRC_OFFSET]);
        bytes[CRC_OFFSET..].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(
            decode_checkpoint(&bytes),
            Err(RetentionCheckpointError::InvalidStoredState)
        ));
    }
}
