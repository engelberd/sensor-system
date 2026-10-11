use core::fmt;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use crate::capture_coverage_checkpoint::{
    CaptureCoverageCheckpointError, CaptureCoverageCheckpointStore,
};
use crate::capture_segments::{
    capture_segment_paths, read_capture_manifest, validate_capture_segment, CaptureSegmentError,
};
use crate::retention_checkpoint::{SegmentKey, TrustedUnixSeconds};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureCleanupTarget {
    pub stream_directory: PathBuf,
    pub checkpoint: PathBuf,
    pub stream_protection: CaptureStreamProtection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureStreamProtection {
    /// The writer still owns this boot stream. Only older ordinals are safe.
    ActiveWindow(u64),
    /// The boot stream has been closed and has no writer that can reference it.
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureCleanupOutcome {
    NotDue,
    Deleted,
    AlreadyRecorded,
}

/// Deletes one sealed Capture segment only after its durable checkpoint
/// authorizes the operation. The WAL is removed before the manifest, the
/// directory is synchronized, and only then is deletion recorded.
pub fn cleanup_capture_segment(
    target: &CaptureCleanupTarget,
    now: TrustedUnixSeconds,
) -> Result<CaptureCleanupOutcome, RetentionCleanupError> {
    cleanup_capture_segment_inner(target, now, None)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FaultPoint {
    AfterWalRemoval,
    AfterDirectorySync,
}

fn cleanup_capture_segment_inner(
    target: &CaptureCleanupTarget,
    now: TrustedUnixSeconds,
    fault: Option<FaultPoint>,
) -> Result<CaptureCleanupOutcome, RetentionCleanupError> {
    let mut checkpoint = CaptureCoverageCheckpointStore::open(&target.checkpoint)
        .map_err(RetentionCleanupError::Checkpoint)?;
    let segment = checkpoint.state().segment();
    let paths = capture_segment_paths(&target.stream_directory, segment.window_ordinal);

    if checkpoint.state().deleted_at().is_some() {
        if paths.wal.exists() || paths.manifest.exists() {
            return Err(RetentionCleanupError::ArtifactsPresentAfterRecordedDeletion);
        }
        return Ok(CaptureCleanupOutcome::AlreadyRecorded);
    }
    if !checkpoint.state().deletable(now) {
        return Ok(CaptureCleanupOutcome::NotDue);
    }
    if matches!(
        target.stream_protection,
        CaptureStreamProtection::ActiveWindow(active) if segment.window_ordinal >= active
    ) {
        return Err(RetentionCleanupError::ActiveWindowProtected);
    }

    match (paths.wal.exists(), paths.manifest.exists()) {
        (true, true) => {
            let validated = validate_capture_segment(&paths.wal, &paths.manifest)
                .map_err(RetentionCleanupError::Segment)?;
            require_key(segment, SegmentKey::from(validated.manifest))?;
        }
        (false, true) => {
            let manifest =
                read_capture_manifest(&paths.manifest).map_err(RetentionCleanupError::Segment)?;
            require_key(segment, SegmentKey::from(manifest))?;
        }
        (true, false) => return Err(RetentionCleanupError::WalWithoutManifest),
        (false, false) => {}
    }

    remove_if_present(&paths.wal)?;
    if fault == Some(FaultPoint::AfterWalRemoval) {
        return Err(RetentionCleanupError::InjectedFault);
    }
    remove_if_present(&paths.manifest)?;
    sync_directory(&target.stream_directory)?;
    if fault == Some(FaultPoint::AfterDirectorySync) {
        return Err(RetentionCleanupError::InjectedFault);
    }
    checkpoint
        .mark_deleted(now)
        .map_err(RetentionCleanupError::Checkpoint)?;
    Ok(CaptureCleanupOutcome::Deleted)
}

fn require_key(expected: SegmentKey, actual: SegmentKey) -> Result<(), RetentionCleanupError> {
    if expected == actual {
        Ok(())
    } else {
        Err(RetentionCleanupError::SegmentIdentityMismatch)
    }
}

fn remove_if_present(path: &Path) -> Result<(), RetentionCleanupError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RetentionCleanupError::Io(error)),
    }
}

fn sync_directory(path: &Path) -> Result<(), RetentionCleanupError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(RetentionCleanupError::Io)
}

#[derive(Debug)]
pub enum RetentionCleanupError {
    Checkpoint(CaptureCoverageCheckpointError),
    Segment(CaptureSegmentError),
    Io(io::Error),
    WalWithoutManifest,
    SegmentIdentityMismatch,
    ArtifactsPresentAfterRecordedDeletion,
    ActiveWindowProtected,
    InjectedFault,
}

impl fmt::Display for RetentionCleanupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Checkpoint(error) => write!(formatter, "cleanup checkpoint failed: {error}"),
            Self::Segment(error) => write!(formatter, "cleanup segment validation failed: {error}"),
            Self::Io(error) => write!(formatter, "cleanup filesystem operation failed: {error}"),
            other => write!(formatter, "capture cleanup error: {other:?}"),
        }
    }
}

