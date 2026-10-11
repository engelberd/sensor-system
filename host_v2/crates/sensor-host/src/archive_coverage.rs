use core::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use sensor_core::{BootEpoch, HardwareId, SequenceCursor};
use sensor_protocol::crc32c;

use crate::retention_checkpoint::{ArchiveIdentity, SegmentKey, TrustedUnixSeconds};

const MAGIC: [u8; 8] = *b"SSARCIX1";
const VERSION: u16 = 1;
const HEADER_BYTES: usize = 144;
const SPAN_BYTES: usize = 56;
const CRC_BYTES: usize = 4;
const MAX_SPANS: usize = 1024;
const SUPERSEDES_PRESENT: u32 = 1 << 0;
const KNOWN_FLAGS: u32 = SUPERSEDES_PRESENT;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchivePartition {
    UtcHour {
        start: TrustedUnixSeconds,
    },
    TimeUnresolved {
        boot_epoch: BootEpoch,
        first_cursor: SequenceCursor,
        end_exclusive: SequenceCursor,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArchiveRevision {
    pub number: u32,
    pub supersedes_product_id: Option<[u8; 16]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArchiveInputSpan {
    pub segment: SegmentKey,
    pub first_cursor: SequenceCursor,
    pub end_exclusive: SequenceCursor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveCoverageIndex {
    pub archive: ArchiveIdentity,
    pub hardware_id: HardwareId,
    pub partition: ArchivePartition,
    pub revision: ArchiveRevision,
    pub inputs: Vec<ArchiveInputSpan>,
}

impl ArchiveCoverageIndex {
    pub fn new(
        archive: ArchiveIdentity,
        hardware_id: HardwareId,
        partition: ArchivePartition,
        revision: ArchiveRevision,
        inputs: Vec<ArchiveInputSpan>,
    ) -> Result<Self, ArchiveCoverageError> {
        let value = Self {
            archive,
            hardware_id,
            partition,
            revision,
            inputs,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), ArchiveCoverageError> {
        validate_archive_identity(self.archive)?;
        if self.hardware_id.bytes() == [0; 8] {
            return Err(ArchiveCoverageError::InvalidHardwareId);
        }
        validate_revision(self.archive, self.revision)?;
        match self.partition {
            ArchivePartition::UtcHour { start } => {
                if start.get() % 3600 != 0 {
                    return Err(ArchiveCoverageError::UnalignedUtcHour);
                }
            }
            ArchivePartition::TimeUnresolved {
                boot_epoch,
                first_cursor,
                end_exclusive,
            } => {
                if boot_epoch.get() == 0 || end_exclusive <= first_cursor {
                    return Err(ArchiveCoverageError::InvalidUnresolvedPartition);
                }
            }
        }
        if self.inputs.is_empty() {
            return Err(ArchiveCoverageError::NoInputs);
        }
        if self.inputs.len() > MAX_SPANS {
            return Err(ArchiveCoverageError::TooManyInputs);
        }
        for (index, input) in self.inputs.iter().copied().enumerate() {
            validate_span(self.hardware_id, self.partition, input)?;
            for other in self.inputs[..index].iter().copied() {
                if other.segment == input.segment
                    && ranges_overlap(
                        other.first_cursor,
                        other.end_exclusive,
                        input.first_cursor,
                        input.end_exclusive,
                    )
                {
                    return Err(ArchiveCoverageError::OverlappingInputs);
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ReplicatedArchiveCoverage<'a> {
    pub index: &'a ArchiveCoverageIndex,
    pub verified_at: TrustedUnixSeconds,
    pub acknowledged_at: TrustedUnixSeconds,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureCoverageProof {
    segment: SegmentKey,
    archive_products: Vec<ArchiveIdentity>,
    fully_replicated_at: TrustedUnixSeconds,
}

impl CaptureCoverageProof {
    pub const fn segment(&self) -> SegmentKey {
        self.segment
    }

    pub fn archive_products(&self) -> &[ArchiveIdentity] {
        &self.archive_products
    }

    pub const fn fully_replicated_at(&self) -> TrustedUnixSeconds {
        self.fully_replicated_at
    }

    pub(crate) fn into_parts(self) -> (SegmentKey, Vec<ArchiveIdentity>, TrustedUnixSeconds) {
        (
            self.segment,
            self.archive_products,
            self.fully_replicated_at,
        )
    }
}

pub fn prove_capture_coverage(
    segment: SegmentKey,
    products: &[ReplicatedArchiveCoverage<'_>],
) -> Result<CaptureCoverageProof, ArchiveCoverageError> {
    validate_segment(segment)?;
    let mut covered = Vec::new();
    let mut product_ids = Vec::new();
    let mut latest_ack = None;

    for evidence in products {
        evidence.index.validate()?;
        if evidence.acknowledged_at < evidence.verified_at {
            return Err(ArchiveCoverageError::AcknowledgementBeforeVerification);
        }
        if product_ids.contains(&evidence.index.archive.product_id) {
            return Err(ArchiveCoverageError::DuplicateProduct);
        }
        product_ids.push(evidence.index.archive.product_id);
        let mut contributes = false;
        for input in evidence.index.inputs.iter().copied() {
            if input.segment == segment {
                contributes = true;
                covered.push((input, evidence.index.archive));
            }
        }
        if contributes {
            latest_ack = Some(
                latest_ack
                    .map(|current: TrustedUnixSeconds| current.max(evidence.acknowledged_at))
                    .unwrap_or(evidence.acknowledged_at),
            );
        }
    }

    for evidence in products {
        if let Some(superseded) = evidence.index.revision.supersedes_product_id {
            if product_ids.contains(&superseded) {
                return Err(ArchiveCoverageError::SupersededProductPresent);
            }
        }
    }

    if covered.is_empty() {
        return Err(ArchiveCoverageError::NoProducts);
    }
    covered.sort_unstable_by_key(|(span, _)| span.first_cursor);
    let mut expected = segment.first_cursor;
    let mut archives = Vec::new();
    for (span, archive) in covered {
        if span.first_cursor < expected {
            return Err(ArchiveCoverageError::CoverageOverlap);
        }
        if span.first_cursor > expected {
            return Err(ArchiveCoverageError::CoverageGap {
                expected,
                actual: span.first_cursor,
            });
        }
        expected = span.end_exclusive;
        if !archives.contains(&archive) {
            archives.push(archive);
        }
    }
    if expected != segment.end_exclusive {
        return Err(ArchiveCoverageError::CoverageIncomplete {
            expected: segment.end_exclusive,
            actual: expected,
        });
    }
    let fully_replicated_at = latest_ack.ok_or(ArchiveCoverageError::NoProducts)?;
    Ok(CaptureCoverageProof {
        segment,
        archive_products: archives,
        fully_replicated_at,
    })
}

pub fn publish_archive_coverage_index(
    path: impl AsRef<Path>,
    index: &ArchiveCoverageIndex,
) -> Result<(), ArchiveCoverageError> {
    index.validate()?;
    let path = path.as_ref();
    if path.exists() {
        let existing = read_archive_coverage_index(path)?;
        return if existing == *index {
            Ok(())
        } else {
            Err(ArchiveCoverageError::PublishedIndexConflict)
        };
    }
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .ok_or(ArchiveCoverageError::MissingParentDirectory)?;
    fs::create_dir_all(parent).map_err(ArchiveCoverageError::Io)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(ArchiveCoverageError::InvalidPath)?;
    let temporary = parent.join(format!(".{file_name}.tmp"));
    let bytes = encode_index(index)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(ArchiveCoverageError::Io)?;
    file.write_all(&bytes).map_err(ArchiveCoverageError::Io)?;
    file.sync_all().map_err(ArchiveCoverageError::Io)?;
    fs::rename(&temporary, path).map_err(ArchiveCoverageError::Io)?;
    sync_directory(parent)
}

pub fn read_archive_coverage_index(
    path: impl AsRef<Path>,
) -> Result<ArchiveCoverageIndex, ArchiveCoverageError> {
    let mut file = File::open(path).map_err(ArchiveCoverageError::Io)?;
    let mut header = [0_u8; HEADER_BYTES];
    file.read_exact(&mut header)
        .map_err(ArchiveCoverageError::Io)?;
    if header[0..8] != MAGIC {
        return Err(ArchiveCoverageError::InvalidMagic);
    }
    if read_u16(&header, 8) != VERSION {
        return Err(ArchiveCoverageError::UnsupportedVersion(read_u16(
            &header, 8,
        )));
    }
    if usize::from(read_u16(&header, 10)) != HEADER_BYTES {
        return Err(ArchiveCoverageError::InvalidLength);
    }
    let total_length =
        usize::try_from(read_u32(&header, 12)).map_err(|_| ArchiveCoverageError::InvalidLength)?;
    let span_count =
        usize::try_from(read_u32(&header, 128)).map_err(|_| ArchiveCoverageError::InvalidLength)?;
    let expected_length = encoded_length(span_count)?;
    if total_length != expected_length {
        return Err(ArchiveCoverageError::InvalidLength);
    }
    let mut bytes = vec![0_u8; total_length];
    bytes[..HEADER_BYTES].copy_from_slice(&header);
    file.read_exact(&mut bytes[HEADER_BYTES..])
        .map_err(ArchiveCoverageError::Io)?;
    let mut trailing = [0_u8; 1];
    if file.read(&mut trailing).map_err(ArchiveCoverageError::Io)? != 0 {
        return Err(ArchiveCoverageError::InvalidLength);
    }
    let stored_crc = read_u32(&bytes, total_length - CRC_BYTES);
    if crc32c(&bytes[..total_length - CRC_BYTES]) != stored_crc {
        return Err(ArchiveCoverageError::ChecksumMismatch);
    }
    decode_index(&bytes, span_count)
}

fn validate_archive_identity(archive: ArchiveIdentity) -> Result<(), ArchiveCoverageError> {
    if archive.product_id == [0; 16] || archive.sha256 == [0; 32] {
        Err(ArchiveCoverageError::InvalidArchiveIdentity)
    } else {
        Ok(())
    }
}

fn validate_revision(
    archive: ArchiveIdentity,
    revision: ArchiveRevision,
) -> Result<(), ArchiveCoverageError> {
    match (revision.number, revision.supersedes_product_id) {
        (0, None) => Ok(()),
        (0, Some(_)) | (_, None) => Err(ArchiveCoverageError::InvalidRevision),
        (_, Some(product_id)) if product_id == [0; 16] || product_id == archive.product_id => {
            Err(ArchiveCoverageError::InvalidRevision)
        }
        (_, Some(_)) => Ok(()),
    }
}

fn validate_segment(segment: SegmentKey) -> Result<(), ArchiveCoverageError> {
    if segment.hardware_id.bytes() == [0; 8]
        || segment.boot_epoch.get() == 0
        || segment.end_exclusive <= segment.first_cursor
    {
        Err(ArchiveCoverageError::InvalidSegment)
    } else {
        Ok(())
    }
}

fn validate_span(
    hardware_id: HardwareId,
    partition: ArchivePartition,
    span: ArchiveInputSpan,
) -> Result<(), ArchiveCoverageError> {
    validate_segment(span.segment)?;
    if span.segment.hardware_id != hardware_id {
        return Err(ArchiveCoverageError::HardwareMismatch);
    }
    if span.first_cursor < span.segment.first_cursor
        || span.end_exclusive > span.segment.end_exclusive
        || span.end_exclusive <= span.first_cursor
    {
        return Err(ArchiveCoverageError::SpanOutsideSegment);
    }
    if let ArchivePartition::TimeUnresolved {
        boot_epoch,
        first_cursor,
        end_exclusive,
    } = partition
    {
        if span.segment.boot_epoch != boot_epoch
            || span.first_cursor < first_cursor
            || span.end_exclusive > end_exclusive
        {
            return Err(ArchiveCoverageError::SpanOutsidePartition);
        }
    }
    Ok(())
}

fn ranges_overlap(
    first_a: SequenceCursor,
    end_a: SequenceCursor,
    first_b: SequenceCursor,
    end_b: SequenceCursor,
) -> bool {
    first_a < end_b && first_b < end_a
}

fn encoded_length(span_count: usize) -> Result<usize, ArchiveCoverageError> {
    if span_count == 0 || span_count > MAX_SPANS {
        return Err(ArchiveCoverageError::InvalidLength);
    }
    HEADER_BYTES
        .checked_add(
            span_count
                .checked_mul(SPAN_BYTES)
                .ok_or(ArchiveCoverageError::InvalidLength)?,
        )
        .and_then(|value| value.checked_add(CRC_BYTES))
        .ok_or(ArchiveCoverageError::InvalidLength)
}

fn encode_index(index: &ArchiveCoverageIndex) -> Result<Vec<u8>, ArchiveCoverageError> {
    let total_length = encoded_length(index.inputs.len())?;
    let mut output = vec![0_u8; total_length];
    output[0..8].copy_from_slice(&MAGIC);
    output[8..10].copy_from_slice(&VERSION.to_le_bytes());
    output[10..12].copy_from_slice(&(HEADER_BYTES as u16).to_le_bytes());
    output[12..16].copy_from_slice(&(total_length as u32).to_le_bytes());
    output[16..32].copy_from_slice(&index.archive.product_id);
    output[32..64].copy_from_slice(&index.archive.sha256);
    output[64..72].copy_from_slice(&index.hardware_id.bytes());
    match index.partition {
        ArchivePartition::UtcHour { start } => {
            output[72] = 1;
            output[80..88].copy_from_slice(&start.get().to_le_bytes());
        }
        ArchivePartition::TimeUnresolved {
            boot_epoch,
            first_cursor,
            end_exclusive,
        } => {
            output[72] = 2;
            output[80..88].copy_from_slice(&boot_epoch.get().to_le_bytes());
            output[88..96].copy_from_slice(&first_cursor.get().to_le_bytes());
            output[96..104].copy_from_slice(&end_exclusive.get().to_le_bytes());
        }
    }
    output[104..108].copy_from_slice(&index.revision.number.to_le_bytes());
    if let Some(product_id) = index.revision.supersedes_product_id {
        output[108..112].copy_from_slice(&SUPERSEDES_PRESENT.to_le_bytes());
        output[112..128].copy_from_slice(&product_id);
    }
    output[128..132].copy_from_slice(&(index.inputs.len() as u32).to_le_bytes());
    for (position, span) in index.inputs.iter().enumerate() {
        let offset = HEADER_BYTES + position * SPAN_BYTES;
        output[offset..offset + 8].copy_from_slice(&span.segment.hardware_id.bytes());
        output[offset + 8..offset + 16]
            .copy_from_slice(&span.segment.boot_epoch.get().to_le_bytes());
        output[offset + 16..offset + 24]
            .copy_from_slice(&span.segment.window_ordinal.to_le_bytes());
        output[offset + 24..offset + 32]
            .copy_from_slice(&span.segment.first_cursor.get().to_le_bytes());
        output[offset + 32..offset + 40]
            .copy_from_slice(&span.segment.end_exclusive.get().to_le_bytes());
        output[offset + 40..offset + 48].copy_from_slice(&span.first_cursor.get().to_le_bytes());
        output[offset + 48..offset + 56].copy_from_slice(&span.end_exclusive.get().to_le_bytes());
    }
    let crc_offset = total_length - CRC_BYTES;
    let crc = crc32c(&output[..crc_offset]);
    output[crc_offset..].copy_from_slice(&crc.to_le_bytes());
    Ok(output)
}

fn decode_index(
    bytes: &[u8],
    span_count: usize,
) -> Result<ArchiveCoverageIndex, ArchiveCoverageError> {
    if bytes[73..80].iter().any(|value| *value != 0)
        || bytes[132..HEADER_BYTES].iter().any(|value| *value != 0)
    {
        return Err(ArchiveCoverageError::NonZeroReservedField);
    }
    let archive = ArchiveIdentity {
        product_id: bytes[16..32].try_into().expect("fixed product ID"),
        sha256: bytes[32..64].try_into().expect("fixed SHA-256"),
    };
    let hardware_id = HardwareId::from_bytes(bytes[64..72].try_into().expect("fixed hardware ID"));
    let partition = match bytes[72] {
        1 => {
            if bytes[88..104].iter().any(|value| *value != 0) {
                return Err(ArchiveCoverageError::NonZeroReservedField);
            }
            ArchivePartition::UtcHour {
                start: TrustedUnixSeconds::new(read_u64(bytes, 80)),
            }
        }
        2 => ArchivePartition::TimeUnresolved {
            boot_epoch: BootEpoch::new(read_u64(bytes, 80)),
            first_cursor: SequenceCursor::new(read_u64(bytes, 88)),
            end_exclusive: SequenceCursor::new(read_u64(bytes, 96)),
        },
        value => return Err(ArchiveCoverageError::UnknownPartition(value)),
    };
    let flags = read_u32(bytes, 108);
    if flags & !KNOWN_FLAGS != 0 {
        return Err(ArchiveCoverageError::UnknownFlags(flags));
    }
    let supersedes_product_id = if flags & SUPERSEDES_PRESENT != 0 {
        Some(bytes[112..128].try_into().expect("fixed supersedes ID"))
    } else {
        if bytes[112..128].iter().any(|value| *value != 0) {
            return Err(ArchiveCoverageError::NonZeroReservedField);
        }
        None
    };
    let mut inputs = Vec::with_capacity(span_count);
    for position in 0..span_count {
        let offset = HEADER_BYTES + position * SPAN_BYTES;
        inputs.push(ArchiveInputSpan {
            segment: SegmentKey {
                hardware_id: HardwareId::from_bytes(
                    bytes[offset..offset + 8]
                        .try_into()
                        .expect("fixed span hardware ID"),
                ),
                boot_epoch: BootEpoch::new(read_u64(bytes, offset + 8)),
                window_ordinal: read_u64(bytes, offset + 16),
                first_cursor: SequenceCursor::new(read_u64(bytes, offset + 24)),
                end_exclusive: SequenceCursor::new(read_u64(bytes, offset + 32)),
            },
            first_cursor: SequenceCursor::new(read_u64(bytes, offset + 40)),
            end_exclusive: SequenceCursor::new(read_u64(bytes, offset + 48)),
        });
    }
    ArchiveCoverageIndex::new(
        archive,
        hardware_id,
        partition,
        ArchiveRevision {
            number: read_u32(bytes, 104),
            supersedes_product_id,
        },
        inputs,
    )
}

fn sync_directory(path: &Path) -> Result<(), ArchiveCoverageError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(ArchiveCoverageError::Io)
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
pub enum ArchiveCoverageError {
    Io(io::Error),
    InvalidArchiveIdentity,
    InvalidHardwareId,
    UnalignedUtcHour,
    InvalidUnresolvedPartition,
    InvalidRevision,
    NoInputs,
    TooManyInputs,
    InvalidSegment,
    HardwareMismatch,
    SpanOutsideSegment,
    SpanOutsidePartition,
    OverlappingInputs,
    AcknowledgementBeforeVerification,
    DuplicateProduct,
    SupersededProductPresent,
    NoProducts,
    CoverageGap {
        expected: SequenceCursor,
        actual: SequenceCursor,
    },
    CoverageOverlap,
    CoverageIncomplete {
        expected: SequenceCursor,
        actual: SequenceCursor,
    },
    MissingParentDirectory,
    InvalidPath,
    PublishedIndexConflict,
    InvalidMagic,
    UnsupportedVersion(u16),
    InvalidLength,
    ChecksumMismatch,
    NonZeroReservedField,
    UnknownPartition(u8),
    UnknownFlags(u32),
}

impl fmt::Display for ArchiveCoverageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "archive coverage I/O failed: {error}"),
            other => write!(formatter, "archive coverage error: {other:?}"),
        }
    }
}

impl std::error::Error for ArchiveCoverageError {
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
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempFile {
        directory: PathBuf,
        path: PathBuf,
    }

    impl TempFile {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "sensor-host-archive-coverage-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&directory).unwrap();
            let path = directory.join("archive.coverage");
            Self { directory, path }
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    fn hardware() -> HardwareId {
        HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8])
    }

    fn archive(id: u8) -> ArchiveIdentity {
        ArchiveIdentity {
            product_id: [id; 16],
            sha256: [id.saturating_add(10); 32],
        }
    }

    fn segment(epoch: u64, ordinal: u64, first: u64, end: u64) -> SegmentKey {
        SegmentKey {
            hardware_id: hardware(),
            boot_epoch: BootEpoch::new(epoch),
            window_ordinal: ordinal,
            first_cursor: SequenceCursor::new(first),
            end_exclusive: SequenceCursor::new(end),
        }
    }

    fn span(segment: SegmentKey, first: u64, end: u64) -> ArchiveInputSpan {
        ArchiveInputSpan {
            segment,
            first_cursor: SequenceCursor::new(first),
            end_exclusive: SequenceCursor::new(end),
        }
    }

    fn index(
        identity: ArchiveIdentity,
        hour: u64,
        inputs: Vec<ArchiveInputSpan>,
    ) -> ArchiveCoverageIndex {
        ArchiveCoverageIndex::new(
            identity,
            hardware(),
            ArchivePartition::UtcHour {
                start: TrustedUnixSeconds::new(hour),
            },
            ArchiveRevision {
                number: 0,
                supersedes_product_id: None,
            },
            inputs,
        )
        .unwrap()
    }

    #[test]
    fn two_hourly_products_can_prove_one_crossing_capture_segment() {
        let source = segment(7, 3, 0, 100);
        let first = index(archive(1), 3_600, vec![span(source, 0, 40)]);
        let second = index(archive(2), 7_200, vec![span(source, 40, 100)]);
        let proof = prove_capture_coverage(
            source,
            &[
                ReplicatedArchiveCoverage {
                    index: &first,
                    verified_at: TrustedUnixSeconds::new(10_000),
                    acknowledged_at: TrustedUnixSeconds::new(10_010),
                },
                ReplicatedArchiveCoverage {
                    index: &second,
                    verified_at: TrustedUnixSeconds::new(10_020),
                    acknowledged_at: TrustedUnixSeconds::new(10_030),
                },
            ],
        )
        .unwrap();
        assert_eq!(proof.archive_products(), &[archive(1), archive(2)]);
        assert_eq!(proof.fully_replicated_at(), TrustedUnixSeconds::new(10_030));
    }

    #[test]
    fn one_hour_can_cover_segments_from_two_boot_epochs() {
        let before = segment(7, 3, 0, 20);
        let after = segment(8, 0, 0, 30);
        let product = index(
            archive(3),
            3_600,
            vec![span(before, 0, 20), span(after, 0, 30)],
        );
        let evidence = [ReplicatedArchiveCoverage {
            index: &product,
            verified_at: TrustedUnixSeconds::new(9_000),
            acknowledged_at: TrustedUnixSeconds::new(9_001),
        }];
        assert!(prove_capture_coverage(before, &evidence).is_ok());
        assert!(prove_capture_coverage(after, &evidence).is_ok());
    }

    #[test]
    fn a_gap_overlap_or_superseded_revision_never_authorizes_cleanup() {
        fn evidence<'a>(
            first: &'a ArchiveCoverageIndex,
            second: &'a ArchiveCoverageIndex,
        ) -> [ReplicatedArchiveCoverage<'a>; 2] {
            [
                ReplicatedArchiveCoverage {
                    index: first,
                    verified_at: TrustedUnixSeconds::new(1),
                    acknowledged_at: TrustedUnixSeconds::new(2),
                },
                ReplicatedArchiveCoverage {
                    index: second,
                    verified_at: TrustedUnixSeconds::new(1),
                    acknowledged_at: TrustedUnixSeconds::new(2),
                },
            ]
        }

        let source = segment(7, 3, 0, 100);
        let first = index(archive(1), 3_600, vec![span(source, 0, 40)]);
        let gap = index(archive(2), 7_200, vec![span(source, 41, 100)]);
        assert!(matches!(
            prove_capture_coverage(source, &evidence(&first, &gap)),
            Err(ArchiveCoverageError::CoverageGap { .. })
        ));

        let overlap = index(archive(2), 7_200, vec![span(source, 39, 100)]);
        assert!(matches!(
            prove_capture_coverage(source, &evidence(&first, &overlap)),
            Err(ArchiveCoverageError::CoverageOverlap)
        ));

        let revised = ArchiveCoverageIndex::new(
            archive(4),
            hardware(),
            ArchivePartition::UtcHour {
                start: TrustedUnixSeconds::new(3_600),
            },
            ArchiveRevision {
                number: 1,
                supersedes_product_id: Some(first.archive.product_id),
            },
            vec![span(source, 0, 100)],
        )
        .unwrap();
        assert!(matches!(
            prove_capture_coverage(source, &evidence(&first, &revised)),
            Err(ArchiveCoverageError::SupersededProductPresent)
        ));
    }

    #[test]
    fn published_index_round_trips_and_corruption_fails_closed() {
        let temp = TempFile::new();
        let source = segment(7, 3, 0, 100);
        let value = index(archive(1), 3_600, vec![span(source, 0, 100)]);
        publish_archive_coverage_index(&temp.path, &value).unwrap();
        publish_archive_coverage_index(&temp.path, &value).unwrap();
        assert_eq!(read_archive_coverage_index(&temp.path).unwrap(), value);

        let mut bytes = fs::read(&temp.path).unwrap();
        bytes[HEADER_BYTES + 5] ^= 0x40;
        fs::write(&temp.path, bytes).unwrap();
        assert!(matches!(
            read_archive_coverage_index(&temp.path),
            Err(ArchiveCoverageError::ChecksumMismatch)
        ));
    }
}
