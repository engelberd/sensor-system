use core::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use sensor_core::{
    BootEpoch, ContinuityBreak, ContinuityBreakReason, Gap, GapReason, HardwareId, SampleAxes,
    SampleAxis, SampleSequence, SequenceCursor,
};
use sensor_protocol::{crc32c, SampleChunk, SampleChunkHeader, TimingEvidence};

use crate::capture_window::{
    CaptureWindowError, CaptureWindowLedger, CaptureWindowManifest, CaptureWindowPolicy,
};
use crate::measurement_persistence::DurableMeasurementSink;

const FILE_MAGIC: [u8; 8] = *b"SSCAPWAL";
const FILE_VERSION: u16 = 1;
pub const CAPTURE_WAL_HEADER_BYTES: usize = 64;
const FILE_HEADER_BYTES: usize = CAPTURE_WAL_HEADER_BYTES;
const FILE_HEADER_CRC_OFFSET: usize = FILE_HEADER_BYTES - 4;

const RECORD_VERSION: u8 = 1;
const RECORD_PREFIX_BYTES: usize = 4;
const RECORD_HEADER_BYTES: usize = 4;
const RECORD_CRC_BYTES: usize = 4;
const MIN_RECORD_BODY_BYTES: usize = RECORD_HEADER_BYTES + RECORD_CRC_BYTES;
const MAX_RECORD_BODY_BYTES: usize = 1024 * 1024;

const CHUNK_FIXED_PAYLOAD_BYTES: usize = 56;
const GAP_PAYLOAD_BYTES: usize = 17;
const BREAK_PAYLOAD_BYTES: usize = 9;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum RecordKind {
    SampleChunk = 1,
    Gap = 2,
    ContinuityBreak = 3,
    WindowSeal = 4,
}

