use core::fmt;
use std::path::PathBuf;

use sensor_core::{BootEpoch, ContinuityBreak, Gap, HardwareId, SequenceCursor};
use sensor_protocol::SampleChunk;

use crate::bounded_writer::{
    GroupDurableMeasurementSink, GroupPersistError, GroupPersistSuccess, PersistenceEnvelope,
    PersistenceRecord,
};
use crate::capture_segments::{CaptureSegmentError, CaptureSegmentStream};
use crate::capture_wal::{CaptureWalBatch, CaptureWalConfig};
use crate::capture_window::CaptureWindowManifest;
use crate::measurement_persistence::DurableMeasurementSink;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureStreamSpec {
    pub directory: PathBuf,
    pub config: CaptureWalConfig,
}

/// Bounded router from channel-level persistence to one WAL owner per sensor.
///
/// The set of streams is fixed at construction. Recording cannot accidentally
/// create a file for an unknown identity or a new boot epoch; stream recovery
/// must first reconcile that change explicitly.
pub struct CaptureRepository {
    streams: Vec<CaptureSegmentStream>,
}

/// Opaque group bound to one configured sensor stream and one physical WAL.
pub struct CaptureRepositoryBatch {
    hardware_id: HardwareId,
    boot_epoch: BootEpoch,
    wal: CaptureWalBatch,
}

impl CaptureRepositoryBatch {
    pub const fn hardware_id(&self) -> HardwareId {
        self.hardware_id
    }

    pub const fn boot_epoch(&self) -> BootEpoch {
        self.boot_epoch
    }

    pub fn record_count(&self) -> usize {
        self.wal.record_count()
    }

    pub fn byte_len(&self) -> usize {
        self.wal.byte_len()
    }

    pub const fn durable_cursor_after_commit(&self) -> SequenceCursor {
        self.wal.durable_cursor_after_commit()
    }

    pub const fn seals_window(&self) -> bool {
        self.wal.seals_window()
    }

    pub fn stage_chunk(
        &mut self,
        chunk: &SampleChunk,
    ) -> Result<SequenceCursor, CaptureRepositoryError> {
        self.wal
            .stage_chunk(self.hardware_id, chunk)
            .map_err(|error| CaptureRepositoryError::Segment(CaptureSegmentError::Wal(error)))
    }

    pub fn stage_gap(&mut self, gap: Gap) -> Result<SequenceCursor, CaptureRepositoryError> {
        self.wal
            .stage_gap(gap)
            .map_err(|error| CaptureRepositoryError::Segment(CaptureSegmentError::Wal(error)))
    }

    pub fn stage_continuity_break(
        &mut self,
        observed: ContinuityBreak,
    ) -> Result<SequenceCursor, CaptureRepositoryError> {
        self.wal
            .stage_continuity_break(observed)
            .map_err(|error| CaptureRepositoryError::Segment(CaptureSegmentError::Wal(error)))
    }
}

impl CaptureRepository {
    pub fn open(
        specs: impl IntoIterator<Item = CaptureStreamSpec>,
    ) -> Result<Self, CaptureRepositoryError> {
        let mut streams = Vec::new();
        for spec in specs {
            if streams.iter().any(|stream: &CaptureSegmentStream| {
                stream.hardware_id() == spec.config.hardware_id
                    && stream.boot_epoch() == spec.config.boot_epoch
            }) {
                return Err(CaptureRepositoryError::DuplicateStream {
                    hardware_id: spec.config.hardware_id,
                    boot_epoch: spec.config.boot_epoch,
                });
            }
            let sink = CaptureSegmentStream::open(spec.directory, spec.config)
                .map_err(CaptureRepositoryError::Segment)?;
            streams.push(sink);
        }
        if streams.is_empty() {
            return Err(CaptureRepositoryError::NoStreams);
        }
        Ok(Self { streams })
    }

    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    pub fn durable_cursors(
        &self,
    ) -> impl ExactSizeIterator<Item = (HardwareId, BootEpoch, sensor_core::SequenceCursor)> + '_
    {
        self.streams.iter().map(|stream| {
            (
                stream.hardware_id(),
                stream.boot_epoch(),
                stream.durable_cursor(),
            )
        })
    }

