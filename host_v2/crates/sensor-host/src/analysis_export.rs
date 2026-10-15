use core::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use sensor_core::{BootEpoch, HardwareId};
use serde_json::json;

use crate::capture_segments::{
    capture_segment_paths, validate_capture_segment, CaptureSegmentError,
};
use crate::capture_wal::{CaptureRecord, CaptureWalError, CaptureWalReader};
use crate::configuration_history::ConfigurationSnapshot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnalysisWindowIdentity<'a> {
    pub channel_name: &'a str,
    pub channel_id: u8,
    pub node_address: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnalysisWindowSummary {
    pub csv_path: PathBuf,
    pub sample_count: u64,
    pub missing_sample_count: u64,
    pub continuity_break_count: u64,
    pub first_device_time_us: u64,
    pub estimated_odr_hz: Option<f64>,
    pub already_exists: bool,
}

/// Produces one analysis-friendly product from one immutable sealed Capture
/// window. Capture remains the source of truth; publication of this derived
/// view is atomic and can be retried without touching the WAL.
pub fn export_analysis_window(
    stream_directory: &Path,
    output_directory: &Path,
    ordinal: u64,
    identity: AnalysisWindowIdentity<'_>,
    configurations: &[ConfigurationSnapshot],
) -> Result<AnalysisWindowSummary, AnalysisExportError> {
    let paths = capture_segment_paths(stream_directory, ordinal);
    let validation = validate_capture_segment(&paths.wal, &paths.manifest)
        .map_err(AnalysisExportError::Segment)?;
    let manifest = validation.manifest;
    let first_device_time_us =
        first_sample_time(&paths.wal)?.ok_or(AnalysisExportError::WindowWithoutSamples)?;
    fs::create_dir_all(output_directory).map_err(AnalysisExportError::Io)?;

    let stem = format!(
        "device-t{first_device_time_us:020}us_ch{:02}_node{:03}_{}_boot{:016x}_w{ordinal:020}",
        identity.channel_id,
        identity.node_address,
        hardware_id_hex(manifest.hardware_id),
        manifest.boot_epoch.get(),
    );
    let csv_path = output_directory.join(format!("{stem}.csv"));
    if csv_path.exists() {
        return Ok(AnalysisWindowSummary {
            csv_path,
            sample_count: manifest.sample_count,
            missing_sample_count: manifest.missing_sample_count,
            continuity_break_count: manifest.continuity_break_count,
            first_device_time_us,
            estimated_odr_hz: None,
            already_exists: true,
        });
    }

    let events_path = output_directory.join(format!("{stem}.events.csv"));
    let metadata_path = output_directory.join(format!("{stem}.json"));
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let csv_temp = output_directory.join(format!(".{stem}.{nonce}.csv.tmp"));
    let events_temp = output_directory.join(format!(".{stem}.{nonce}.events.tmp"));
    let metadata_temp = output_directory.join(format!(".{stem}.{nonce}.json.tmp"));

    let result = write_window(
        &paths.wal,
        &csv_temp,
        &events_temp,
        &metadata_temp,
        identity,
        manifest.hardware_id,
        manifest.boot_epoch,
        ordinal,
        manifest.sample_count,
        manifest.missing_sample_count,
        manifest.continuity_break_count,
        configurations,
    );
    let written = match result {
        Ok(value) => value,
        Err(error) => {
            let _ = fs::remove_file(&csv_temp);
            let _ = fs::remove_file(&events_temp);
            let _ = fs::remove_file(&metadata_temp);
            return Err(error);
        }
    };

    fs::rename(&events_temp, &events_path).map_err(AnalysisExportError::Io)?;
    fs::rename(&metadata_temp, &metadata_path).map_err(AnalysisExportError::Io)?;
    // The primary CSV is the completion marker and is therefore published last.
    fs::rename(&csv_temp, &csv_path).map_err(AnalysisExportError::Io)?;
    sync_directory(output_directory)?;
    Ok(AnalysisWindowSummary {
        csv_path,
        sample_count: written.sample_count,
        missing_sample_count: manifest.missing_sample_count,
        continuity_break_count: manifest.continuity_break_count,
        first_device_time_us: written.first_device_time_us,
        estimated_odr_hz: written.estimated_odr_hz,
        already_exists: false,
    })
}