impl TryFrom<u8> for RecordKind {
    type Error = CaptureWalError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::SampleChunk),
            2 => Ok(Self::Gap),
            3 => Ok(Self::ContinuityBreak),
            4 => Ok(Self::WindowSeal),
            _ => Err(CaptureWalError::UnknownRecordKind(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureWalConfig {
    pub hardware_id: HardwareId,
    pub boot_epoch: BootEpoch,
    pub recovered_cursor: SequenceCursor,
    pub first_window_ordinal: u64,
    pub window_policy: CaptureWindowPolicy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureWalHeader {
    pub hardware_id: HardwareId,
    pub boot_epoch: BootEpoch,
    pub first_cursor: SequenceCursor,
    pub first_window_ordinal: u64,
    pub window_policy: CaptureWindowPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CaptureRecord {
    SampleChunk(SampleChunk),
    Gap(Gap),
    ContinuityBreak(ContinuityBreak),
    WindowSeal,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CaptureWalRecovery {
    pub durable_cursor: SequenceCursor,
    pub record_count: u64,
    pub sample_count: u64,
    pub missing_sample_count: u64,
    pub continuity_break_count: u64,
    pub sealed_window_count: u64,
    pub last_sealed_window: Option<CaptureWindowManifest>,
    pub truncated_tail_bytes: u64,
}

/// Append-only Capture writer for one `(hardware_id, boot_epoch)` stream.
///
/// `persist_*` returns only after `sync_data`, so its success is a valid input
/// to `DurableReceipt`. Any append or sync failure poisons this instance; the
/// caller must reopen it and use recovery before retrying the pending node
/// record. This prevents an uncertain write from being appended twice.
pub struct CaptureWalSink {
    path: PathBuf,
    file: File,
    hardware_id: HardwareId,
    boot_epoch: BootEpoch,
    ledger: CaptureWindowLedger,
    recovery: CaptureWalRecovery,
    durable_len: u64,
    poisoned: bool,
    #[cfg(test)]
    fail_before_next_sync: bool,
}

/// Prepared durability group for exactly one physical WAL file.
///
/// Records are encoded and ordered in memory. `CaptureWalSink` does not expose
/// their cursors as durable until `commit_batch` has written the complete byte
/// sequence and one `sync_data` barrier has succeeded.
pub struct CaptureWalBatch {
    base_durable_len: u64,
    base_recovery: CaptureWalRecovery,
    ledger: CaptureWindowLedger,
    recovery: CaptureWalRecovery,
    bytes: Vec<u8>,
    record_count: usize,
    sealed: bool,
}

impl CaptureWalBatch {
    pub fn record_count(&self) -> usize {
        self.record_count
    }

    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.record_count == 0
    }

    pub const fn durable_cursor_after_commit(&self) -> SequenceCursor {
        self.recovery.durable_cursor
    }

    pub const fn seals_window(&self) -> bool {
        self.sealed
    }

    pub fn stage_chunk(
        &mut self,
        hardware_id: HardwareId,
        chunk: &SampleChunk,
    ) -> Result<SequenceCursor, CaptureWalError> {
        self.ensure_open_window()?;
        let plan = self
            .ledger
            .plan_chunk(hardware_id, chunk)
            .map_err(CaptureWalError::Window)?;
        let record = encode_chunk_record(chunk)?;
        self.stage_planned(
            record,
            plan,
            RecordCounters {
                samples: u64::from(chunk.header.sample_count),
                ..RecordCounters::default()
            },
        )
    }

    pub fn stage_gap(&mut self, gap: Gap) -> Result<SequenceCursor, CaptureWalError> {
        self.ensure_open_window()?;
        let plan = self.ledger.plan_gap(gap).map_err(CaptureWalError::Window)?;
        self.stage_planned(
            encode_gap_record(gap),
            plan,
            RecordCounters {
                missing_samples: gap.missing_count,
                ..RecordCounters::default()
            },
        )
    }

    pub fn stage_continuity_break(
        &mut self,
        observed: ContinuityBreak,
    ) -> Result<SequenceCursor, CaptureWalError> {
        self.ensure_open_window()?;
        let plan = self
            .ledger
            .plan_continuity_break(observed)
            .map_err(CaptureWalError::Window)?;
        self.stage_planned(
            encode_break_record(observed),
            plan,
            RecordCounters {
                continuity_breaks: 1,
                ..RecordCounters::default()
            },
        )
    }

    fn stage_planned(
        &mut self,
        record: Vec<u8>,
        plan: crate::capture_window::CaptureAppendPlan,
        counters: RecordCounters,
    ) -> Result<SequenceCursor, CaptureWalError> {
        let mut ledger = self.ledger.clone();
        let mut recovery = self.recovery;
        let sealed = ledger.commit(plan).map_err(CaptureWalError::Window)?;
        recovery.durable_cursor = ledger.next_cursor();
        recovery.record_count = checked_add(recovery.record_count, 1)?;
        recovery.sample_count = checked_add(recovery.sample_count, counters.samples)?;
        recovery.missing_sample_count =
            checked_add(recovery.missing_sample_count, counters.missing_samples)?;
        recovery.continuity_break_count =
            checked_add(recovery.continuity_break_count, counters.continuity_breaks)?;
        if let Some(manifest) = sealed {
            recovery.sealed_window_count = checked_add(recovery.sealed_window_count, 1)?;
            recovery.last_sealed_window = Some(manifest);
        }
        let record_count = self
            .record_count
            .checked_add(1)
            .ok_or(CaptureWalError::CounterOverflow)?;
        self.bytes
            .try_reserve(record.len())
            .map_err(|_| CaptureWalError::LengthOverflow)?;
        self.bytes.extend_from_slice(&record);
        self.ledger = ledger;
        self.recovery = recovery;
        self.record_count = record_count;
        self.sealed = sealed.is_some();
        Ok(recovery.durable_cursor)
    }

    fn ensure_open_window(&self) -> Result<(), CaptureWalError> {
        if self.sealed {
            Err(CaptureWalError::BatchCrossesWindowBoundary)
        } else {
            Ok(())
        }
    }
}

/// Read-only, constant-memory decoder used by validation and Archive.
pub struct CaptureWalReader {
    file: File,
    header: CaptureWalHeader,
    offset: u64,
    file_len: u64,
}

impl CaptureWalReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CaptureWalError> {
        let mut file = File::open(path).map_err(CaptureWalError::Io)?;
        let file_len = file.metadata().map_err(CaptureWalError::Io)?.len();
        if file_len < FILE_HEADER_BYTES as u64 {
            return Err(CaptureWalError::TruncatedFileHeader);
        }
        let mut bytes = [0_u8; FILE_HEADER_BYTES];
        file.read_exact(&mut bytes).map_err(CaptureWalError::Io)?;
        let header = decode_file_header(&bytes)?;
        Ok(Self {
            file,
            header,
            offset: FILE_HEADER_BYTES as u64,
            file_len,
        })
    }

    pub const fn header(&self) -> CaptureWalHeader {
        self.header
    }

    pub fn next_record(&mut self) -> Result<Option<CaptureRecord>, CaptureWalError> {
        if self.offset == self.file_len {
            return Ok(None);
        }
        if self.file_len - self.offset < RECORD_PREFIX_BYTES as u64 {
            return Err(CaptureWalError::IncompleteRecord {
                offset: self.offset,
            });
        }
        self.file
            .seek(SeekFrom::Start(self.offset))
            .map_err(CaptureWalError::Io)?;
        let mut prefix = [0_u8; RECORD_PREFIX_BYTES];
        self.file
            .read_exact(&mut prefix)
            .map_err(CaptureWalError::Io)?;
        let body_len = u32::from_le_bytes(prefix) as usize;
        if !(MIN_RECORD_BODY_BYTES..=MAX_RECORD_BODY_BYTES).contains(&body_len) {
            return Err(CaptureWalError::InvalidRecordLength {
                offset: self.offset,
                length: body_len,
            });
        }
        let record_end = self
            .offset
            .checked_add(RECORD_PREFIX_BYTES as u64)
            .and_then(|value| value.checked_add(body_len as u64))
            .ok_or(CaptureWalError::LengthOverflow)?;
        if record_end > self.file_len {
            return Err(CaptureWalError::IncompleteRecord {
                offset: self.offset,
            });
        }
        let mut body = vec![0_u8; body_len];
        self.file
            .read_exact(&mut body)
            .map_err(CaptureWalError::Io)?;
        let data_len = body_len - RECORD_CRC_BYTES;
        if crc32c(&body[..data_len]) != read_u32(&body, data_len) {
            return Err(CaptureWalError::RecordChecksumMismatch {
                offset: self.offset,
            });
        }
        let record = decode_record(
            &body[..data_len],
            self.header.hardware_id,
            self.header.boot_epoch,
        )?;
        self.offset = record_end;
        Ok(Some(record))
    }
}

impl CaptureWalSink {
    pub fn open(path: impl AsRef<Path>, config: CaptureWalConfig) -> Result<Self, CaptureWalError> {
        if config.boot_epoch.get() == 0 {
            return Err(CaptureWalError::ZeroBootEpoch);
        }
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(CaptureWalError::Io)?;
        }
        let existed = path.exists();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(CaptureWalError::Io)?;

        if !existed || file.metadata().map_err(CaptureWalError::Io)?.len() == 0 {
            write_file_header(&mut file, config)?;
            file.sync_all().map_err(CaptureWalError::Io)?;
            sync_parent_directory(&path)?;
        }

        let (ledger, recovery, durable_len) = recover(&mut file, config)?;
        file.seek(SeekFrom::Start(durable_len))
            .map_err(CaptureWalError::Io)?;
        Ok(Self {
            path,
            file,
            hardware_id: config.hardware_id,
            boot_epoch: config.boot_epoch,
            ledger,
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

    pub const fn hardware_id(&self) -> HardwareId {
        self.hardware_id
    }

    pub const fn boot_epoch(&self) -> BootEpoch {
        self.boot_epoch
    }

    pub const fn recovery(&self) -> CaptureWalRecovery {
        self.recovery
    }

    pub const fn durable_cursor(&self) -> SequenceCursor {
        self.recovery.durable_cursor
    }

    pub const fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    #[cfg(test)]
    pub(crate) fn inject_failure_before_next_sync(&mut self) {
        self.fail_before_next_sync = true;
    }

    pub fn begin_batch(&self) -> Result<CaptureWalBatch, CaptureWalError> {
        self.ensure_healthy()?;
        Ok(CaptureWalBatch {
            base_durable_len: self.durable_len,
            base_recovery: self.recovery,
            ledger: self.ledger.clone(),
            recovery: self.recovery,
            bytes: Vec::new(),
            record_count: 0,
            sealed: false,
        })
    }

    pub fn commit_batch(&mut self, batch: CaptureWalBatch) -> Result<(), CaptureWalError> {
        self.ensure_healthy()?;
        if batch.is_empty() {
            return Err(CaptureWalError::EmptyBatch);
        }
        if batch.base_durable_len != self.durable_len || batch.base_recovery != self.recovery {
            return Err(CaptureWalError::StaleBatch);
        }
        let durable_len = self
            .durable_len
            .checked_add(batch.bytes.len() as u64)
            .ok_or(CaptureWalError::LengthOverflow)?;
        self.write_and_sync(&batch.bytes)?;
        self.durable_len = durable_len;
        self.ledger = batch.ledger;
        self.recovery = batch.recovery;
        Ok(())
    }

    pub fn finish_window(&mut self) -> Result<Option<CaptureWindowManifest>, CaptureWalError> {
        self.ensure_healthy()?;
        let mut after = self.ledger.clone();
        let manifest = after.finish().map_err(CaptureWalError::Window)?;
        if let Some(value) = manifest {
            let record = wrap_record(RecordKind::WindowSeal, &[]);
            let durable_len = self
                .durable_len
                .checked_add(record.len() as u64)
                .ok_or(CaptureWalError::LengthOverflow)?;
            let mut recovery = self.recovery;
            recovery.record_count = checked_add(recovery.record_count, 1)?;
            recovery.sealed_window_count = checked_add(recovery.sealed_window_count, 1)?;
            recovery.last_sealed_window = Some(value);
            self.write_and_sync(&record)?;
            self.durable_len = durable_len;
            self.ledger = after;
            self.recovery = recovery;
        }
        Ok(manifest)
    }

    fn ensure_healthy(&self) -> Result<(), CaptureWalError> {
        if self.poisoned {
            Err(CaptureWalError::Poisoned)
        } else {
            Ok(())
        }
    }

    fn write_and_sync(&mut self, bytes: &[u8]) -> Result<(), CaptureWalError> {
        if let Err(error) = self.file.write_all(bytes) {
            self.poisoned = true;
            return Err(CaptureWalError::Io(error));
        }
        #[cfg(test)]
        if self.fail_before_next_sync {
            self.fail_before_next_sync = false;
            self.poisoned = true;
            return Err(CaptureWalError::Io(io::Error::other(
                "injected failure before durability barrier",
            )));
        }
        if let Err(error) = self.file.sync_data() {
            self.poisoned = true;
            return Err(CaptureWalError::Io(error));
        }
        Ok(())
    }
}

impl DurableMeasurementSink for CaptureWalSink {
    type Error = CaptureWalError;

    fn persist_chunk(
        &mut self,
        hardware_id: HardwareId,
        chunk: &SampleChunk,
    ) -> Result<(), Self::Error> {
        let mut batch = self.begin_batch()?;
        batch.stage_chunk(hardware_id, chunk)?;
        self.commit_batch(batch)
    }

    fn persist_gap(&mut self, gap: Gap) -> Result<(), Self::Error> {
        let mut batch = self.begin_batch()?;
        batch.stage_gap(gap)?;
        self.commit_batch(batch)
    }

    fn persist_continuity_break(&mut self, observed: ContinuityBreak) -> Result<(), Self::Error> {
        let mut batch = self.begin_batch()?;
        batch.stage_continuity_break(observed)?;
        self.commit_batch(batch)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RecordCounters {
    samples: u64,
    missing_samples: u64,
    continuity_breaks: u64,
}

fn recover(
    file: &mut File,
    config: CaptureWalConfig,
) -> Result<(CaptureWindowLedger, CaptureWalRecovery, u64), CaptureWalError> {
    let file_len = file.metadata().map_err(CaptureWalError::Io)?.len();
    if file_len < FILE_HEADER_BYTES as u64 {
        return Err(CaptureWalError::TruncatedFileHeader);
    }
    let mut header = [0_u8; FILE_HEADER_BYTES];
    file.seek(SeekFrom::Start(0)).map_err(CaptureWalError::Io)?;
    file.read_exact(&mut header).map_err(CaptureWalError::Io)?;
    validate_file_header(&header, config)?;

    let mut ledger = CaptureWindowLedger::new(
        config.window_policy,
        config.hardware_id,
        config.boot_epoch,
        config.recovered_cursor,
        config.first_window_ordinal,
    );
    let mut report = CaptureWalRecovery {
        durable_cursor: config.recovered_cursor,
        ..CaptureWalRecovery::default()
    };
    let mut offset = FILE_HEADER_BYTES as u64;

    while offset < file_len {
        let remaining = file_len - offset;
        if remaining < RECORD_PREFIX_BYTES as u64 {
            return truncate_tail(file, &mut report, offset, file_len)
                .map(|()| (ledger, report, offset));
        }

        file.seek(SeekFrom::Start(offset))
            .map_err(CaptureWalError::Io)?;
        let mut prefix = [0_u8; RECORD_PREFIX_BYTES];
        file.read_exact(&mut prefix).map_err(CaptureWalError::Io)?;
        let body_len = u32::from_le_bytes(prefix) as usize;
        if !(MIN_RECORD_BODY_BYTES..=MAX_RECORD_BODY_BYTES).contains(&body_len) {
            return Err(CaptureWalError::InvalidRecordLength {
                offset,
                length: body_len,
            });
        }
        let record_end = offset
            .checked_add(RECORD_PREFIX_BYTES as u64)
            .and_then(|value| value.checked_add(body_len as u64))
            .ok_or(CaptureWalError::LengthOverflow)?;
        if record_end > file_len {
            return truncate_tail(file, &mut report, offset, file_len)
                .map(|()| (ledger, report, offset));
        }

        let mut body = vec![0_u8; body_len];
        file.read_exact(&mut body).map_err(CaptureWalError::Io)?;
        let data_len = body_len - RECORD_CRC_BYTES;
        let expected_crc = read_u32(&body, data_len);
        if crc32c(&body[..data_len]) != expected_crc {
            return Err(CaptureWalError::RecordChecksumMismatch { offset });
        }

        let sealed = replay_record(
            &mut ledger,
            config.hardware_id,
            config.boot_epoch,
            &body[..data_len],
            &mut report,
        )?;
        if let Some(manifest) = sealed {
            report.sealed_window_count = checked_add(report.sealed_window_count, 1)?;
            report.last_sealed_window = Some(manifest);
        }
        report.record_count = checked_add(report.record_count, 1)?;
        report.durable_cursor = ledger.next_cursor();
        offset = record_end;
    }
    Ok((ledger, report, offset))
}

fn replay_record(
    ledger: &mut CaptureWindowLedger,
    hardware_id: HardwareId,
    boot_epoch: BootEpoch,
    data: &[u8],
    report: &mut CaptureWalRecovery,
) -> Result<Option<CaptureWindowManifest>, CaptureWalError> {
    match decode_record(data, hardware_id, boot_epoch)? {
        CaptureRecord::SampleChunk(chunk) => {
            let count = u64::from(chunk.header.sample_count);
            let plan = ledger
                .plan_chunk(hardware_id, &chunk)
                .map_err(CaptureWalError::Window)?;
            let sealed = ledger.commit(plan).map_err(CaptureWalError::Window)?;
            report.sample_count = checked_add(report.sample_count, count)?;
            Ok(sealed)
        }
        CaptureRecord::Gap(gap) => {
            let count = gap.missing_count;
            let plan = ledger.plan_gap(gap).map_err(CaptureWalError::Window)?;
            let sealed = ledger.commit(plan).map_err(CaptureWalError::Window)?;
            report.missing_sample_count = checked_add(report.missing_sample_count, count)?;
            Ok(sealed)
        }
        CaptureRecord::ContinuityBreak(observed) => {
            let plan = ledger
                .plan_continuity_break(observed)
                .map_err(CaptureWalError::Window)?;
            let sealed = ledger.commit(plan).map_err(CaptureWalError::Window)?;
            report.continuity_break_count = checked_add(report.continuity_break_count, 1)?;
            Ok(sealed)
        }
        CaptureRecord::WindowSeal => ledger.finish().map_err(CaptureWalError::Window),
    }
}

fn decode_record(
    data: &[u8],
    hardware_id: HardwareId,
    boot_epoch: BootEpoch,
) -> Result<CaptureRecord, CaptureWalError> {
    if data.len() < RECORD_HEADER_BYTES {
        return Err(CaptureWalError::InvalidRecordPayload);
    }
    let kind = RecordKind::try_from(data[0])?;
    if data[1] != RECORD_VERSION {
        return Err(CaptureWalError::UnsupportedRecordVersion(data[1]));
    }
    if data[2] != 0 || data[3] != 0 {
        return Err(CaptureWalError::NonZeroReservedField);
    }
    let payload = &data[RECORD_HEADER_BYTES..];
    match kind {
        RecordKind::SampleChunk => {
            decode_chunk(payload, boot_epoch).map(CaptureRecord::SampleChunk)
        }
        RecordKind::Gap => decode_gap(payload, hardware_id, boot_epoch).map(CaptureRecord::Gap),
        RecordKind::ContinuityBreak => {
            decode_break(payload, hardware_id, boot_epoch).map(CaptureRecord::ContinuityBreak)
        }
        RecordKind::WindowSeal => {
            if !payload.is_empty() {
                return Err(CaptureWalError::InvalidRecordPayload);
            }
            Ok(CaptureRecord::WindowSeal)
        }
    }
}

fn write_file_header(file: &mut File, config: CaptureWalConfig) -> Result<(), CaptureWalError> {
    let mut header = [0_u8; FILE_HEADER_BYTES];
    header[0..8].copy_from_slice(&FILE_MAGIC);
    header[8..10].copy_from_slice(&FILE_VERSION.to_le_bytes());
    header[10..12].copy_from_slice(&(FILE_HEADER_BYTES as u16).to_le_bytes());
    header[12..20].copy_from_slice(&config.hardware_id.bytes());
    header[20..28].copy_from_slice(&config.boot_epoch.get().to_le_bytes());
    header[28..36].copy_from_slice(&config.recovered_cursor.get().to_le_bytes());
    header[36..44].copy_from_slice(&config.first_window_ordinal.to_le_bytes());
    header[44..52].copy_from_slice(&config.window_policy.target_duration_us.to_le_bytes());
    let crc = crc32c(&header[..FILE_HEADER_CRC_OFFSET]);
    header[FILE_HEADER_CRC_OFFSET..].copy_from_slice(&crc.to_le_bytes());
    file.seek(SeekFrom::Start(0)).map_err(CaptureWalError::Io)?;
    file.write_all(&header).map_err(CaptureWalError::Io)
}

fn validate_file_header(
    header: &[u8; FILE_HEADER_BYTES],
    config: CaptureWalConfig,
) -> Result<(), CaptureWalError> {
    let decoded = decode_file_header(header)?;
    if decoded.hardware_id != config.hardware_id {
        return Err(CaptureWalError::HardwareMismatch);
    }
    if decoded.boot_epoch != config.boot_epoch {
        return Err(CaptureWalError::BootEpochMismatch);
    }
    if decoded.first_cursor != config.recovered_cursor {
        return Err(CaptureWalError::StartingCursorMismatch);
    }
    if decoded.first_window_ordinal != config.first_window_ordinal {
        return Err(CaptureWalError::WindowOrdinalMismatch);
    }
    if decoded.window_policy != config.window_policy {
        return Err(CaptureWalError::WindowPolicyMismatch);
    }
    Ok(())
}

fn decode_file_header(
    header: &[u8; FILE_HEADER_BYTES],
) -> Result<CaptureWalHeader, CaptureWalError> {
    if header[0..8] != FILE_MAGIC {
        return Err(CaptureWalError::InvalidFileMagic);
    }
    let version = read_u16(header, 8);
    if version != FILE_VERSION {
        return Err(CaptureWalError::UnsupportedFileVersion(version));
    }
    if usize::from(read_u16(header, 10)) != FILE_HEADER_BYTES {
        return Err(CaptureWalError::InvalidFileHeaderLength);
    }
    if crc32c(&header[..FILE_HEADER_CRC_OFFSET]) != read_u32(header, FILE_HEADER_CRC_OFFSET) {
        return Err(CaptureWalError::FileHeaderChecksumMismatch);
    }
    if header[52..FILE_HEADER_CRC_OFFSET]
        .iter()
        .any(|value| *value != 0)
    {
        return Err(CaptureWalError::NonZeroReservedField);
    }
    let boot_epoch = BootEpoch::new(read_u64(header, 20));
    if boot_epoch.get() == 0 {
        return Err(CaptureWalError::ZeroBootEpoch);
    }
    Ok(CaptureWalHeader {
        hardware_id: HardwareId::from_bytes(header[12..20].try_into().expect("fixed field")),
        boot_epoch,
        first_cursor: SequenceCursor::new(read_u64(header, 28)),
        first_window_ordinal: read_u64(header, 36),
        window_policy: CaptureWindowPolicy::new(read_u64(header, 44))
            .map_err(CaptureWalError::Window)?,
    })
}

fn encode_chunk_record(chunk: &SampleChunk) -> Result<Vec<u8>, CaptureWalError> {
    chunk.validate().map_err(CaptureWalError::ChunkContract)?;
    let mut payload = Vec::with_capacity(CHUNK_FIXED_PAYLOAD_BYTES + chunk.samples.len() * 9);
    payload.extend_from_slice(&chunk.header.retained_from.get().to_le_bytes());
    payload.extend_from_slice(&chunk.header.produced.get().to_le_bytes());
    payload.extend_from_slice(&chunk.header.first.get().to_le_bytes());
    payload.extend_from_slice(&chunk.header.sample_count.to_le_bytes());
    payload.extend_from_slice(&chunk.config_revision.to_le_bytes());
    payload.extend_from_slice(&chunk.timing.segment_id.to_le_bytes());
    payload.extend_from_slice(&chunk.timing.first_device_time_us.to_le_bytes());
    payload.extend_from_slice(&chunk.timing.last_device_time_us.to_le_bytes());
    payload.extend_from_slice(&chunk.timing.max_fit_residual_us.to_le_bytes());
    payload.extend_from_slice(&chunk.timing.quality_flags.to_le_bytes());
    for sample in &chunk.samples {
        encode_i24(sample.x, &mut payload);
        encode_i24(sample.y, &mut payload);
        encode_i24(sample.z, &mut payload);
    }
    Ok(wrap_record(RecordKind::SampleChunk, &payload))
}

fn encode_gap_record(gap: Gap) -> Vec<u8> {
    let mut payload = Vec::with_capacity(GAP_PAYLOAD_BYTES);
    payload.extend_from_slice(&gap.first_missing.get().to_le_bytes());
    payload.extend_from_slice(&gap.missing_count.to_le_bytes());
    payload.push(encode_gap_reason(gap.reason));
    wrap_record(RecordKind::Gap, &payload)
}

fn encode_break_record(observed: ContinuityBreak) -> Vec<u8> {
    let mut payload = Vec::with_capacity(BREAK_PAYLOAD_BYTES);
    payload.extend_from_slice(&observed.at.get().to_le_bytes());
    payload.push(encode_break_reason(observed.reason));
    wrap_record(RecordKind::ContinuityBreak, &payload)
}

fn wrap_record(kind: RecordKind, payload: &[u8]) -> Vec<u8> {
    let body_len = RECORD_HEADER_BYTES + payload.len() + RECORD_CRC_BYTES;
    let mut record = Vec::with_capacity(RECORD_PREFIX_BYTES + body_len);
    record.extend_from_slice(&(body_len as u32).to_le_bytes());
    record.push(kind as u8);
    record.push(RECORD_VERSION);
    record.extend_from_slice(&[0, 0]);
    record.extend_from_slice(payload);
    let crc = crc32c(&record[RECORD_PREFIX_BYTES..]);
    record.extend_from_slice(&crc.to_le_bytes());
    record
}

fn decode_chunk(payload: &[u8], boot_epoch: BootEpoch) -> Result<SampleChunk, CaptureWalError> {
    if payload.len() < CHUNK_FIXED_PAYLOAD_BYTES {
        return Err(CaptureWalError::InvalidRecordPayload);
    }
    let count = read_u16(payload, 24);
    let expected = CHUNK_FIXED_PAYLOAD_BYTES
        .checked_add(usize::from(count) * 9)
        .ok_or(CaptureWalError::LengthOverflow)?;
    if payload.len() != expected {
        return Err(CaptureWalError::InvalidRecordPayload);
    }
    let mut samples = Vec::with_capacity(usize::from(count));
    for index in 0..usize::from(count) {
        let offset = CHUNK_FIXED_PAYLOAD_BYTES + index * 9;
        samples.push(SampleAxes::new(
            decode_i24(&payload[offset..offset + 3])?,
            decode_i24(&payload[offset + 3..offset + 6])?,
            decode_i24(&payload[offset + 6..offset + 9])?,
        ));
    }
    let chunk = SampleChunk {
        header: SampleChunkHeader {
            boot_epoch,
            retained_from: SequenceCursor::new(read_u64(payload, 0)),
            produced: SequenceCursor::new(read_u64(payload, 8)),
            first: SequenceCursor::new(read_u64(payload, 16)),
            sample_count: count,
        },
        config_revision: read_u32(payload, 26),
        timing: TimingEvidence {
            segment_id: read_u32(payload, 30),
            first_device_time_us: read_u64(payload, 34),
            last_device_time_us: read_u64(payload, 42),
            max_fit_residual_us: read_u32(payload, 50),
            quality_flags: read_u16(payload, 54),
        },
        samples,
    };
    chunk.validate().map_err(CaptureWalError::ChunkContract)?;
    Ok(chunk)
}

fn decode_gap(
    payload: &[u8],
    hardware_id: HardwareId,
    boot_epoch: BootEpoch,
) -> Result<Gap, CaptureWalError> {
    if payload.len() != GAP_PAYLOAD_BYTES {
        return Err(CaptureWalError::InvalidRecordPayload);
    }
    Gap::new(
        hardware_id,
        boot_epoch,
        SampleSequence::new(read_u64(payload, 0)),
        read_u64(payload, 8),
        decode_gap_reason(payload[16])?,
    )
    .map_err(|_| CaptureWalError::InvalidRecordPayload)
}

fn decode_break(
    payload: &[u8],
    hardware_id: HardwareId,
    boot_epoch: BootEpoch,
) -> Result<ContinuityBreak, CaptureWalError> {
    if payload.len() != BREAK_PAYLOAD_BYTES {
        return Err(CaptureWalError::InvalidRecordPayload);
    }
    ContinuityBreak::new(
        hardware_id,
        boot_epoch,
        SequenceCursor::new(read_u64(payload, 0)),
        decode_break_reason(payload[8])?,
    )
    .map_err(|_| CaptureWalError::InvalidRecordPayload)
}

fn encode_i24(value: SampleAxis, output: &mut Vec<u8>) {
    let bytes = value.get().to_le_bytes();
    output.extend_from_slice(&bytes[..3]);
}

fn decode_i24(bytes: &[u8]) -> Result<SampleAxis, CaptureWalError> {
    let extension = if bytes[2] & 0x80 == 0 { 0 } else { 0xff };
    SampleAxis::new(i32::from_le_bytes([
        bytes[0], bytes[1], bytes[2], extension,
    ]))
    .map_err(|_| CaptureWalError::InvalidRecordPayload)
}

fn encode_gap_reason(reason: GapReason) -> u8 {
    match reason {
        GapReason::UnknownDiscontinuity => 0,
        GapReason::SourceLoss => 1,
        GapReason::IngressOverflow => 2,
        GapReason::JournalOverflow => 3,
    }
}

fn decode_gap_reason(value: u8) -> Result<GapReason, CaptureWalError> {
    match value {
        0 => Ok(GapReason::UnknownDiscontinuity),
        1 => Ok(GapReason::SourceLoss),
        2 => Ok(GapReason::IngressOverflow),
        3 => Ok(GapReason::JournalOverflow),
        _ => Err(CaptureWalError::InvalidRecordPayload),
    }
}

fn encode_break_reason(reason: ContinuityBreakReason) -> u8 {
    match reason {
        ContinuityBreakReason::Unknown => 0,
        ContinuityBreakReason::SensorOverflow => 1,
        ContinuityBreakReason::SensorReadUncertain => 2,
        ContinuityBreakReason::InvalidTiming => 3,
        ContinuityBreakReason::ProcessingReset => 4,
    }
}

fn decode_break_reason(value: u8) -> Result<ContinuityBreakReason, CaptureWalError> {
    match value {
        0 => Ok(ContinuityBreakReason::Unknown),
        1 => Ok(ContinuityBreakReason::SensorOverflow),
        2 => Ok(ContinuityBreakReason::SensorReadUncertain),
        3 => Ok(ContinuityBreakReason::InvalidTiming),
        4 => Ok(ContinuityBreakReason::ProcessingReset),
        _ => Err(CaptureWalError::InvalidRecordPayload),
    }
}

fn truncate_tail(
    file: &mut File,
    report: &mut CaptureWalRecovery,
    valid_len: u64,
    old_len: u64,
) -> Result<(), CaptureWalError> {
    file.set_len(valid_len).map_err(CaptureWalError::Io)?;
    file.sync_all().map_err(CaptureWalError::Io)?;
    report.truncated_tail_bytes = old_len - valid_len;
    Ok(())
}

fn sync_parent_directory(path: &Path) -> Result<(), CaptureWalError> {
    if let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(CaptureWalError::Io)?;
    }
    Ok(())
}

fn checked_add(left: u64, right: u64) -> Result<u64, CaptureWalError> {
    left.checked_add(right)
        .ok_or(CaptureWalError::CounterOverflow)
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
pub enum CaptureWalError {
    Io(io::Error),
    Window(CaptureWindowError),
    ChunkContract(sensor_protocol::MeasurementWireError),
    ZeroBootEpoch,
    TruncatedFileHeader,
    InvalidFileMagic,
    UnsupportedFileVersion(u16),
    InvalidFileHeaderLength,
    FileHeaderChecksumMismatch,
    HardwareMismatch,
    BootEpochMismatch,
    StartingCursorMismatch,
    WindowOrdinalMismatch,
    WindowPolicyMismatch,
    IncompleteRecord { offset: u64 },
    InvalidRecordLength { offset: u64, length: usize },
    RecordChecksumMismatch { offset: u64 },
    UnknownRecordKind(u8),
    UnsupportedRecordVersion(u8),
    NonZeroReservedField,
    InvalidRecordPayload,
    LengthOverflow,
    CounterOverflow,
    Poisoned,
    EmptyBatch,
    StaleBatch,
    BatchCrossesWindowBoundary,
}

impl fmt::Display for CaptureWalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "capture WAL I/O failed: {error}"),
            Self::Window(error) => write!(formatter, "capture WAL ordering failed: {error}"),
            Self::ChunkContract(error) => {
                write!(formatter, "capture WAL chunk is invalid: {error}")
            }
            other => write!(formatter, "capture WAL error: {other:?}"),
        }
    }
}

impl std::error::Error for CaptureWalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Window(error) => Some(error),
            Self::ChunkContract(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    use sensor_core::{ContinuityBreakReason, GapReason};
    use sensor_protocol::TIMING_VALID;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempWal {
        directory: PathBuf,
        path: PathBuf,
    }

    impl TempWal {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "sensor-host-capture-wal-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join("capture.wal");
            Self { directory, path }
        }
    }

    impl Drop for TempWal {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    const HARDWARE: HardwareId = HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);
    const BOOT: BootEpoch = BootEpoch::new(9);

    fn config(target_duration_us: u64) -> CaptureWalConfig {
        CaptureWalConfig {
            hardware_id: HARDWARE,
            boot_epoch: BOOT,
            recovered_cursor: SequenceCursor::ORIGIN,
            first_window_ordinal: 0,
            window_policy: CaptureWindowPolicy::new(target_duration_us).unwrap(),
        }
    }

    fn chunk(first: u64, values: &[[i32; 3]], first_time: u64, last_time: u64) -> SampleChunk {
        let samples = values
            .iter()
            .map(|value| {
                SampleAxes::new(
                    SampleAxis::new(value[0]).unwrap(),
                    SampleAxis::new(value[1]).unwrap(),
                    SampleAxis::new(value[2]).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let count = u16::try_from(samples.len()).unwrap();
        SampleChunk {
            header: SampleChunkHeader {
                boot_epoch: BOOT,
                retained_from: SequenceCursor::new(first),
                produced: SequenceCursor::new(first + u64::from(count)),
                first: SequenceCursor::new(first),
                sample_count: count,
            },
            config_revision: 1,
            timing: TimingEvidence {
                segment_id: 1,
                first_device_time_us: first_time,
                last_device_time_us: last_time,
                max_fit_residual_us: 3,
                quality_flags: TIMING_VALID,
            },
            samples,
        }
    }

    #[test]
    fn recovers_samples_gaps_breaks_and_an_explicit_seal() {
        let temp = TempWal::new();
        let mut sink = CaptureWalSink::open(&temp.path, config(1_000_000)).unwrap();
        sink.persist_chunk(
            HARDWARE,
            &chunk(
                0,
                &[[SampleAxis::MIN, 0, SampleAxis::MAX], [-1, 1, 17]],
                10,
                20,
            ),
        )
        .unwrap();
        sink.persist_gap(
            Gap::new(
                HARDWARE,
                BOOT,
                SampleSequence::new(2),
                2,
                GapReason::SourceLoss,
            )
            .unwrap(),
        )
        .unwrap();
        sink.persist_continuity_break(
            ContinuityBreak::new(
                HARDWARE,
                BOOT,
                SequenceCursor::new(4),
                ContinuityBreakReason::SensorReadUncertain,
            )
            .unwrap(),
        )
        .unwrap();
        sink.persist_chunk(HARDWARE, &chunk(5, &[[2, 3, 4]], 30, 30))
            .unwrap();
        let manifest = sink.finish_window().unwrap().unwrap();
        assert_eq!(manifest.end_exclusive, SequenceCursor::new(6));
        drop(sink);

        let recovered = CaptureWalSink::open(&temp.path, config(1_000_000)).unwrap();
        assert_eq!(recovered.durable_cursor(), SequenceCursor::new(6));
        assert_eq!(recovered.recovery().record_count, 5);
        assert_eq!(recovered.recovery().sample_count, 3);
        assert_eq!(recovered.recovery().missing_sample_count, 2);
        assert_eq!(recovered.recovery().continuity_break_count, 1);
        assert_eq!(recovered.recovery().sealed_window_count, 1);
        assert_eq!(recovered.recovery().last_sealed_window, Some(manifest));
    }

    #[test]
    fn truncates_an_incomplete_tail_to_the_last_verified_record() {
        let temp = TempWal::new();
        let mut sink = CaptureWalSink::open(&temp.path, config(1_000_000)).unwrap();
        sink.persist_chunk(HARDWARE, &chunk(0, &[[1, 2, 3]], 10, 10))
            .unwrap();
        let valid_len = sink.durable_len;
        drop(sink);

        let mut file = OpenOptions::new().append(true).open(&temp.path).unwrap();
        file.write_all(&[0xaa, 0xbb, 0xcc]).unwrap();
        file.sync_all().unwrap();
        drop(file);

        let recovered = CaptureWalSink::open(&temp.path, config(1_000_000)).unwrap();
        assert_eq!(recovered.durable_cursor(), SequenceCursor::new(1));
        assert_eq!(recovered.recovery().truncated_tail_bytes, 3);
        assert_eq!(std::fs::metadata(&temp.path).unwrap().len(), valid_len);
    }

    #[test]
    fn replays_a_synced_record_after_crash_before_in_memory_commit() {
        let temp = TempWal::new();
        let sink = CaptureWalSink::open(&temp.path, config(1_000_000)).unwrap();
        drop(sink);

        let record = encode_chunk_record(&chunk(0, &[[7, 8, 9]], 10, 10)).unwrap();
        let mut file = OpenOptions::new().append(true).open(&temp.path).unwrap();
        file.write_all(&record).unwrap();
        file.sync_all().unwrap();
        drop(file);

        let recovered = CaptureWalSink::open(&temp.path, config(1_000_000)).unwrap();
        assert_eq!(recovered.durable_cursor(), SequenceCursor::new(1));
        assert_eq!(recovered.recovery().record_count, 1);
    }

    #[test]
    fn batch_exposes_no_durable_progress_before_its_single_commit() {
        let temp = TempWal::new();
        let mut sink = CaptureWalSink::open(&temp.path, config(1_000_000)).unwrap();
        let initial_len = std::fs::metadata(&temp.path).unwrap().len();

        let mut batch = sink.begin_batch().unwrap();
        assert_eq!(
            batch
                .stage_chunk(HARDWARE, &chunk(0, &[[1, 2, 3]], 10, 10))
                .unwrap(),
            SequenceCursor::new(1)
        );
        assert_eq!(
            batch
                .stage_gap(
                    Gap::new(
                        HARDWARE,
                        BOOT,
                        SampleSequence::new(1),
                        2,
                        GapReason::SourceLoss,
                    )
                    .unwrap(),
                )
                .unwrap(),
            SequenceCursor::new(3)
        );
        batch
            .stage_chunk(HARDWARE, &chunk(3, &[[4, 5, 6]], 20, 20))
            .unwrap();

        assert_eq!(batch.record_count(), 3);
        assert!(batch.byte_len() > 0);
        assert_eq!(batch.durable_cursor_after_commit(), SequenceCursor::new(4));
        assert_eq!(sink.durable_cursor(), SequenceCursor::ORIGIN);
        assert_eq!(sink.recovery().record_count, 0);
        assert_eq!(std::fs::metadata(&temp.path).unwrap().len(), initial_len);

        sink.commit_batch(batch).unwrap();
        assert_eq!(sink.durable_cursor(), SequenceCursor::new(4));
        assert_eq!(sink.recovery().record_count, 3);
        assert_eq!(sink.recovery().sample_count, 2);
        assert_eq!(sink.recovery().missing_sample_count, 2);
        drop(sink);

        let recovered = CaptureWalSink::open(&temp.path, config(1_000_000)).unwrap();
        assert_eq!(recovered.durable_cursor(), SequenceCursor::new(4));
        assert_eq!(recovered.recovery().record_count, 3);
    }

    #[test]
    fn batch_stops_at_a_window_boundary() {
        let temp = TempWal::new();
        let mut sink = CaptureWalSink::open(&temp.path, config(10)).unwrap();
        let mut batch = sink.begin_batch().unwrap();
        batch
            .stage_chunk(HARDWARE, &chunk(0, &[[1, 2, 3]], 0, 0))
            .unwrap();
        batch
            .stage_chunk(HARDWARE, &chunk(1, &[[4, 5, 6]], 20, 20))
            .unwrap();
        assert!(batch.seals_window());
        assert!(matches!(
            batch.stage_chunk(HARDWARE, &chunk(2, &[[7, 8, 9]], 30, 30)),
            Err(CaptureWalError::BatchCrossesWindowBoundary)
        ));

        sink.commit_batch(batch).unwrap();
        assert_eq!(sink.recovery().sealed_window_count, 1);
        assert_eq!(sink.durable_cursor(), SequenceCursor::new(2));
    }

    #[test]
    fn rejects_empty_and_stale_batches_without_poisoning_the_sink() {
        let temp = TempWal::new();
        let mut sink = CaptureWalSink::open(&temp.path, config(1_000_000)).unwrap();
        let empty = sink.begin_batch().unwrap();
        assert!(matches!(
            sink.commit_batch(empty),
            Err(CaptureWalError::EmptyBatch)
        ));

        let mut stale = sink.begin_batch().unwrap();
        stale
            .stage_chunk(HARDWARE, &chunk(0, &[[1, 2, 3]], 10, 10))
            .unwrap();
        sink.persist_chunk(HARDWARE, &chunk(0, &[[1, 2, 3]], 10, 10))
            .unwrap();
        assert!(matches!(
            sink.commit_batch(stale),
            Err(CaptureWalError::StaleBatch)
        ));
        assert!(!sink.is_poisoned());
        assert_eq!(sink.durable_cursor(), SequenceCursor::new(1));
    }

    #[test]
    fn checksum_corruption_fails_closed_instead_of_discarding_durable_data() {
        let temp = TempWal::new();
        let mut sink = CaptureWalSink::open(&temp.path, config(1_000_000)).unwrap();
        sink.persist_chunk(HARDWARE, &chunk(0, &[[1, 2, 3]], 10, 10))
            .unwrap();
        sink.persist_chunk(HARDWARE, &chunk(1, &[[4, 5, 6]], 20, 20))
            .unwrap();
        drop(sink);

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&temp.path)
            .unwrap();
        let corrupt_at = FILE_HEADER_BYTES as u64 + RECORD_PREFIX_BYTES as u64 + 10;
        file.seek(SeekFrom::Start(corrupt_at)).unwrap();
        let mut byte = [0_u8; 1];
        file.read_exact(&mut byte).unwrap();
        file.seek(SeekFrom::Start(corrupt_at)).unwrap();
        file.write_all(&[byte[0] ^ 0x80]).unwrap();
        file.sync_all().unwrap();
        drop(file);

        assert!(matches!(
            CaptureWalSink::open(&temp.path, config(1_000_000)),
            Err(CaptureWalError::RecordChecksumMismatch { .. })
        ));
    }

    #[test]
    fn checksum_corruption_of_the_last_complete_record_also_fails_closed() {
        let temp = TempWal::new();
        let mut sink = CaptureWalSink::open(&temp.path, config(1_000_000)).unwrap();
        sink.persist_chunk(HARDWARE, &chunk(0, &[[1, 2, 3]], 10, 10))
            .unwrap();
        drop(sink);

        let length = std::fs::metadata(&temp.path).unwrap().len();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&temp.path)
            .unwrap();
        file.seek(SeekFrom::Start(length - 1)).unwrap();
        let mut byte = [0_u8; 1];
        file.read_exact(&mut byte).unwrap();
        file.seek(SeekFrom::Start(length - 1)).unwrap();
        file.write_all(&[byte[0] ^ 1]).unwrap();
        file.sync_all().unwrap();
        drop(file);

        assert!(matches!(
            CaptureWalSink::open(&temp.path, config(1_000_000)),
            Err(CaptureWalError::RecordChecksumMismatch { .. })
        ));
    }

    #[test]
    fn rejects_opening_a_stream_with_different_identity_or_policy() {
        let temp = TempWal::new();
        drop(CaptureWalSink::open(&temp.path, config(1_000_000)).unwrap());

        let mut wrong_hardware = config(1_000_000);
        wrong_hardware.hardware_id = HardwareId::from_bytes([8, 7, 6, 5, 4, 3, 2, 1]);
        assert!(matches!(
            CaptureWalSink::open(&temp.path, wrong_hardware),
            Err(CaptureWalError::HardwareMismatch)
        ));
        assert!(matches!(
            CaptureWalSink::open(&temp.path, config(2_000_000)),
            Err(CaptureWalError::WindowPolicyMismatch)
        ));
    }

    #[test]
    fn read_only_reader_preserves_records_and_never_repairs_a_tail() {
        let temp = TempWal::new();
        let expected = chunk(0, &[[SampleAxis::MIN, 0, SampleAxis::MAX]], 10, 10);
        let mut sink = CaptureWalSink::open(&temp.path, config(1_000_000)).unwrap();
        sink.persist_chunk(HARDWARE, &expected).unwrap();
        sink.finish_window().unwrap();
        drop(sink);

        let mut file = OpenOptions::new().append(true).open(&temp.path).unwrap();
        file.write_all(&[0xaa, 0xbb]).unwrap();
        file.sync_all().unwrap();
        drop(file);
        let length_with_tail = std::fs::metadata(&temp.path).unwrap().len();

        let mut reader = CaptureWalReader::open(&temp.path).unwrap();
        assert_eq!(reader.header().hardware_id, HARDWARE);
        assert_eq!(
            reader.next_record().unwrap(),
            Some(CaptureRecord::SampleChunk(expected))
        );
        assert_eq!(
            reader.next_record().unwrap(),
            Some(CaptureRecord::WindowSeal)
        );
        assert!(matches!(
            reader.next_record(),
            Err(CaptureWalError::IncompleteRecord { .. })
        ));
        assert_eq!(
            std::fs::metadata(&temp.path).unwrap().len(),
            length_with_tail
        );
    }
}
