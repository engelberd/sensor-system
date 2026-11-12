use core::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sensor_core::{BootEpoch, HardwareId, SequenceCursor};
use sensor_protocol::crc32c;

use crate::archive_coverage::CaptureCoverageProof;
use crate::retention_checkpoint::{
    ArchiveIdentity, RetentionPolicy, SegmentKey, TrustedUnixSeconds,
};

const MAGIC: [u8; 8] = *b"SSCOVCP2";
const VERSION: u16 = 2;
const HEADER_BYTES: usize = 96;
const PRODUCT_BYTES: usize = 48;
const CRC_BYTES: usize = 4;
const MAX_PRODUCTS: usize = 32;
const DELETED: u32 = 1 << 0;
const KNOWN_FLAGS: u32 = DELETED;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureCoverageState {
    segment: SegmentKey,
    archive_products: Vec<ArchiveIdentity>,
    fully_replicated_at: TrustedUnixSeconds,
    delete_after: TrustedUnixSeconds,
    deleted_at: Option<TrustedUnixSeconds>,
}

impl CaptureCoverageState {
    pub fn from_proof(
        proof: CaptureCoverageProof,
        policy: RetentionPolicy,
    ) -> Result<Self, CaptureCoverageCheckpointError> {
        let (segment, archive_products, fully_replicated_at) = proof.into_parts();
        validate_segment(segment)?;
        validate_products(&archive_products)?;
        if policy.capture_grace_seconds == 0 {
            return Err(CaptureCoverageCheckpointError::ZeroGracePeriod);
        }
        let delete_after = fully_replicated_at
            .get()
            .checked_add(policy.capture_grace_seconds)
            .map(TrustedUnixSeconds::new)
            .ok_or(CaptureCoverageCheckpointError::TimestampOverflow)?;
        Ok(Self {
            segment,
            archive_products,
            fully_replicated_at,
            delete_after,
            deleted_at: None,
        })
    }

    pub const fn segment(&self) -> SegmentKey {
        self.segment
    }

    pub fn archive_products(&self) -> &[ArchiveIdentity] {
        &self.archive_products
    }

    pub const fn fully_replicated_at(&self) -> TrustedUnixSeconds {
        self.fully_replicated_at
    }

    pub const fn delete_after(&self) -> TrustedUnixSeconds {
        self.delete_after
    }

    pub const fn deleted_at(&self) -> Option<TrustedUnixSeconds> {
        self.deleted_at
    }

    pub fn deletable(&self, now: TrustedUnixSeconds) -> bool {
        self.deleted_at.is_none() && now >= self.delete_after
    }

    fn mark_deleted(
        &mut self,
        at: TrustedUnixSeconds,
    ) -> Result<bool, CaptureCoverageCheckpointError> {
        if let Some(existing) = self.deleted_at {
            return if existing == at {
                Ok(false)
            } else {
                Err(CaptureCoverageCheckpointError::ConflictingDeletionTime)
            };
        }
        if !self.deletable(at) {
            return Err(CaptureCoverageCheckpointError::CaptureNotDeletable);
        }
        self.deleted_at = Some(at);
        Ok(true)
    }
}

pub struct CaptureCoverageCheckpointStore {
    path: PathBuf,
    state: CaptureCoverageState,
}