struct WrittenWindow {
    sample_count: u64,
    first_device_time_us: u64,
    estimated_odr_hz: Option<f64>,
}

#[allow(clippy::too_many_arguments)]
fn write_window(
    wal_path: &Path,
    csv_path: &Path,
    events_path: &Path,
    metadata_path: &Path,
    identity: AnalysisWindowIdentity<'_>,
    hardware_id: HardwareId,
    boot_epoch: BootEpoch,
    ordinal: u64,
    expected_samples: u64,
    missing_samples: u64,
    continuity_breaks: u64,
    configurations: &[ConfigurationSnapshot],
) -> Result<WrittenWindow, AnalysisExportError> {
    let mut samples = create_writer(csv_path)?;
    let mut events = create_writer(events_path)?;
    writeln!(
        samples,
        "sample_number,device_time_us,x_raw,y_raw,z_raw,output_odr_hz"
    )?;
    writeln!(events, "sample_number,event,count,reason")?;

    let mut reader = CaptureWalReader::open(wal_path).map_err(AnalysisExportError::Wal)?;
    let mut sample_count = 0_u64;
    let mut first_time = None;
    let mut prior = None::<(u64, u64)>;
    let mut interval_sum_us = 0_u128;
    let mut interval_count = 0_u64;
    while let Some(record) = reader.next_record().map_err(AnalysisExportError::Wal)? {
        match record {
            CaptureRecord::SampleChunk(chunk) => {
                let output_rate = configuration_for_chunk(
                    configurations,
                    hardware_id,
                    boot_epoch,
                    chunk.config_revision,
                    chunk.header.first.get(),
                )?;
                for (index, sample) in chunk.samples.iter().enumerate() {
                    let cursor = chunk
                        .header
                        .first
                        .get()
                        .checked_add(index as u64)
                        .ok_or(AnalysisExportError::CounterOverflow)?;
                    let device_time = interpolate_device_time(
                        chunk.timing.first_device_time_us,
                        chunk.timing.last_device_time_us,
                        chunk.samples.len(),
                        index,
                    )?;
                    first_time.get_or_insert(device_time);
                    if let Some((prior_cursor, prior_time)) = prior {
                        if cursor == prior_cursor.saturating_add(1) && device_time > prior_time {
                            interval_sum_us += u128::from(device_time - prior_time);
                            interval_count = interval_count
                                .checked_add(1)
                                .ok_or(AnalysisExportError::CounterOverflow)?;
                        }
                    }
                    prior = Some((cursor, device_time));
                    writeln!(
                        samples,
                        "{cursor},{device_time},{},{},{},{}",
                        sample.x.get(),
                        sample.y.get(),
                        sample.z.get(),
                        output_rate,
                    )?;
                    sample_count = sample_count
                        .checked_add(1)
                        .ok_or(AnalysisExportError::CounterOverflow)?;
                }
            }
            CaptureRecord::Gap(gap) => writeln!(
                events,
                "{},gap,{},\"{:?}\"",
                gap.first_missing.get(),
                gap.missing_count,
                gap.reason
            )?,
            CaptureRecord::ContinuityBreak(observed) => writeln!(
                events,
                "{},continuity_break,1,\"{:?}\"",
                observed.at.get(),
                observed.reason
            )?,
            CaptureRecord::WindowSeal => {}
        }
    }
    if sample_count != expected_samples {
        return Err(AnalysisExportError::SampleCountMismatch {
            expected: expected_samples,
            actual: sample_count,
        });
    }
    let first_device_time_us = first_time.ok_or(AnalysisExportError::WindowWithoutSamples)?;
    let estimated_odr_hz = if interval_count == 0 || interval_sum_us == 0 {
        None
    } else {
        Some(interval_count as f64 * 1_000_000.0 / interval_sum_us as f64)
    };
    samples.flush()?;
    events.flush()?;
    samples.get_ref().sync_all()?;
    events.get_ref().sync_all()?;

    let metadata = json!({
        "schema": "sensor-system-analysis-csv-v1",
        "channel_name": identity.channel_name,
        "channel_id": identity.channel_id,
        "node_address": identity.node_address,
        "hardware_id": hardware_id_hex(hardware_id),
        "boot_epoch": boot_epoch.get(),
        "window_ordinal": ordinal,
        "first_device_time_us": first_device_time_us,
        "file_time_basis": "node monotonic time since boot; not UTC",
        "sample_count": sample_count,
        "missing_sample_count": missing_samples,
        "continuity_break_count": continuity_breaks,
        "estimated_odr_hz": estimated_odr_hz,
        "estimated_odr_method": "mean interval between adjacent, consecutive samples",
        "axis_unit": "raw ADXL355 signed sample code",
        "source": "validated sealed Capture WAL"
    });
    let mut metadata_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(metadata_path)
        .map_err(AnalysisExportError::Io)?;
    serde_json::to_writer_pretty(&mut metadata_file, &metadata)
        .map_err(AnalysisExportError::Json)?;
    metadata_file.write_all(b"\n")?;
    metadata_file.sync_all()?;

    Ok(WrittenWindow {
        sample_count,
        first_device_time_us,
        estimated_odr_hz,
    })
}