    pub fn finish_stream(
        &mut self,
        hardware_id: HardwareId,
        boot_epoch: BootEpoch,
    ) -> Result<Option<CaptureWindowManifest>, CaptureRepositoryError> {
        self.stream_mut(hardware_id, boot_epoch)?
            .finish_window()
            .map_err(CaptureRepositoryError::Segment)
    }

    /// Seals every non-empty current window after all submitted writes have
    /// become durable. All streams are attempted so one local failure does not
    /// leave unrelated streams needlessly open; the first error is reported.
    pub fn finish_all_streams(
        &mut self,
    ) -> Result<Vec<CaptureWindowManifest>, CaptureRepositoryError> {
        let mut manifests = Vec::with_capacity(self.streams.len());
        let mut first_error = None;
        for stream in &mut self.streams {
            match stream.finish_window() {
                Ok(Some(manifest)) => manifests.push(manifest),
                Ok(None) => {}
                Err(error) if first_error.is_none() => {
                    first_error = Some(CaptureRepositoryError::Segment(error));
                }
                Err(_) => {}
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(manifests),
        }
    }

    pub fn begin_batch(
        &mut self,
        hardware_id: HardwareId,
        boot_epoch: BootEpoch,
    ) -> Result<CaptureRepositoryBatch, CaptureRepositoryError> {
        let wal = self
            .stream_mut(hardware_id, boot_epoch)?
            .begin_batch()
            .map_err(CaptureRepositoryError::Segment)?;
        Ok(CaptureRepositoryBatch {
            hardware_id,
            boot_epoch,
            wal,
        })
    }

    pub fn commit_batch(
        &mut self,
        batch: CaptureRepositoryBatch,
    ) -> Result<(), CaptureRepositoryError> {
        self.stream_mut(batch.hardware_id, batch.boot_epoch)?
            .commit_batch(batch.wal)
            .map_err(CaptureRepositoryError::Segment)
    }

    #[cfg(test)]
    pub(crate) fn inject_failure_before_next_sync(
        &mut self,
        hardware_id: HardwareId,
        boot_epoch: BootEpoch,
    ) -> Result<(), CaptureRepositoryError> {
        self.stream_mut(hardware_id, boot_epoch)?
            .inject_failure_before_next_sync();
        Ok(())
    }

    fn stream_mut(
        &mut self,
        hardware_id: HardwareId,
        boot_epoch: BootEpoch,
    ) -> Result<&mut CaptureSegmentStream, CaptureRepositoryError> {
        self.streams
            .iter_mut()
            .find(|stream| stream.hardware_id() == hardware_id && stream.boot_epoch() == boot_epoch)
            .ok_or(CaptureRepositoryError::UnknownStream {
                hardware_id,
                boot_epoch,
            })
    }
}

impl DurableMeasurementSink for CaptureRepository {
    type Error = CaptureRepositoryError;

    fn persist_chunk(
        &mut self,
        hardware_id: HardwareId,
        chunk: &SampleChunk,
    ) -> Result<(), Self::Error> {
        self.stream_mut(hardware_id, chunk.header.boot_epoch)?
            .persist_chunk(hardware_id, chunk)
            .map_err(CaptureRepositoryError::Segment)
    }

    fn persist_gap(&mut self, gap: Gap) -> Result<(), Self::Error> {
        self.stream_mut(gap.hardware_id, gap.boot_epoch)?
            .persist_gap(gap)
            .map_err(CaptureRepositoryError::Segment)
    }