impl CaptureCoverageCheckpointStore {
    pub fn create(
        path: impl AsRef<Path>,
        proof: CaptureCoverageProof,
        policy: RetentionPolicy,
    ) -> Result<Self, CaptureCoverageCheckpointError> {
        let path = path.as_ref().to_path_buf();
        let state = CaptureCoverageState::from_proof(proof, policy)?;
        if path.exists() {
            let existing = Self::open(&path)?;
            return if existing.state == state {
                Ok(existing)
            } else {
                Err(CaptureCoverageCheckpointError::AlreadyExists)
            };
        }
        persist_atomic(&path, &state)?;
        Ok(Self { path, state })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, CaptureCoverageCheckpointError> {
        let path = path.as_ref().to_path_buf();
        let state = read_checkpoint(&path)?;
        Ok(Self { path, state })
    }

    pub fn state(&self) -> &CaptureCoverageState {
        &self.state
    }

    pub fn mark_deleted(
        &mut self,
        at: TrustedUnixSeconds,
    ) -> Result<(), CaptureCoverageCheckpointError> {
        let mut next = self.state.clone();
        if next.mark_deleted(at)? {
            persist_atomic(&self.path, &next)?;
            self.state = next;
        }
        Ok(())
    }
}

fn validate_segment(segment: SegmentKey) -> Result<(), CaptureCoverageCheckpointError> {
    if segment.hardware_id.bytes() == [0; 8]
        || segment.boot_epoch.get() == 0
        || segment.end_exclusive <= segment.first_cursor
    {
        Err(CaptureCoverageCheckpointError::InvalidSegment)
    } else {
        Ok(())
    }
}

fn validate_products(products: &[ArchiveIdentity]) -> Result<(), CaptureCoverageCheckpointError> {
    if products.is_empty() {
        return Err(CaptureCoverageCheckpointError::NoArchiveProducts);
    }
    if products.len() > MAX_PRODUCTS {
        return Err(CaptureCoverageCheckpointError::TooManyArchiveProducts);
    }
    for (index, product) in products.iter().copied().enumerate() {
        if product.product_id == [0; 16] || product.sha256 == [0; 32] {
            return Err(CaptureCoverageCheckpointError::InvalidArchiveIdentity);
        }
        if products[..index].contains(&product) {
            return Err(CaptureCoverageCheckpointError::DuplicateArchiveProduct);
        }
    }
    Ok(())
}

fn encoded_length(product_count: usize) -> Result<usize, CaptureCoverageCheckpointError> {
    if product_count == 0 || product_count > MAX_PRODUCTS {
        return Err(CaptureCoverageCheckpointError::InvalidLength);
    }
    HEADER_BYTES
        .checked_add(
            product_count
                .checked_mul(PRODUCT_BYTES)
                .ok_or(CaptureCoverageCheckpointError::InvalidLength)?,
        )
        .and_then(|value| value.checked_add(CRC_BYTES))
        .ok_or(CaptureCoverageCheckpointError::InvalidLength)
}

fn encode_checkpoint(
    state: &CaptureCoverageState,
) -> Result<Vec<u8>, CaptureCoverageCheckpointError> {
    let total_length = encoded_length(state.archive_products.len())?;
    let mut output = vec![0_u8; total_length];
    output[0..8].copy_from_slice(&MAGIC);
    output[8..10].copy_from_slice(&VERSION.to_le_bytes());
    output[10..12].copy_from_slice(&(HEADER_BYTES as u16).to_le_bytes());
    output[12..16].copy_from_slice(&(total_length as u32).to_le_bytes());
    output[16..24].copy_from_slice(&state.segment.hardware_id.bytes());
    output[24..32].copy_from_slice(&state.segment.boot_epoch.get().to_le_bytes());
    output[32..40].copy_from_slice(&state.segment.window_ordinal.to_le_bytes());
    output[40..48].copy_from_slice(&state.segment.first_cursor.get().to_le_bytes());
    output[48..56].copy_from_slice(&state.segment.end_exclusive.get().to_le_bytes());
    output[56..64].copy_from_slice(&state.fully_replicated_at.get().to_le_bytes());
    output[64..72].copy_from_slice(&state.delete_after.get().to_le_bytes());
    let mut flags = 0_u32;
    if let Some(deleted_at) = state.deleted_at {
        flags |= DELETED;
        output[72..80].copy_from_slice(&deleted_at.get().to_le_bytes());
    }
    output[80..84].copy_from_slice(&flags.to_le_bytes());
    output[84..88].copy_from_slice(&(state.archive_products.len() as u32).to_le_bytes());
    for (position, product) in state.archive_products.iter().enumerate() {
        let offset = HEADER_BYTES + position * PRODUCT_BYTES;
        output[offset..offset + 16].copy_from_slice(&product.product_id);
        output[offset + 16..offset + 48].copy_from_slice(&product.sha256);
    }
    let crc_offset = total_length - CRC_BYTES;
    let crc = crc32c(&output[..crc_offset]);
    output[crc_offset..].copy_from_slice(&crc.to_le_bytes());
    Ok(output)
}

fn decode_checkpoint(bytes: &[u8]) -> Result<CaptureCoverageState, CaptureCoverageCheckpointError> {
    if bytes[0..8] != MAGIC {
        return Err(CaptureCoverageCheckpointError::InvalidMagic);
    }
    let version = read_u16(bytes, 8);
    if version != VERSION {
        return Err(CaptureCoverageCheckpointError::UnsupportedVersion(version));
    }
    if usize::from(read_u16(bytes, 10)) != HEADER_BYTES {
        return Err(CaptureCoverageCheckpointError::InvalidLength);
    }
    let total_length = usize::try_from(read_u32(bytes, 12))
        .map_err(|_| CaptureCoverageCheckpointError::InvalidLength)?;
    let product_count = usize::try_from(read_u32(bytes, 84))
        .map_err(|_| CaptureCoverageCheckpointError::InvalidLength)?;
    if total_length != bytes.len() || total_length != encoded_length(product_count)? {
        return Err(CaptureCoverageCheckpointError::InvalidLength);
    }
    let stored_crc = read_u32(bytes, total_length - CRC_BYTES);
    if crc32c(&bytes[..total_length - CRC_BYTES]) != stored_crc {
        return Err(CaptureCoverageCheckpointError::ChecksumMismatch);
    }
    if bytes[88..HEADER_BYTES].iter().any(|value| *value != 0) {
        return Err(CaptureCoverageCheckpointError::NonZeroReservedField);
    }
    let flags = read_u32(bytes, 80);
    if flags & !KNOWN_FLAGS != 0 {
        return Err(CaptureCoverageCheckpointError::UnknownFlags(flags));
    }
    let deleted_at = if flags & DELETED != 0 {
        Some(TrustedUnixSeconds::new(read_u64(bytes, 72)))
    } else {
        if read_u64(bytes, 72) != 0 {
            return Err(CaptureCoverageCheckpointError::NonZeroReservedField);
        }
        None
    };
    let segment = SegmentKey {
        hardware_id: HardwareId::from_bytes(bytes[16..24].try_into().expect("fixed hardware ID")),
        boot_epoch: BootEpoch::new(read_u64(bytes, 24)),
        window_ordinal: read_u64(bytes, 32),
        first_cursor: SequenceCursor::new(read_u64(bytes, 40)),
        end_exclusive: SequenceCursor::new(read_u64(bytes, 48)),
    };
    let fully_replicated_at = TrustedUnixSeconds::new(read_u64(bytes, 56));
    let delete_after = TrustedUnixSeconds::new(read_u64(bytes, 64));
    validate_segment(segment)?;
    if delete_after <= fully_replicated_at || deleted_at.is_some_and(|value| value < delete_after) {
        return Err(CaptureCoverageCheckpointError::InvalidStoredState);
    }
    let mut archive_products = Vec::with_capacity(product_count);
    for position in 0..product_count {
        let offset = HEADER_BYTES + position * PRODUCT_BYTES;
        archive_products.push(ArchiveIdentity {
            product_id: bytes[offset..offset + 16]
                .try_into()
                .expect("fixed product ID"),
            sha256: bytes[offset + 16..offset + 48]
                .try_into()
                .expect("fixed SHA-256"),
        });
    }
    validate_products(&archive_products)?;
    Ok(CaptureCoverageState {
        segment,
        archive_products,
        fully_replicated_at,
        delete_after,
        deleted_at,
    })
}

fn persist_atomic(
    path: &Path,
    state: &CaptureCoverageState,
) -> Result<(), CaptureCoverageCheckpointError> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .ok_or(CaptureCoverageCheckpointError::MissingParentDirectory)?;
    fs::create_dir_all(parent).map_err(CaptureCoverageCheckpointError::Io)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(CaptureCoverageCheckpointError::InvalidPath)?;
    let temporary = parent.join(format!(".{file_name}.tmp"));
    let bytes = encode_checkpoint(state)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(CaptureCoverageCheckpointError::Io)?;
    file.write_all(&bytes)
        .map_err(CaptureCoverageCheckpointError::Io)?;
    file.sync_all()
        .map_err(CaptureCoverageCheckpointError::Io)?;
    fs::rename(&temporary, path).map_err(CaptureCoverageCheckpointError::Io)?;
    sync_directory(parent)
}