fn configuration_for_chunk(
    configurations: &[ConfigurationSnapshot],
    hardware_id: HardwareId,
    boot_epoch: BootEpoch,
    revision: u32,
    first_cursor: u64,
) -> Result<&'static str, AnalysisExportError> {
    configurations
        .iter()
        .rev()
        .find(|snapshot| {
            snapshot.hardware_id == hardware_id
                && snapshot.boot_epoch == boot_epoch
                && snapshot.observed.revision == revision
                && snapshot.effective_sample_seq <= first_cursor
        })
        .map(|snapshot| snapshot.observed.desired.output_rate.hertz_label())
        .ok_or(AnalysisExportError::MissingConfiguration {
            boot_epoch,
            revision,
            first_cursor,
        })
}

fn first_sample_time(wal_path: &Path) -> Result<Option<u64>, AnalysisExportError> {
    let mut reader = CaptureWalReader::open(wal_path).map_err(AnalysisExportError::Wal)?;
    while let Some(record) = reader.next_record().map_err(AnalysisExportError::Wal)? {
        if let CaptureRecord::SampleChunk(chunk) = record {
            return Ok(Some(chunk.timing.first_device_time_us));
        }
    }
    Ok(None)
}

fn interpolate_device_time(
    first: u64,
    last: u64,
    count: usize,
    index: usize,
) -> Result<u64, AnalysisExportError> {
    if count == 0 || index >= count || last < first {
        return Err(AnalysisExportError::InvalidTiming);
    }
    if count == 1 {
        return (index == 0 && first == last)
            .then_some(first)
            .ok_or(AnalysisExportError::InvalidTiming);
    }
    let denominator = (count - 1) as u128;
    let offset = u128::from(last - first)
        .checked_mul(index as u128)
        .and_then(|value| value.checked_add(denominator / 2))
        .ok_or(AnalysisExportError::CounterOverflow)?
        / denominator;
    first
        .checked_add(u64::try_from(offset).map_err(|_| AnalysisExportError::CounterOverflow)?)
        .ok_or(AnalysisExportError::CounterOverflow)
}

fn create_writer(path: &Path) -> Result<BufWriter<File>, AnalysisExportError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(BufWriter::new)
        .map_err(AnalysisExportError::Io)
}

fn hardware_id_hex(hardware_id: HardwareId) -> String {
    hardware_id
        .bytes()
        .iter()
        .map(|value| format!("{value:02x}"))
        .collect()
}

fn sync_directory(path: &Path) -> Result<(), AnalysisExportError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(AnalysisExportError::Io)
}

#[derive(Debug)]
pub enum AnalysisExportError {
    Io(io::Error),
    Json(serde_json::Error),
    Segment(CaptureSegmentError),
    Wal(CaptureWalError),
    WindowWithoutSamples,
    InvalidTiming,
    CounterOverflow,
    MissingConfiguration {
        boot_epoch: BootEpoch,
        revision: u32,
        first_cursor: u64,
    },
    SampleCountMismatch {
        expected: u64,
        actual: u64,
    },
}

