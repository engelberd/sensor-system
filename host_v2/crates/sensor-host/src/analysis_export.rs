use core::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use sensor_core::{BootEpoch, HardwareId};
use serde_json::json;

use crate::analysis_time::{
    estimate_first_sample_timestamp, AnalysisTimeError, AnalysisTimeRequest, AnalysisTimestamp,
};
use crate::capture_segments::{
    capture_segment_paths, validate_capture_segment, CaptureSegmentError,
};
use crate::capture_wal::{CaptureRecord, CaptureWalError, CaptureWalReader};
use crate::configuration_history::ConfigurationSnapshot;
use crate::telemetry_wal::{TelemetryWalError, TelemetryWalReader};

const STANDARD_GRAVITY_M_S2: f64 = 9.806_65;

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
    time_root: &Path,
    telemetry_wal_path: &Path,
    ordinal: u64,
    identity: AnalysisWindowIdentity<'_>,
    configurations: &[ConfigurationSnapshot],
) -> Result<AnalysisWindowSummary, AnalysisExportError> {
    let paths = capture_segment_paths(stream_directory, ordinal);
    let validation = validate_capture_segment(&paths.wal, &paths.manifest)
        .map_err(AnalysisExportError::Segment)?;
    let manifest = validation.manifest;
    let (first_device_time_us, last_device_time_us) =
        sample_time_bounds(&paths.wal)?.ok_or(AnalysisExportError::WindowWithoutSamples)?;
    let first_sample_timestamp = estimate_first_sample_timestamp(AnalysisTimeRequest {
        time_root,
        channel_name: identity.channel_name,
        hardware_id: manifest.hardware_id,
        node_address: identity.node_address,
        boot_epoch: manifest.boot_epoch,
        first_device_time_us,
        last_device_time_us,
        sealed_manifest: &paths.manifest,
    })
    .map_err(AnalysisExportError::Time)?;
    fs::create_dir_all(output_directory).map_err(AnalysisExportError::Io)?;

    if let Some(csv_path) = existing_product_path(
        output_directory,
        manifest.hardware_id,
        manifest.boot_epoch,
        ordinal,
    )? {
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

    let simple_channel_name = identity.channel_name.replace('-', "_");
    let base_stem = format!(
        "{simple_channel_name}-node_{}-{}",
        identity.node_address, first_sample_timestamp.local_filename_minute,
    );
    let stem = unique_product_stem(output_directory, &base_stem, ordinal);
    let csv_path = output_directory.join(format!("{stem}.csv"));
    let events_path = output_directory.join(format!("{stem}.events.csv"));
    let telemetry_path = output_directory.join(format!("{stem}.telemetry.csv"));
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
    let telemetry_temp = output_directory.join(format!(".{stem}.{nonce}.telemetry.tmp"));
    let metadata_temp = output_directory.join(format!(".{stem}.{nonce}.json.tmp"));

    let result = write_window(
        &paths.wal,
        &csv_temp,
        &events_temp,
        &telemetry_temp,
        &metadata_temp,
        telemetry_wal_path,
        identity,
        manifest.hardware_id,
        manifest.boot_epoch,
        ordinal,
        manifest.sample_count,
        manifest.missing_sample_count,
        manifest.continuity_break_count,
        configurations,
        &first_sample_timestamp,
        last_device_time_us,
        &format!("{stem}.csv"),
        &format!("{stem}.telemetry.csv"),
    );
    let written = match result {
        Ok(value) => value,
        Err(error) => {
            let _ = fs::remove_file(&csv_temp);
            let _ = fs::remove_file(&events_temp);
            let _ = fs::remove_file(&telemetry_temp);
            let _ = fs::remove_file(&metadata_temp);
            return Err(error);
        }
    };

    fs::rename(&events_temp, &events_path).map_err(AnalysisExportError::Io)?;
    fs::rename(&telemetry_temp, &telemetry_path).map_err(AnalysisExportError::Io)?;
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
    telemetry_path: &Path,
    metadata_path: &Path,
    telemetry_wal_path: &Path,
    identity: AnalysisWindowIdentity<'_>,
    hardware_id: HardwareId,
    boot_epoch: BootEpoch,
    ordinal: u64,
    expected_samples: u64,
    missing_samples: u64,
    continuity_breaks: u64,
    configurations: &[ConfigurationSnapshot],
    first_sample_timestamp: &AnalysisTimestamp,
    last_device_time_us: u64,
    csv_file_name: &str,
    telemetry_file_name: &str,
) -> Result<WrittenWindow, AnalysisExportError> {
    let mut samples = create_writer(csv_path)?;
    let mut events = create_writer(events_path)?;
    writeln!(samples, "x_m_s2,y_m_s2,z_m_s2")?;
    writeln!(events, "row_index,source_sample_number,event,count,reason")?;

    let mut reader = CaptureWalReader::open(wal_path).map_err(AnalysisExportError::Wal)?;
    let mut sample_count = 0_u64;
    let mut first_time = None;
    let mut prior = None::<(u64, u64)>;
    let mut interval_sum_us = 0_u128;
    let mut interval_count = 0_u64;
    let mut analysis_configuration = None::<AnalysisConfiguration>;
    let mut first_config_revision = None;
    let mut last_config_revision = None;
    while let Some(record) = reader.next_record().map_err(AnalysisExportError::Wal)? {
        match record {
            CaptureRecord::SampleChunk(chunk) => {
                let snapshot = configuration_for_chunk(
                    configurations,
                    hardware_id,
                    boot_epoch,
                    chunk.config_revision,
                    chunk.header.first.get(),
                )?;
                let current_configuration = AnalysisConfiguration::from_snapshot(snapshot)?;
                if let Some(expected) = analysis_configuration {
                    if expected.output_rate_nanohertz != current_configuration.output_rate_nanohertz
                        || expected.range_g != current_configuration.range_g
                    {
                        return Err(AnalysisExportError::MixedAnalysisConfiguration {
                            first_revision: first_config_revision.unwrap_or(chunk.config_revision),
                            next_revision: chunk.config_revision,
                        });
                    }
                } else {
                    analysis_configuration = Some(current_configuration);
                    first_config_revision = Some(chunk.config_revision);
                }
                last_config_revision = Some(chunk.config_revision);
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
                    let scale = current_configuration.m_s2_per_lsb;
                    writeln!(
                        samples,
                        "{:.9},{:.9},{:.9}",
                        f64::from(sample.x.get()) * scale,
                        f64::from(sample.y.get()) * scale,
                        f64::from(sample.z.get()) * scale,
                    )?;
                    sample_count = sample_count
                        .checked_add(1)
                        .ok_or(AnalysisExportError::CounterOverflow)?;
                }
            }
            CaptureRecord::Gap(gap) => writeln!(
                events,
                "{sample_count},{},gap,{},\"{:?}\"",
                gap.first_missing.get(),
                gap.missing_count,
                gap.reason
            )?,
            CaptureRecord::ContinuityBreak(observed) => writeln!(
                events,
                "{sample_count},{},continuity_break,1,\"{:?}\"",
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
    let analysis_configuration =
        analysis_configuration.ok_or(AnalysisExportError::WindowWithoutSamples)?;
    let estimated_odr_hz = if interval_count == 0 || interval_sum_us == 0 {
        None
    } else {
        Some(interval_count as f64 * 1_000_000.0 / interval_sum_us as f64)
    };
    samples.flush()?;
    events.flush()?;
    samples.get_ref().sync_all()?;
    events.get_ref().sync_all()?;

    let telemetry_record_count = write_telemetry_window(
        telemetry_wal_path,
        telemetry_path,
        hardware_id,
        identity.node_address,
        boot_epoch,
        first_device_time_us,
        last_device_time_us,
    )?;

    let metadata = json!({
        "schema": "sensor-system-analysis-csv-v2",
        "data_file": csv_file_name,
        "telemetry_file": telemetry_file_name,
        "channel_name": identity.channel_name,
        "channel_id": identity.channel_id,
        "node_address": identity.node_address,
        "hardware_id": hardware_id_hex(hardware_id),
        "boot_epoch": boot_epoch.get(),
        "window_ordinal": ordinal,
        "first_device_time_us": first_device_time_us,
        "last_device_time_us": last_device_time_us,
        "first_sample_time_utc": first_sample_timestamp.utc,
        "first_sample_time_local": first_sample_timestamp.local,
        "first_sample_unix_time_us": first_sample_timestamp.unix_time_us,
        "first_sample_time_uncertainty_us": first_sample_timestamp.uncertainty_us,
        "first_sample_time_method": first_sample_timestamp.method,
        "sample_count": sample_count,
        "missing_sample_count": missing_samples,
        "continuity_break_count": continuity_breaks,
        "telemetry_record_count": telemetry_record_count,
        "telemetry_columns": [
            "captured_device_time_us",
            "time_from_first_sample_s",
            "adxl355_temperature_c",
            "adxl355_temperature_raw"
        ],
        "telemetry_window_policy": "captured_device_time_us within inclusive sample time bounds",
        "estimated_odr_hz": estimated_odr_hz,
        "estimated_odr_method": "mean interval between adjacent, consecutive samples",
        "configured_odr_hz": analysis_configuration.output_rate_hz(),
        "range_g": analysis_configuration.range_g,
        "first_config_revision": first_config_revision,
        "last_config_revision": last_config_revision,
        "axis_unit": "m/s^2",
        "columns": ["x_m_s2", "y_m_s2", "z_m_s2"],
        "nominal_sensitivity_lsb_per_g": analysis_configuration.sensitivity_lsb_per_g,
        "standard_gravity_m_s2": STANDARD_GRAVITY_M_S2,
        "conversion": "raw_code / nominal_sensitivity_lsb_per_g * standard_gravity_m_s2",
        "sensitivity_basis": "ADXL355 datasheet Rev. D typical digital sensitivity for the configured range",
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

fn write_telemetry_window(
    wal_path: &Path,
    output_path: &Path,
    hardware_id: HardwareId,
    node_address: u8,
    boot_epoch: BootEpoch,
    first_device_time_us: u64,
    last_device_time_us: u64,
) -> Result<u64, AnalysisExportError> {
    let mut reader =
        TelemetryWalReader::open(wal_path).map_err(AnalysisExportError::TelemetryWal)?;
    let config = reader.config();
    if config.hardware_id != hardware_id
        || config.node_address != node_address
        || config.boot_epoch != boot_epoch
    {
        return Err(AnalysisExportError::TelemetryIdentityMismatch);
    }
    let mut output = create_writer(output_path)?;
    writeln!(
        output,
        "captured_device_time_us,time_from_first_sample_s,adxl355_temperature_c,adxl355_temperature_raw"
    )?;
    let mut count = 0_u64;
    while let Some(snapshot) = reader
        .next_snapshot()
        .map_err(AnalysisExportError::TelemetryWal)?
    {
        if snapshot.captured_device_time_us < first_device_time_us {
            continue;
        }
        if snapshot.captured_device_time_us > last_device_time_us {
            break;
        }
        let offset_seconds =
            (snapshot.captured_device_time_us - first_device_time_us) as f64 / 1_000_000.0;
        writeln!(
            output,
            "{},{:.6},{:.6},{}",
            snapshot.captured_device_time_us,
            offset_seconds,
            snapshot.adxl355_temperature.celsius(),
            snapshot.adxl355_temperature.get(),
        )?;
        count = count
            .checked_add(1)
            .ok_or(AnalysisExportError::CounterOverflow)?;
    }
    output.flush()?;
    output.get_ref().sync_all()?;
    Ok(count)
}

fn configuration_for_chunk(
    configurations: &[ConfigurationSnapshot],
    hardware_id: HardwareId,
    boot_epoch: BootEpoch,
    revision: u32,
    first_cursor: u64,
) -> Result<&ConfigurationSnapshot, AnalysisExportError> {
    configurations
        .iter()
        .rev()
        .find(|snapshot| {
            snapshot.hardware_id == hardware_id
                && snapshot.boot_epoch == boot_epoch
                && snapshot.observed.revision == revision
                && snapshot.effective_sample_seq <= first_cursor
        })
        .ok_or(AnalysisExportError::MissingConfiguration {
            boot_epoch,
            revision,
            first_cursor,
        })
}

fn sample_time_bounds(wal_path: &Path) -> Result<Option<(u64, u64)>, AnalysisExportError> {
    let mut reader = CaptureWalReader::open(wal_path).map_err(AnalysisExportError::Wal)?;
    let mut first = None;
    let mut last = None;
    while let Some(record) = reader.next_record().map_err(AnalysisExportError::Wal)? {
        if let CaptureRecord::SampleChunk(chunk) = record {
            first.get_or_insert(chunk.timing.first_device_time_us);
            last = Some(chunk.timing.last_device_time_us);
        }
    }
    Ok(first.zip(last))
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct AnalysisConfiguration {
    output_rate_nanohertz: u64,
    range_g: u8,
    sensitivity_lsb_per_g: u32,
    m_s2_per_lsb: f64,
}

impl AnalysisConfiguration {
    fn from_snapshot(snapshot: &ConfigurationSnapshot) -> Result<Self, AnalysisExportError> {
        let range_g = snapshot.observed.desired.range_g;
        let sensitivity_lsb_per_g = match range_g {
            2 => 256_000,
            4 => 128_000,
            8 => 64_000,
            value => return Err(AnalysisExportError::UnsupportedRange(value)),
        };
        Ok(Self {
            output_rate_nanohertz: snapshot
                .observed
                .desired
                .output_rate
                .sample_rate()
                .nanohertz(),
            range_g,
            sensitivity_lsb_per_g,
            m_s2_per_lsb: STANDARD_GRAVITY_M_S2 / f64::from(sensitivity_lsb_per_g),
        })
    }

    fn output_rate_hz(self) -> f64 {
        self.output_rate_nanohertz as f64 / 1_000_000_000.0
    }
}

fn unique_product_stem(output_directory: &Path, base: &str, ordinal: u64) -> String {
    if !output_directory.join(format!("{base}.csv")).exists() {
        base.to_owned()
    } else {
        format!("{base}-w{ordinal}")
    }
}

fn existing_product_path(
    output_directory: &Path,
    hardware_id: HardwareId,
    boot_epoch: BootEpoch,
    ordinal: u64,
) -> Result<Option<PathBuf>, AnalysisExportError> {
    let entries = match fs::read_dir(output_directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AnalysisExportError::Io(error)),
    };
    let expected_hardware = hardware_id_hex(hardware_id);
    for entry in entries {
        let path = entry.map_err(AnalysisExportError::Io)?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let metadata: serde_json::Value = match fs::read(&path)
            .map_err(AnalysisExportError::Io)
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(AnalysisExportError::Json))
        {
            Ok(value) => value,
            Err(_) => continue,
        };
        if metadata.get("schema").and_then(|value| value.as_str())
            != Some("sensor-system-analysis-csv-v2")
            || metadata.get("hardware_id").and_then(|value| value.as_str())
                != Some(expected_hardware.as_str())
            || metadata.get("boot_epoch").and_then(|value| value.as_u64()) != Some(boot_epoch.get())
            || metadata
                .get("window_ordinal")
                .and_then(|value| value.as_u64())
                != Some(ordinal)
        {
            continue;
        }
        let csv_path = metadata
            .get("data_file")
            .and_then(|value| value.as_str())
            .map(|name| output_directory.join(name))
            .unwrap_or_else(|| path.with_extension("csv"));
        let telemetry_path = metadata
            .get("telemetry_file")
            .and_then(|value| value.as_str())
            .map(|name| output_directory.join(name));
        if csv_path.is_file() && telemetry_path.is_some_and(|path| path.is_file()) {
            return Ok(Some(csv_path));
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
    TelemetryWal(TelemetryWalError),
    TelemetryIdentityMismatch,
    Time(AnalysisTimeError),
    WindowWithoutSamples,
    InvalidTiming,
    CounterOverflow,
    UnsupportedRange(u8),
    MixedAnalysisConfiguration {
        first_revision: u32,
        next_revision: u32,
    },
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
        Adxl355Odr, Adxl355TemperatureCode, DecimationFilterProfile, MeasurementConfiguration,
        ObservedConfiguration, OutputRateCode, SampleChunk, SampleChunkHeader, TelemetrySnapshot,
        TimingEvidence, TIMING_VALID,
    };

    use crate::capture_segments::CaptureSegmentStream;
    use crate::capture_wal::CaptureWalConfig;
    use crate::capture_window::CaptureWindowPolicy;
    use crate::measurement_persistence::DurableMeasurementSink;
    use crate::telemetry_wal::{TelemetryWalConfig, TelemetryWalSink};

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
        let telemetry_path = temp.0.join("telemetry.wal");
        let mut telemetry = TelemetryWalSink::open(
            &telemetry_path,
            TelemetryWalConfig {
                hardware_id: HARDWARE,
                node_address: 1,
                boot_epoch: BOOT,
            },
        )
        .unwrap();
        for (captured_device_time_us, raw) in
            [(999_000, 1884), (1_004_000, 1885), (1_009_000, 1886)]
        {
            telemetry
                .append(TelemetrySnapshot {
                    captured_device_time_us,
                    adxl355_temperature: Adxl355TemperatureCode::new(raw).unwrap(),
                })
                .unwrap();
        }

        let identity = AnalysisWindowIdentity {
            channel_name: "line-b",
            channel_id: 2,
            node_address: 1,
        };
        let first = export_analysis_window(
            &stream_dir,
            &output_dir,
            &temp.0.join("time"),
            &telemetry_path,
            0,
            identity,
            &[configuration()],
        )
        .unwrap();
        assert!(!first.already_exists);
        assert_eq!(first.sample_count, 2);
        assert_eq!(first.first_device_time_us, 1_000_000);
        assert_eq!(first.estimated_odr_hz, Some(125.0));
        let csv = fs::read_to_string(&first.csv_path).unwrap();
        assert!(csv.starts_with("x_m_s2,y_m_s2,z_m_s2\n"));
        assert!(csv.contains("0.000383072,0.000766145,0.001149217\n"));
        assert!(csv.contains("0.000421379,0.000804452,0.001187524\n"));
        assert!(first
            .csv_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("line_b-node_1-"));
        let metadata = fs::read_to_string(first.csv_path.with_extension("json")).unwrap();
        assert!(metadata.contains("\"schema\": \"sensor-system-analysis-csv-v2\""));
        assert!(metadata.contains("\"configured_odr_hz\": 125.0"));
        assert!(metadata.contains("\"range_g\": 2"));
        assert!(metadata.contains("\"axis_unit\": \"m/s^2\""));
        assert!(metadata.contains("\"telemetry_record_count\": 1"));
        let telemetry_csv = fs::read_to_string(first.csv_path.with_file_name(format!(
            "{}.telemetry.csv",
            first.csv_path.file_stem().unwrap().to_string_lossy()
        )))
        .unwrap();
        assert_eq!(
            telemetry_csv,
            "captured_device_time_us,time_from_first_sample_s,adxl355_temperature_c,adxl355_temperature_raw\n1004000,0.004000,25.000000,1885\n"
        );

        let repeated = export_analysis_window(
            &stream_dir,
            &output_dir,
            &temp.0.join("time"),
            &telemetry_path,
            0,
            identity,
            &[configuration()],
        )
        .unwrap();
        assert!(repeated.already_exists);
    }
}