fn read_checkpoint(path: &Path) -> Result<CaptureCoverageState, CaptureCoverageCheckpointError> {
    let mut file = File::open(path).map_err(CaptureCoverageCheckpointError::Io)?;
    let mut prefix = [0_u8; 16];
    file.read_exact(&mut prefix)
        .map_err(CaptureCoverageCheckpointError::Io)?;
    if prefix[0..8] != MAGIC {
        return Err(CaptureCoverageCheckpointError::InvalidMagic);
    }
    let total_length = usize::try_from(read_u32(&prefix, 12))
        .map_err(|_| CaptureCoverageCheckpointError::InvalidLength)?;
    let maximum = encoded_length(MAX_PRODUCTS)?;
    if total_length < HEADER_BYTES + PRODUCT_BYTES + CRC_BYTES || total_length > maximum {
        return Err(CaptureCoverageCheckpointError::InvalidLength);
    }
    let mut bytes = vec![0_u8; total_length];
    bytes[..prefix.len()].copy_from_slice(&prefix);
    file.read_exact(&mut bytes[prefix.len()..])
        .map_err(CaptureCoverageCheckpointError::Io)?;
    let mut trailing = [0_u8; 1];
    if file
        .read(&mut trailing)
        .map_err(CaptureCoverageCheckpointError::Io)?
        != 0
    {
        return Err(CaptureCoverageCheckpointError::InvalidLength);
    }
    decode_checkpoint(&bytes)
}