impl From<io::Error> for AnalysisExportError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl fmt::Display for AnalysisExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "analysis CSV export failed: {self:?}")
    }
}

impl std::error::Error for AnalysisExportError {}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_core::{SampleAxes, SampleAxis, SequenceCursor};
    use sensor_protocol::{
        Adxl355Odr, DecimationFilterProfile, MeasurementConfiguration, ObservedConfiguration,
        OutputRateCode, SampleChunk, SampleChunkHeader, TimingEvidence, TIMING_VALID,
    };

    use crate::capture_segments::CaptureSegmentStream;
    use crate::capture_wal::CaptureWalConfig;
    use crate::capture_window::CaptureWindowPolicy;
    use crate::measurement_persistence::DurableMeasurementSink;

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "sensor-analysis-export-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
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

    fn configuration() -> ConfigurationSnapshot {
        ConfigurationSnapshot {
            hardware_id: HARDWARE,
            boot_epoch: BOOT,
            observed: ObservedConfiguration {
                revision: 3,
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
            effective_sample_seq: 0,
        }
    }

    #[test]
    fn publishes_simple_idempotent_csv_from_one_sealed_window() {
        let temp = TempDirectory::new();
        let stream_dir = temp.0.join("capture");
        let output_dir = temp.0.join("analysis");
        let mut stream = CaptureSegmentStream::open(
            &stream_dir,
            CaptureWalConfig {
                hardware_id: HARDWARE,
                boot_epoch: BOOT,
                recovered_cursor: SequenceCursor::ORIGIN,
                first_window_ordinal: 0,
                window_policy: CaptureWindowPolicy::new(600_000_000).unwrap(),
            },
        )
        .unwrap();
        let samples = [
            SampleAxes::new(
                SampleAxis::new(10).unwrap(),
                SampleAxis::new(20).unwrap(),
                SampleAxis::new(30).unwrap(),
            ),
            SampleAxes::new(
                SampleAxis::new(11).unwrap(),
                SampleAxis::new(21).unwrap(),
                SampleAxis::new(31).unwrap(),
            ),
        ];
        stream
            .persist_chunk(
                HARDWARE,
                &SampleChunk {
                    header: SampleChunkHeader {
                        boot_epoch: BOOT,
                        retained_from: SequenceCursor::ORIGIN,
                        produced: SequenceCursor::new(2),
                        first: SequenceCursor::ORIGIN,
                        sample_count: 2,
                    },
                    config_revision: 3,
                    timing: TimingEvidence {
                        segment_id: 1,
                        first_device_time_us: 1_000_000,
                        last_device_time_us: 1_008_000,
                        max_fit_residual_us: 1,
                        quality_flags: TIMING_VALID,
                    },
                    samples: samples.to_vec(),
                },
            )
            .unwrap();
        stream.finish_window().unwrap().unwrap();

        let identity = AnalysisWindowIdentity {
            channel_name: "line-b",
            channel_id: 2,
            node_address: 1,
        };
        let first =
            export_analysis_window(&stream_dir, &output_dir, 0, identity, &[configuration()])
                .unwrap();
        assert!(!first.already_exists);
        assert_eq!(first.sample_count, 2);
        assert_eq!(first.first_device_time_us, 1_000_000);
        assert_eq!(first.estimated_odr_hz, Some(125.0));
        let csv = fs::read_to_string(&first.csv_path).unwrap();
        assert!(csv.starts_with("sample_number,device_time_us,x_raw,y_raw,z_raw,output_odr_hz\n"));
        assert!(csv.contains("0,1000000,10,20,30,125\n"));
        assert!(csv.contains("1,1008000,11,21,31,125\n"));
        assert!(first
            .csv_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("device-t00000000000001000000us_ch02_node001_0102030405060708"));

        let repeated =
            export_analysis_window(&stream_dir, &output_dir, 0, identity, &[configuration()])
                .unwrap();
        assert!(repeated.already_exists);
    }
}