impl std::error::Error for RetentionCleanupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Checkpoint(error) => Some(error),
            Self::Segment(error) => Some(error),
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    use sensor_core::{BootEpoch, HardwareId, SampleAxes, SampleAxis, SequenceCursor};
    use sensor_protocol::{SampleChunk, SampleChunkHeader, TimingEvidence, TIMING_VALID};

    use crate::archive_coverage::{
        prove_capture_coverage, ArchiveCoverageIndex, ArchiveInputSpan, ArchivePartition,
        ArchiveRevision, ReplicatedArchiveCoverage,
    };
    use crate::capture_coverage_checkpoint::CaptureCoverageCheckpointStore;
    use crate::capture_segments::CaptureSegmentStream;
    use crate::capture_wal::CaptureWalConfig;
    use crate::capture_window::CaptureWindowPolicy;
    use crate::measurement_persistence::DurableMeasurementSink;
    use crate::retention_checkpoint::{ArchiveIdentity, RetentionPolicy};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sensor-host-retention-cleanup-{}-{id}",
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

    fn prepared_target(capture_grace_seconds: u64) -> (TempDirectory, CaptureCleanupTarget) {
        let temp = TempDirectory::new();
        let stream_directory = temp.0.join("capture");
        let hardware_id = HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);
        let boot_epoch = BootEpoch::new(9);
        let mut stream = CaptureSegmentStream::open(
            &stream_directory,
            CaptureWalConfig {
                hardware_id,
                boot_epoch,
                recovered_cursor: SequenceCursor::ORIGIN,
                first_window_ordinal: 0,
                window_policy: CaptureWindowPolicy::new(600_000_000).unwrap(),
            },
        )
        .unwrap();
        let axis = SampleAxis::new(7).unwrap();
        stream
            .persist_chunk(
                hardware_id,
                &SampleChunk {
                    header: SampleChunkHeader {
                        boot_epoch,
                        retained_from: SequenceCursor::ORIGIN,
                        produced: SequenceCursor::new(1),
                        first: SequenceCursor::ORIGIN,
                        sample_count: 1,
                    },
                    config_revision: 1,
                    timing: TimingEvidence {
                        segment_id: 1,
                        first_device_time_us: 10,
                        last_device_time_us: 10,
                        max_fit_residual_us: 0,
                        quality_flags: TIMING_VALID,
                    },
                    samples: vec![SampleAxes::new(axis, axis, axis)],
                },
            )
            .unwrap();
        let manifest = stream.finish_window().unwrap().unwrap();
        drop(stream);

        let checkpoint_path = temp.0.join("checkpoints/window-0.coverage");
        let archive = ArchiveIdentity {
            product_id: [1; 16],
            sha256: [2; 32],
        };
        let segment = SegmentKey::from(manifest);
        let coverage = ArchiveCoverageIndex::new(
            archive,
            hardware_id,
            ArchivePartition::UtcHour {
                start: TrustedUnixSeconds::new(0),
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
        let proof = prove_capture_coverage(
            segment,
            &[ReplicatedArchiveCoverage {
                index: &coverage,
                verified_at: TrustedUnixSeconds::new(101),
                acknowledged_at: TrustedUnixSeconds::new(102),
            }],
        )
        .unwrap();
        CaptureCoverageCheckpointStore::create(
            &checkpoint_path,
            proof,
            RetentionPolicy::new(capture_grace_seconds, 100).unwrap(),
        )
        .unwrap();

        let target = CaptureCleanupTarget {
            stream_directory,
            checkpoint: checkpoint_path,
            stream_protection: CaptureStreamProtection::Closed,
        };
        (temp, target)
    }

    #[test]
    fn cleanup_requires_the_persisted_coverage_deadline() {
        let (_temp, target) = prepared_target(10);
        assert_eq!(
            cleanup_capture_segment(&target, TrustedUnixSeconds::new(111)).unwrap(),
            CaptureCleanupOutcome::NotDue
        );
        assert_eq!(
            cleanup_capture_segment(&target, TrustedUnixSeconds::new(112)).unwrap(),
            CaptureCleanupOutcome::Deleted
        );
        assert_eq!(
            cleanup_capture_segment(&target, TrustedUnixSeconds::new(113)).unwrap(),
            CaptureCleanupOutcome::AlreadyRecorded
        );
    }

    #[test]
    fn crash_after_unlink_is_reconciled_without_weakening_the_checkpoint() {
        let (_temp, target) = prepared_target(1);

        assert!(matches!(
            cleanup_capture_segment_inner(
                &target,
                TrustedUnixSeconds::new(103),
                Some(FaultPoint::AfterWalRemoval)
            ),
            Err(RetentionCleanupError::InjectedFault)
        ));
        assert!(CaptureCoverageCheckpointStore::open(&target.checkpoint)
            .unwrap()
            .state()
            .deleted_at()
            .is_none());
        assert_eq!(
            cleanup_capture_segment(&target, TrustedUnixSeconds::new(103)).unwrap(),
            CaptureCleanupOutcome::Deleted
        );
    }

    #[test]
    fn checkpoint_is_updated_only_after_directory_sync() {
        let (_temp, target) = prepared_target(1);

        assert!(matches!(
            cleanup_capture_segment_inner(
                &target,
                TrustedUnixSeconds::new(103),
                Some(FaultPoint::AfterDirectorySync)
            ),
            Err(RetentionCleanupError::InjectedFault)
        ));
        assert!(CaptureCoverageCheckpointStore::open(&target.checkpoint)
            .unwrap()
            .state()
            .deleted_at()
            .is_none());
        assert_eq!(
            cleanup_capture_segment(&target, TrustedUnixSeconds::new(103)).unwrap(),
            CaptureCleanupOutcome::Deleted
        );
    }

    #[test]
    fn active_writer_window_is_never_unlinked() {
        let (_temp, mut target) = prepared_target(1);
        target.stream_protection = CaptureStreamProtection::ActiveWindow(0);
        assert!(matches!(
            cleanup_capture_segment(&target, TrustedUnixSeconds::new(103)),
            Err(RetentionCleanupError::ActiveWindowProtected)
        ));
        let paths = capture_segment_paths(&target.stream_directory, 0);
        assert!(paths.wal.exists());
        assert!(paths.manifest.exists());
    }
}