    fn persist_continuity_break(&mut self, observed: ContinuityBreak) -> Result<(), Self::Error> {
        self.stream_mut(observed.hardware_id, observed.boot_epoch)?
            .persist_continuity_break(observed)
            .map_err(CaptureRepositoryError::Segment)
    }
}

impl GroupDurableMeasurementSink for CaptureRepository {
    fn persist_group(
        &mut self,
        envelopes: &[&PersistenceEnvelope],
    ) -> Result<GroupPersistSuccess, GroupPersistError<Self::Error>> {
        let Some(first) = envelopes.first() else {
            return Ok(GroupPersistSuccess {
                persisted_records: 0,
            });
        };
        let mut batch = self
            .begin_batch(first.hardware_id(), first.boot_epoch())
            .map_err(GroupPersistError::Preparation)?;
        let mut staged = 0_usize;
        for (index, envelope) in envelopes.iter().enumerate() {
            if envelope.hardware_id() != batch.hardware_id()
                || envelope.boot_epoch() != batch.boot_epoch()
                || (staged > 0 && envelope.start_cursor() != batch.durable_cursor_after_commit())
            {
                break;
            }
            let result = match envelope.record() {
                PersistenceRecord::Chunk { chunk, .. } => batch.stage_chunk(chunk),
                PersistenceRecord::Gap(gap) => batch.stage_gap(*gap),
                PersistenceRecord::ContinuityBreak(observed) => {
                    batch.stage_continuity_break(*observed)
                }
            };
            match result {
                Ok(_) => staged += 1,
                Err(error) if staged == 0 => {
                    debug_assert_eq!(index, 0);
                    return Err(GroupPersistError::Preparation(error));
                }
                Err(_) => break,
            }
            if batch.seals_window() {
                break;
            }
        }
        self.commit_batch(batch)
            .map_err(|error| GroupPersistError::Commit {
                attempted_records: staged,
                error,
            })?;
        Ok(GroupPersistSuccess {
            persisted_records: staged,
        })
    }
}

#[derive(Debug)]
pub enum CaptureRepositoryError {
    NoStreams,
    DuplicateStream {
        hardware_id: HardwareId,
        boot_epoch: BootEpoch,
    },
    UnknownStream {
        hardware_id: HardwareId,
        boot_epoch: BootEpoch,
    },
    Segment(CaptureSegmentError),
}

impl fmt::Display for CaptureRepositoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Segment(error) => write!(formatter, "capture repository segment failed: {error}"),
            other => write!(formatter, "capture repository error: {other:?}"),
        }
    }
}