fn sync_directory(path: &Path) -> Result<(), CaptureCoverageCheckpointError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(CaptureCoverageCheckpointError::Io)
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
pub enum CaptureCoverageCheckpointError {
    Io(io::Error),
    InvalidSegment,
    NoArchiveProducts,
    TooManyArchiveProducts,
    InvalidArchiveIdentity,
    DuplicateArchiveProduct,
    ZeroGracePeriod,
    TimestampOverflow,
    CaptureNotDeletable,
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
}

impl fmt::Display for CaptureCoverageCheckpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "coverage checkpoint I/O failed: {error}"),
            other => write!(formatter, "coverage checkpoint error: {other:?}"),
        }
    }
}

impl std::error::Error for CaptureCoverageCheckpointError {
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

    use crate::archive_coverage::{
        prove_capture_coverage, ArchiveCoverageIndex, ArchiveInputSpan, ArchivePartition,
        ArchiveRevision, ReplicatedArchiveCoverage,
    };

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempFile {
        directory: PathBuf,
        path: PathBuf,
    }

    impl TempFile {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "sensor-host-coverage-checkpoint-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&directory).unwrap();
            let path = directory.join("window.coverage");
            Self { directory, path }
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    fn segment() -> SegmentKey {
        SegmentKey {
            hardware_id: HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]),
            boot_epoch: BootEpoch::new(9),
            window_ordinal: 2,
            first_cursor: SequenceCursor::new(10),
            end_exclusive: SequenceCursor::new(20),
        }
    }

    fn archive(id: u8) -> ArchiveIdentity {
        ArchiveIdentity {
            product_id: [id; 16],
            sha256: [id.saturating_add(20); 32],
        }
    }

    fn proof() -> CaptureCoverageProof {
        let source = segment();
        let index = |identity, hour, first, end| {
            ArchiveCoverageIndex::new(
                identity,
                source.hardware_id,
                ArchivePartition::UtcHour {
                    start: TrustedUnixSeconds::new(hour),
                },
                ArchiveRevision {
                    number: 0,
                    supersedes_product_id: None,
                },
                vec![ArchiveInputSpan {
                    segment: source,
                    first_cursor: SequenceCursor::new(first),
                    end_exclusive: SequenceCursor::new(end),
                }],
            )
            .unwrap()
        };
        let first = index(archive(1), 3_600, 10, 15);
        let second = index(archive(2), 7_200, 15, 20);
        prove_capture_coverage(
            source,
            &[
                ReplicatedArchiveCoverage {
                    index: &first,
                    verified_at: TrustedUnixSeconds::new(990),
                    acknowledged_at: TrustedUnixSeconds::new(995),
                },
                ReplicatedArchiveCoverage {
                    index: &second,
                    verified_at: TrustedUnixSeconds::new(996),
                    acknowledged_at: TrustedUnixSeconds::new(1_000),
                },
            ],
        )
        .unwrap()
    }

    #[test]
    fn checkpoint_persists_the_complete_product_set_and_deadline() {
        let temp = TempFile::new();
        let mut store = CaptureCoverageCheckpointStore::create(
            &temp.path,
            proof(),
            RetentionPolicy::new(100, 1_000).unwrap(),
        )
        .unwrap();
        assert_eq!(store.state().archive_products(), &[archive(1), archive(2)]);
        assert_eq!(store.state().delete_after(), TrustedUnixSeconds::new(1_100));
        assert!(!store.state().deletable(TrustedUnixSeconds::new(1_099)));
        store.mark_deleted(TrustedUnixSeconds::new(1_100)).unwrap();

        let recovered = CaptureCoverageCheckpointStore::open(&temp.path).unwrap();
        assert_eq!(recovered.state(), store.state());
        assert_eq!(
            recovered.state().deleted_at(),
            Some(TrustedUnixSeconds::new(1_100))
        );
    }

    #[test]
    fn corruption_and_conflicting_recreation_fail_closed() {
        let temp = TempFile::new();
        CaptureCoverageCheckpointStore::create(
            &temp.path,
            proof(),
            RetentionPolicy::new(100, 1_000).unwrap(),
        )
        .unwrap();
        let conflicting_source = segment();
        let conflicting_index = ArchiveCoverageIndex::new(
            archive(3),
            conflicting_source.hardware_id,
            ArchivePartition::UtcHour {
                start: TrustedUnixSeconds::new(3_600),
            },
            ArchiveRevision {
                number: 0,
                supersedes_product_id: None,
            },
            vec![ArchiveInputSpan {
                segment: conflicting_source,
                first_cursor: conflicting_source.first_cursor,
                end_exclusive: conflicting_source.end_exclusive,
            }],
        )
        .unwrap();
        let conflicting = prove_capture_coverage(
            conflicting_source,
            &[ReplicatedArchiveCoverage {
                index: &conflicting_index,
                verified_at: TrustedUnixSeconds::new(990),
                acknowledged_at: TrustedUnixSeconds::new(1_000),
            }],
        )
        .unwrap();
        assert!(matches!(
            CaptureCoverageCheckpointStore::create(
                &temp.path,
                conflicting,
                RetentionPolicy::new(100, 1_000).unwrap()
            ),
            Err(CaptureCoverageCheckpointError::AlreadyExists)
        ));

        let mut bytes = fs::read(&temp.path).unwrap();
        bytes[HEADER_BYTES + 3] ^= 0x80;
        fs::write(&temp.path, bytes).unwrap();
        assert!(matches!(
            CaptureCoverageCheckpointStore::open(&temp.path),
            Err(CaptureCoverageCheckpointError::ChecksumMismatch)
        ));
    }
}