impl std::error::Error for CaptureRepositoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Segment(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    use sensor_core::{GapReason, SampleAxes, SampleAxis, SampleSequence, SequenceCursor};
    use sensor_protocol::{SampleChunkHeader, TimingEvidence, TIMING_VALID};

    use crate::capture_window::CaptureWindowPolicy;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sensor-host-capture-repository-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn spec(root: &std::path::Path, id: u8, epoch: u64) -> CaptureStreamSpec {
        CaptureStreamSpec {
            directory: root.join(format!("node-{id}")),
            config: CaptureWalConfig {
                hardware_id: HardwareId::from_bytes([id, 0, 0, 0, 0, 0, 0, 0]),
                boot_epoch: BootEpoch::new(epoch),
                recovered_cursor: SequenceCursor::ORIGIN,
                first_window_ordinal: 0,
                window_policy: CaptureWindowPolicy::new(600_000_000).unwrap(),
            },
        }
    }

    fn chunk(epoch: u64, value: i32) -> SampleChunk {
        let axis = SampleAxis::new(value).unwrap();
        SampleChunk {
            header: SampleChunkHeader {
                boot_epoch: BootEpoch::new(epoch),
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
                max_fit_residual_us: 1,
                quality_flags: TIMING_VALID,
            },
            samples: vec![SampleAxes::new(axis, axis, axis)],
        }
    }

    #[test]
    fn routes_multiple_nodes_without_mixing_their_cursors() {
        let temp = TempDirectory::new();
        let first = spec(&temp.0, 1, 10);
        let second = spec(&temp.0, 2, 20);
        let mut repository = CaptureRepository::open([first.clone(), second.clone()]).unwrap();

        repository
            .persist_chunk(first.config.hardware_id, &chunk(10, 11))
            .unwrap();
        repository
            .persist_chunk(second.config.hardware_id, &chunk(20, 22))
            .unwrap();
        drop(repository);

        let recovered = CaptureRepository::open([first, second]).unwrap();
        let states = recovered.durable_cursors().collect::<Vec<_>>();
        assert_eq!(states.len(), 2);
        assert!(states
            .iter()
            .all(|(_, _, cursor)| *cursor == SequenceCursor::new(1)));
    }

    #[test]
    fn commits_one_stream_batch_without_advancing_another_stream() {
        let temp = TempDirectory::new();
        let first = spec(&temp.0, 1, 10);
        let second = spec(&temp.0, 2, 20);
        let mut repository = CaptureRepository::open([first.clone(), second.clone()]).unwrap();

        let mut batch = repository
            .begin_batch(first.config.hardware_id, first.config.boot_epoch)
            .unwrap();
        batch.stage_chunk(&chunk(10, 11)).unwrap();
        batch
            .stage_gap(
                Gap::new(
                    first.config.hardware_id,
                    first.config.boot_epoch,
                    SampleSequence::new(1),
                    2,
                    GapReason::SourceLoss,
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(batch.record_count(), 2);
        assert_eq!(batch.durable_cursor_after_commit(), SequenceCursor::new(3));
        assert!(repository
            .durable_cursors()
            .all(|(_, _, cursor)| cursor == SequenceCursor::ORIGIN));

        repository.commit_batch(batch).unwrap();
        let states = repository.durable_cursors().collect::<Vec<_>>();
        assert!(states.iter().any(|(hardware_id, _, cursor)| {
            *hardware_id == first.config.hardware_id && *cursor == SequenceCursor::new(3)
        }));
        assert!(states.iter().any(|(hardware_id, _, cursor)| {
            *hardware_id == second.config.hardware_id && *cursor == SequenceCursor::ORIGIN
        }));
    }

    #[test]
    fn group_sink_persists_only_the_contiguous_prefix_of_one_stream() {
        let temp = TempDirectory::new();
        let first = spec(&temp.0, 1, 10);
        let second = spec(&temp.0, 2, 20);
        let mut repository = CaptureRepository::open([first.clone(), second.clone()]).unwrap();
        let records = [
            PersistenceEnvelope::chunk(first.config.hardware_id, chunk(10, 11)).unwrap(),
            PersistenceEnvelope::gap(
                Gap::new(
                    first.config.hardware_id,
                    first.config.boot_epoch,
                    SampleSequence::new(1),
                    2,
                    GapReason::SourceLoss,
                )
                .unwrap(),
            ),
            PersistenceEnvelope::chunk(second.config.hardware_id, chunk(20, 22)).unwrap(),
        ];

        assert_eq!(
            repository
                .persist_group(&records.iter().collect::<Vec<_>>())
                .unwrap(),
            GroupPersistSuccess {
                persisted_records: 2
            }
        );
        let states = repository.durable_cursors().collect::<Vec<_>>();
        assert!(states.iter().any(|(hardware_id, _, cursor)| {
            *hardware_id == first.config.hardware_id && *cursor == SequenceCursor::new(3)
        }));
        assert!(states.iter().any(|(hardware_id, _, cursor)| {
            *hardware_id == second.config.hardware_id && *cursor == SequenceCursor::ORIGIN
        }));
    }

    #[test]
    fn rejects_unknown_boot_epoch_and_duplicate_stream_owners() {
        let temp = TempDirectory::new();
        let configured = spec(&temp.0, 1, 10);
        let mut repository = CaptureRepository::open([configured.clone()]).unwrap();
        assert!(matches!(
            repository.persist_chunk(configured.config.hardware_id, &chunk(11, 1)),
            Err(CaptureRepositoryError::UnknownStream { .. })
        ));

        assert!(matches!(
            CaptureRepository::open([configured.clone(), configured]),
            Err(CaptureRepositoryError::DuplicateStream { .. })
        ));
    }
}
