use core::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::os::unix::io::AsRawFd;
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
use crate::time_wal::{TimeWalError, TimeWalReader};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnalysisWindowIdentity<'a> {
    pub channel_name: &'a str,
    pub timezone: chrono_tz::Tz,
    pub channel_id: u8,
    pub node_address: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnalysisWindowSummary {
    pub hdf5_path: PathBuf,
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
    let calendar = crate::capture_calendar::CalendarWindow::load(stream_directory, ordinal)?;
    let (unix_us, uncertainty_us, method) = if let Some(calendar) = calendar {
        (
            calendar.unix_time(first_device_time_us)?,
            calendar.uncertainty_us,
            "frozen WAL calendar anchor; unknown uncertainty indicates receive-time fallback or clock-regression clamp",
        )
    } else {
        let estimate = estimate_first_sample_timestamp(AnalysisTimeRequest {
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
        (
            estimate.unix_time_us,
            estimate.uncertainty_us,
            estimate.method,
        )
    };
    let utc = chrono::DateTime::<chrono::Utc>::from_timestamp_micros(
        i64::try_from(unix_us).map_err(|_| AnalysisExportError::CounterOverflow)?,
    )
    .ok_or(AnalysisExportError::InvalidTiming)?;
    let timezone = calendar.map_or(identity.timezone, |c| c.timezone);
    let local = utc.with_timezone(&timezone);
    let first_sample_timestamp = AnalysisTimestamp {
        unix_time_us: unix_us,
        utc: utc.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        local: local.to_rfc3339_opts(chrono::SecondsFormat::Micros, false),
        local_filename_minute: local.format("%Y-%m-%d_%H-%M").to_string(),
        uncertainty_us,
        method,
    };
    let identity = AnalysisWindowIdentity {
        timezone,
        ..identity
    };
    fs::create_dir_all(output_directory)?;
    sync_directory(output_directory)?;
    if let Some(parent) = output_directory.parent() {
        sync_directory(parent)?;
    }

    // Serialize selection and publication across manual exports and the timer.
    // Closing the descriptor releases flock even after process termination.
    let directory_lock = File::open(output_directory)?;
    nix::fcntl::flock(
        directory_lock.as_raw_fd(),
        nix::fcntl::FlockArg::LockExclusive,
    )
    .map_err(|error| io::Error::from_raw_os_error(error as i32))?;
    let base_stem = format!(
        "{}_{}",
        identity.channel_name, first_sample_timestamp.local_filename_minute
    );
    let selection = hdf5_helper(&[
        "--select".into(),
        output_directory.as_os_str().to_owned(),
        base_stem.into(),
        hardware_id_hex(manifest.hardware_id).into(),
        manifest.boot_epoch.get().to_string().into(),
        ordinal.to_string().into(),
    ])?;
    let hdf5_path = PathBuf::from(
        selection["path"]
            .as_str()
            .ok_or_else(|| io::Error::other("HDF5 selector returned no path"))?,
    );
    let stem = hdf5_path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| io::Error::other("invalid HDF5 product name"))?;
    if selection["already_exists"].as_bool() == Some(true) {
        return Ok(AnalysisWindowSummary {
            hdf5_path,
            sample_count: manifest.sample_count,
            missing_sample_count: manifest.missing_sample_count,
            continuity_break_count: manifest.continuity_break_count,
            first_device_time_us,
            estimated_odr_hz: None,
            already_exists: true,
        });
    }
    // The directory lock makes it safe to remove remnants of interrupted runs.
    let temporary_prefix = format!(".{stem}.");
    for entry in fs::read_dir(output_directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if name
            .to_str()
            .is_some_and(|name| name.starts_with(&temporary_prefix) && name.ends_with(".tmp"))
        {
            fs::remove_file(entry.path())?;
        }
    }
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let samples_temp = output_directory.join(format!(".{stem}.{nonce}.samples.tmp"));
    let hdf5_temp = output_directory.join(format!(".{stem}.{nonce}.h5.tmp"));
    let events_temp = output_directory.join(format!(".{stem}.{nonce}.events.tmp"));
    let telemetry_temp = output_directory.join(format!(".{stem}.{nonce}.telemetry.tmp"));
    let time_sync_temp = output_directory.join(format!(".{stem}.{nonce}.time-sync.tmp"));
    let metadata_temp = output_directory.join(format!(".{stem}.{nonce}.json.tmp"));

    let result = write_window(
        &paths.wal,
        &samples_temp,
        &events_temp,
        &telemetry_temp,
        &time_sync_temp,
        &metadata_temp,
        time_root,
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
        &format!("{stem}.h5"),
        manifest,
        calendar,
    );
    let result = result.and_then(|written| {
        let status = std::process::Command::new(analysis_python())
            .arg("-c")
            .arg(include_str!("../../../analysis/write_analysis_hdf5.py"))
            .args([
                &samples_temp,
                &events_temp,
                &telemetry_temp,
                &time_sync_temp,
                &metadata_temp,
                &hdf5_temp,
            ])
            .status()?;
        if !status.success() {
            return Err(AnalysisExportError::Io(io::Error::other(
                "HDF5 writer failed; install h5py and numpy for SENSOR_ANALYSIS_PYTHON",
            )));
        }
        File::open(&hdf5_temp)?.sync_all()?;
        fs::rename(&hdf5_temp, &hdf5_path)?;
        Ok(written)
    });
    for path in [
        &samples_temp,
        &events_temp,
        &telemetry_temp,
        &time_sync_temp,
        &metadata_temp,
        &hdf5_temp,
    ] {
        let _ = fs::remove_file(path);
    }
    let written = result?;
    sync_directory(output_directory)?;
    Ok(AnalysisWindowSummary {
        hdf5_path,
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
    hdf5_path: &Path,
    events_path: &Path,
    telemetry_path: &Path,
    time_sync_path: &Path,
    metadata_path: &Path,
    time_root: &Path,
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
    data_file_name: &str,
    source_manifest: crate::capture_window::CaptureWindowManifest,
    calendar: Option<crate::capture_calendar::CalendarWindow>,
) -> Result<WrittenWindow, AnalysisExportError> {
    let mut samples = create_writer(hdf5_path)?;
    let mut events = create_writer(events_path)?;
    let mut reader = CaptureWalReader::open(wal_path).map_err(AnalysisExportError::Wal)?;
    let mut sample_count = 0_u64;
    let mut first_time = None;
    let mut prior = None::<(u64, u64)>;
    let mut interval_sum_us = 0_u128;
    let mut interval_count = 0_u64;
    let mut event_record_count = 0_u64;
    let mut analysis_configuration = None::<AnalysisConfiguration>;
    let mut first_configuration_snapshot = None;
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
                    if first_config_revision != Some(chunk.config_revision)
                        || expected.output_rate_nanohertz
                            != current_configuration.output_rate_nanohertz
                        || expected.range_g != current_configuration.range_g
                    {
                        return Err(AnalysisExportError::MixedAnalysisConfiguration {
                            first_revision: first_config_revision.unwrap_or(chunk.config_revision),
                            next_revision: chunk.config_revision,
                        });
                    }
                } else {
                    analysis_configuration = Some(current_configuration);
                    first_configuration_snapshot = Some(*snapshot);
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
                    let device_time = chunk.device_times.as_slice()[index];
                    first_time.get_or_insert(device_time);
                    if let Some((prior_cursor, prior_time)) = prior {
                        if device_time <= prior_time {
                            return Err(AnalysisExportError::InvalidTiming);
                        }
                        if cursor == prior_cursor.saturating_add(1) && device_time > prior_time {
                            interval_sum_us += u128::from(device_time - prior_time);
                            interval_count = interval_count
                                .checked_add(1)
                                .ok_or(AnalysisExportError::CounterOverflow)?;
                        }
                    }
                    prior = Some((cursor, device_time));
                    serde_json::to_writer(
                        &mut samples,
                        &json!([
                            cursor,
                            device_time,
                            sample.x.get(),
                            sample.y.get(),
                            sample.z.get(),
                        ]),
                    )
                    .map_err(AnalysisExportError::Json)?;
                    writeln!(samples)?;
                    sample_count = sample_count
                        .checked_add(1)
                        .ok_or(AnalysisExportError::CounterOverflow)?;
                }
            }
            CaptureRecord::Gap(gap) => {
                serde_json::to_writer(
                    &mut events,
                    &json!([
                        sample_count,
                        gap.first_missing.get(),
                        "gap",
                        gap.missing_count,
                        format!("{:?}", gap.reason)
                    ]),
                )
                .map_err(AnalysisExportError::Json)?;
                writeln!(events)?;
                event_record_count = event_record_count
                    .checked_add(1)
                    .ok_or(AnalysisExportError::CounterOverflow)?;
            }
            CaptureRecord::ContinuityBreak(observed) => {
                serde_json::to_writer(
                    &mut events,
                    &json!([
                        sample_count,
                        observed.at.get(),
                        "continuity_break",
                        1,
                        format!("{:?}", observed.reason)
                    ]),
                )
                .map_err(AnalysisExportError::Json)?;
                writeln!(events)?;
                event_record_count = event_record_count
                    .checked_add(1)
                    .ok_or(AnalysisExportError::CounterOverflow)?;
            }
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
    let (time_sync_record_count, time_sync_host_clock_count) = write_time_sync_window(
        time_root,
        time_sync_path,
        identity,
        hardware_id,
        boot_epoch,
        first_device_time_us..=last_device_time_us,
    )?;

    let mut metadata = json!({
        "schema": "sensor-system-analysis-hdf5-v6",
        "data_file": data_file_name,
        "channel_name": identity.channel_name,
        "channel_id": identity.channel_id,
        "node_address": identity.node_address,
        "hardware_id": hardware_id_hex(hardware_id),
        "boot_epoch": boot_epoch.get(),
        "window_ordinal": ordinal,
        "timezone": identity.timezone.name(),
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
        "event_record_count": event_record_count,
        "telemetry_record_count": telemetry_record_count,
        "telemetry_status": if telemetry_record_count == 0 { "no_observations_in_window" } else { "present" },
        "telemetry_columns": ["time_us", "temperature_c"],
        "telemetry_window_policy": "time_us within inclusive sample time bounds",
        "time_sync_record_count": time_sync_record_count,
        "time_sync_host_clock_count": time_sync_host_clock_count,
        "time_sync_status": if time_sync_record_count == 0 { "no_observations_available" } else { "present" },
        "time_sync_columns": ["host_clock_id", "t1_host_us", "t2_node_us", "t3_node_us", "t4_host_us", "path_delay_us", "uncertainty_us"],
        "time_sync_selection_policy": "all observations with node midpoint inside the sample window plus up to two observations before and after per host clock",
        "estimated_odr_hz": estimated_odr_hz,
        "estimated_odr_method": "mean interval between adjacent, consecutive samples",
        "configured_odr_hz": analysis_configuration.output_rate_hz(),
        "range_g": analysis_configuration.range_g,
        "first_config_revision": first_config_revision,
        "last_config_revision": last_config_revision,
        "axis_unit": "raw",
        "columns": ["sample_sequence", "czas_us", "x", "y", "z"],
        "sample_time_method": "individual node monotonic timestamps; FIR center sample",
        "sample_time_resolution_us": 1,
        "source": "validated sealed Capture WAL"
    });
    let snapshot = first_configuration_snapshot.ok_or(AnalysisExportError::WindowWithoutSamples)?;
    let desired = snapshot.observed.desired;
    metadata["configuration"] = json!({
        "revision": snapshot.observed.revision,
        "effective_sample_seq": snapshot.effective_sample_seq,
        "output_rate_code": desired.output_rate as u8,
        "sensor_odr_code": snapshot.observed.sensor_odr as u8,
        "decimation_factor": snapshot.observed.decimation_factor,
        "range_g": desired.range_g, "high_pass_corner": desired.high_pass_corner,
        "filter_profile": desired.filter_profile as u8,
        "fifo_watermark": desired.fifo_watermark,
        "offset_x": desired.offset_x, "offset_y": desired.offset_y, "offset_z": desired.offset_z,
    });
    metadata["source_first_cursor"] = json!(source_manifest.first_cursor.get());
    metadata["source_end_exclusive"] = json!(source_manifest.end_exclusive.get());
    metadata["calendar_end_unix_time_us"] = json!(calendar.map(|c| c.end_unix_us));
    metadata["calendar_end_device_time_us"] = json!(calendar.map(|c| c.end_device_us));
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
        serde_json::to_writer(
            &mut output,
            &json!([
                snapshot.captured_device_time_us,
                snapshot.adxl355_temperature.celsius(),
            ]),
        )
        .map_err(AnalysisExportError::Json)?;
        writeln!(output)?;
        count = count
            .checked_add(1)
            .ok_or(AnalysisExportError::CounterOverflow)?;
    }
    output.flush()?;
    output.get_ref().sync_all()?;
    Ok(count)
}

fn write_time_sync_window(
    time_root: &Path,
    output_path: &Path,
    identity: AnalysisWindowIdentity<'_>,
    hardware_id: HardwareId,
    boot_epoch: BootEpoch,
    device_time_range: core::ops::RangeInclusive<u64>,
) -> Result<(u64, u64), AnalysisExportError> {
    let first_device_time_us = *device_time_range.start();
    let last_device_time_us = *device_time_range.end();
    let mut output = create_writer(output_path)?;
    let parent = time_root
        .join(identity.channel_name)
        .join(hardware_id_hex(hardware_id));
    let entries = match fs::read_dir(&parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            output.flush()?;
            output.get_ref().sync_all()?;
            return Ok((0, 0));
        }
        Err(error) => return Err(AnalysisExportError::Io(error)),
    };
    let expected_name = format!(
        "boot-{:016x}-node-{:02x}.wal",
        boot_epoch.get(),
        identity.node_address
    );
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir()
            || !entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("host-"))
        {
            continue;
        }
        let path = entry.path().join(&expected_name);
        if path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();

    let mut count = 0_u64;
    let mut host_clock_count = 0_u64;
    for path in paths {
        let mut reader = TimeWalReader::open(&path).map_err(AnalysisExportError::TimeWal)?;
        let config = reader.config();
        if config.hardware_id != hardware_id
            || config.node_address != identity.node_address
            || config.boot_epoch != boot_epoch
        {
            return Err(AnalysisExportError::TimeIdentityMismatch);
        }
        let mut observations = Vec::new();
        while let Some(observation) = reader
            .next_observation()
            .map_err(AnalysisExportError::TimeWal)?
        {
            observations.push(observation);
        }
        if observations.is_empty() {
            continue;
        }
        let inside_start = observations
            .partition_point(|observation| observation.node_midpoint_us() < first_device_time_us);
        let inside_end = observations
            .partition_point(|observation| observation.node_midpoint_us() <= last_device_time_us);
        let selected_start = inside_start.saturating_sub(2);
        let selected_end = inside_end.saturating_add(2).min(observations.len());
        if selected_start == selected_end {
            continue;
        }
        host_clock_count = host_clock_count
            .checked_add(1)
            .ok_or(AnalysisExportError::CounterOverflow)?;
        let host_clock_id = config
            .host_clock_id
            .bytes()
            .iter()
            .map(|value| format!("{value:02x}"))
            .collect::<String>();
        for observation in &observations[selected_start..selected_end] {
            serde_json::to_writer(
                &mut output,
                &json!([
                    host_clock_id.as_str(),
                    observation.host_send_us,
                    observation.node_receive_us,
                    observation.node_send_us,
                    observation.host_receive_us,
                    observation.path_delay_us(),
                    observation.uncertainty_us(),
                ]),
            )
            .map_err(AnalysisExportError::Json)?;
            writeln!(output)?;
            count = count
                .checked_add(1)
                .ok_or(AnalysisExportError::CounterOverflow)?;
        }
    }
    output.flush()?;
    output.get_ref().sync_all()?;
    Ok((count, host_clock_count))
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
}

impl AnalysisConfiguration {
    fn from_snapshot(snapshot: &ConfigurationSnapshot) -> Result<Self, AnalysisExportError> {
        let range_g = snapshot.observed.desired.range_g;
        if !matches!(range_g, 2 | 4 | 8) {
            return Err(AnalysisExportError::UnsupportedRange(range_g));
        }
        Ok(Self {
            output_rate_nanohertz: snapshot
                .observed
                .desired
                .output_rate
                .sample_rate()
                .nanohertz(),
            range_g,
        })
    }

    fn output_rate_hz(self) -> f64 {
        self.output_rate_nanohertz as f64 / 1_000_000_000.0
    }
}

/// Read identities from HDF5 metadata, independently of human-facing names.
pub fn analysis_product_index(root: &Path) -> Result<serde_json::Value, AnalysisExportError> {
    if !root.exists() {
        return Ok(json!([]));
    }
    hdf5_helper(&["--index".into(), root.as_os_str().to_owned()])
}

fn hdf5_helper(arguments: &[std::ffi::OsString]) -> Result<serde_json::Value, AnalysisExportError> {
    let output = std::process::Command::new(analysis_python())
        .arg("-c")
        .arg(include_str!("../../../analysis/write_analysis_hdf5.py"))
        .args(arguments)
        .output()?;
    if !output.status.success() {
        return Err(AnalysisExportError::Io(io::Error::other(format!(
            "HDF5 metadata operation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ))));
    }
    serde_json::from_slice(&output.stdout).map_err(AnalysisExportError::Json)
}

fn analysis_python() -> std::ffi::OsString {
    std::env::var_os("SENSOR_ANALYSIS_PYTHON").unwrap_or_else(|| "python3".into())
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
    TimeWal(TimeWalError),
    TelemetryWal(TelemetryWalError),
    TelemetryIdentityMismatch,
    TimeIdentityMismatch,
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
        write!(formatter, "analysis HDF5 export failed: {self:?}")
    }
}

impl std::error::Error for AnalysisExportError {}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_core::{ClockObservation, SampleAxes, SampleAxis, SequenceCursor};
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
    use crate::time_wal::{HostClockId, TimeWalConfig, TimeWalSink};

    static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "sensor-analysis-export-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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

    fn inspect_hdf5(path: &Path) -> serde_json::Value {
        let output = std::process::Command::new(analysis_python()).arg("-c").arg(
            "import h5py,json,sys; f=h5py.File(sys.argv[1]); m={k:v.tolist() if hasattr(v,'tolist') else v for k,v in f.attrs.items()}; m['configuration']={k.removeprefix('configuration_'):m.pop(k) for k in list(m) if k.startswith('configuration_')}; print(json.dumps(dict(metadata=m, sequence=f['samples']['sample_sequence'].tolist(), times=f['samples']['czas_us'].tolist(), axes={a:f['samples'][a].tolist() for a in 'xyz'}, temperature=f['telemetry']['temperature_c'].tolist(), telemetry_times=f['telemetry']['time_us'].tolist(), time_sync_t1=f['time_sync']['t1_host_us'].tolist(), time_sync_host_clock=[v.decode('ascii') for v in f['time_sync']['host_clock_id'].tolist()])))"
        ).arg(path).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    #[test]
    fn time_sync_window_preserves_host_clock_domains_and_boundary_observations() {
        let temp = TempDirectory::new();
        let time_root = temp.0.join("time");
        for (clock_byte, node_midpoints) in [
            (9_u8, vec![800_u64, 900, 1_050, 1_200]),
            (8_u8, vec![500_u64, 600]),
        ] {
            let clock = HostClockId::from_bytes([clock_byte; 16]);
            let path = time_root
                .join("line-b/0102030405060708")
                .join(format!("host-{}", format!("{clock_byte:02x}").repeat(16)))
                .join("boot-0000000000000009-node-01.wal");
            let mut sink = TimeWalSink::open(
                path,
                TimeWalConfig {
                    hardware_id: HARDWARE,
                    node_address: 1,
                    boot_epoch: BOOT,
                    host_clock_id: clock,
                },
            )
            .unwrap();
            for (index, node_midpoint) in node_midpoints.into_iter().enumerate() {
                let host = 10_000 + index as u64 * 1_000;
                sink.append(
                    ClockObservation::new(
                        BOOT,
                        host,
                        node_midpoint - 5,
                        node_midpoint + 5,
                        host + 20,
                    )
                    .unwrap(),
                )
                .unwrap();
            }
        }
        let output = temp.0.join("time-sync.jsonl");
        let (count, clocks) = write_time_sync_window(
            &time_root,
            &output,
            AnalysisWindowIdentity {
                channel_name: "line-b",
                channel_id: 2,
                node_address: 1,
                timezone: chrono_tz::UTC,
            },
            HARDWARE,
            BOOT,
            1_000..=1_100,
        )
        .unwrap();
        assert_eq!((count, clocks), (6, 2));
        let rows = fs::read_to_string(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            rows.iter().filter(|row| row[0] == "08".repeat(16)).count(),
            2
        );
        assert!(rows.iter().all(|row| row[5] == 10 && row[6] == 5));
    }

    #[test]
    fn aligned_windows_preserve_gaps_and_resolve_same_minute_filename_collisions() {
        use crate::capture_calendar::{CalendarSource, CalendarWindow};
        use chrono::TimeZone;
        use sensor_core::{
            ContinuityBreak, ContinuityBreakReason, Gap, GapReason, SampleSequence, SampleTimes,
        };
        let temp = TempDirectory::new();
        let source = temp.0.join("source");
        std::fs::create_dir_all(&source).unwrap();
        let timezone = chrono_tz::Europe::Warsaw;
        let unix = chrono::Utc
            .with_ymd_and_hms(2026, 11, 2, 22, 59, 50)
            .unwrap()
            .timestamp_micros() as u64;
        for (ordinal, device) in [(0, 0), (1, 10_000_000), (2, 11_000_000)] {
            CalendarWindow::in_timezone(device, unix + device, 600_000_000, Some(20), timezone)
                .unwrap()
                .save(&source, ordinal)
                .unwrap();
        }
        let mut stream = CaptureSegmentStream::open(
            &source,
            CaptureWalConfig {
                hardware_id: HARDWARE,
                boot_epoch: BOOT,
                recovered_cursor: SequenceCursor::ORIGIN,
                first_window_ordinal: 0,
                window_policy: CaptureWindowPolicy::new(600_000_000).unwrap(),
            },
        )
        .unwrap();
        stream
            .set_calendar_source(Some(CalendarSource {
                time_root: temp.0.join("time"),
                channel_name: "line-b".to_owned(),
                timezone,
            }))
            .unwrap();
        let axis = SampleAxis::new(10).unwrap();
        let mut chunk = SampleChunk {
            header: SampleChunkHeader {
                boot_epoch: BOOT,
                retained_from: SequenceCursor::ORIGIN,
                produced: SequenceCursor::new(4),
                first: SequenceCursor::ORIGIN,
                sample_count: 4,
            },
            config_revision: 3,
            timing: TimingEvidence {
                segment_id: 1,
                first_device_time_us: 0,
                last_device_time_us: 10_000_001,
                max_fit_residual_us: 0,
                quality_flags: TIMING_VALID,
            },
            device_times: SampleTimes::new(vec![0, 9_999_999, 10_000_000, 10_000_001]).unwrap(),
            samples: vec![SampleAxes::new(axis, axis, axis); 4],
        };
        stream.persist_chunk(HARDWARE, &chunk).unwrap();
        stream
            .persist_gap(
                Gap::new(
                    HARDWARE,
                    BOOT,
                    SampleSequence::new(4),
                    2,
                    GapReason::SourceLoss,
                )
                .unwrap(),
            )
            .unwrap();
        stream
            .persist_continuity_break(
                ContinuityBreak::new(
                    HARDWARE,
                    BOOT,
                    SequenceCursor::new(6),
                    ContinuityBreakReason::ProcessingReset,
                )
                .unwrap(),
            )
            .unwrap();
        chunk.header.first = SequenceCursor::new(7);
        chunk.header.produced = SequenceCursor::new(8);
        chunk.header.sample_count = 1;
        chunk.config_revision = 4;
        chunk.device_times = SampleTimes::new(vec![11_000_000]).unwrap();
        chunk.timing.first_device_time_us = 11_000_000;
        chunk.timing.last_device_time_us = 11_000_000;
        chunk.samples.truncate(1);
        stream.persist_chunk(HARDWARE, &chunk).unwrap();
        stream.finish_window().unwrap();
        drop(stream);
        let telemetry = temp.0.join("telemetry.wal");
        let _ = TelemetryWalSink::open(
            &telemetry,
            TelemetryWalConfig {
                hardware_id: HARDWARE,
                node_address: 1,
                boot_epoch: BOOT,
            },
        )
        .unwrap();
        let mut changed = configuration();
        changed.observed.revision = 4;
        changed.observed.desired.offset_x = 1;
        changed.effective_sample_seq = 7;
        let configs = [configuration(), changed];
        let output = temp.0.join("analysis");
        let mut cursors = Vec::new();
        for ordinal in 0..3 {
            let result = export_analysis_window(
                &source,
                &output,
                &temp.0.join("time"),
                &telemetry,
                ordinal,
                AnalysisWindowIdentity {
                    channel_name: "line-b",
                    channel_id: 2,
                    node_address: 1,
                    timezone,
                },
                &configs,
            )
            .unwrap();
            assert_eq!(result.hdf5_path.parent().unwrap(), output);
            let expected = match ordinal {
                0 => "line-b_2026-11-02_23-59.h5",
                1 => "line-b_2026-11-03_00-00.h5",
                _ => "line-b_2026-11-03_00-00_2.h5",
            };
            assert_eq!(result.hdf5_path.file_name().unwrap(), expected);
            let product = inspect_hdf5(&result.hdf5_path);
            let meta = &product["metadata"];
            assert_eq!(meta["first_config_revision"], meta["last_config_revision"]);
            assert_eq!(result.hdf5_path.extension().unwrap(), "h5");
            if ordinal == 1 {
                assert_eq!(meta["missing_sample_count"], 2);
                assert_eq!(meta["continuity_break_count"], 1);
            }
            cursors.extend(
                product["sequence"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap()),
            );
            let retry = export_analysis_window(
                &source,
                &output,
                &temp.0.join("time"),
                &telemetry,
                ordinal,
                AnalysisWindowIdentity {
                    channel_name: "line-b",
                    channel_id: 2,
                    node_address: 1,
                    timezone,
                },
                &configs,
            )
            .unwrap();
            assert!(retry.already_exists);
            assert_eq!(retry.hdf5_path, result.hdf5_path);
            // Removing a derived product allows a deterministic rebuild from WAL.
            std::fs::remove_file(&result.hdf5_path).unwrap();
            let repaired = export_analysis_window(
                &source,
                &output,
                &temp.0.join("time"),
                &telemetry,
                ordinal,
                AnalysisWindowIdentity {
                    channel_name: "line-b",
                    channel_id: 2,
                    node_address: 1,
                    timezone,
                },
                &configs,
            )
            .unwrap();
            assert!(!repaired.already_exists);
            assert_eq!(repaired.hdf5_path, result.hdf5_path);
            assert!(repaired.hdf5_path.is_file());
        }
        assert_eq!(cursors, [0, 1, 2, 3, 7]);
    }

    #[test]
    fn publishes_self_contained_idempotent_hdf5_from_one_sealed_window() {
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
            SampleAxes::new(
                SampleAxis::new(12).unwrap(),
                SampleAxis::new(22).unwrap(),
                SampleAxis::new(32).unwrap(),
            ),
        ];
        stream
            .persist_chunk(
                HARDWARE,
                &SampleChunk {
                    device_times: sensor_core::SampleTimes::new(vec![
                        1_000_000, 1_008_003, 1_016_000,
                    ])
                    .unwrap(),
                    header: SampleChunkHeader {
                        boot_epoch: BOOT,
                        retained_from: SequenceCursor::ORIGIN,
                        produced: SequenceCursor::new(3),
                        first: SequenceCursor::ORIGIN,
                        sample_count: 3,
                    },
                    config_revision: 3,
                    timing: TimingEvidence {
                        segment_id: 1,
                        first_device_time_us: 1_000_000,
                        last_device_time_us: 1_016_000,
                        max_fit_residual_us: 3,
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
            [(999_000, 1884), (1_004_000, 1885), (1_017_000, 1886)]
        {
            telemetry
                .append(TelemetrySnapshot {
                    captured_device_time_us,
                    adxl355_temperature: Adxl355TemperatureCode::new(raw).unwrap(),
                })
                .unwrap();
        }
        let host_clock_id = HostClockId::from_bytes([9; 16]);
        let time_path = temp
            .0
            .join("time/line-b/0102030405060708")
            .join(format!("host-{}", "09".repeat(16)))
            .join("boot-0000000000000009-node-01.wal");
        let mut time = TimeWalSink::open(
            &time_path,
            TimeWalConfig {
                hardware_id: HARDWARE,
                node_address: 1,
                boot_epoch: BOOT,
                host_clock_id,
            },
        )
        .unwrap();
        for observation in [
            ClockObservation::new(BOOT, 1_000, 990_000, 990_010, 1_020).unwrap(),
            ClockObservation::new(BOOT, 16_000, 1_005_000, 1_005_010, 16_022).unwrap(),
            ClockObservation::new(BOOT, 31_000, 1_020_000, 1_020_010, 31_024).unwrap(),
        ] {
            time.append(observation).unwrap();
        }

        let identity = AnalysisWindowIdentity {
            timezone: chrono_tz::UTC,
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
        assert_eq!(first.sample_count, 3);
        assert_eq!(first.first_device_time_us, 1_000_000);
        assert_eq!(first.estimated_odr_hz, Some(125.0));
        let product = inspect_hdf5(&first.hdf5_path);
        let metadata = &product["metadata"];
        assert_eq!(metadata["schema"], "sensor-system-analysis-hdf5-v6");
        assert_eq!(metadata["configured_odr_hz"], 125.0);
        assert_eq!(metadata["range_g"], 2);
        assert_eq!(metadata["axis_unit"], "raw");
        assert_eq!(metadata["telemetry_record_count"], 1);
        assert_eq!(metadata["telemetry_status"], "present");
        assert_eq!(metadata["time_sync_record_count"], 3);
        assert_eq!(metadata["time_sync_host_clock_count"], 1);
        assert_eq!(metadata["time_sync_status"], "present");
        assert_eq!(product["sequence"], json!([0, 1, 2]));
        assert_eq!(
            product["axes"],
            json!({"x":[10,11,12],"y":[20,21,22],"z":[30,31,32]})
        );
        assert_eq!(product["times"], json!([1000000, 1008003, 1016000]));
        assert_eq!(product["temperature"], json!([25.0]));
        assert_eq!(product["telemetry_times"], json!([1_004_000]));
        assert_eq!(product["time_sync_t1"], json!([1_000, 16_000, 31_000]));
        assert_eq!(
            product["time_sync_host_clock"],
            json!(["09".repeat(16), "09".repeat(16), "09".repeat(16)])
        );
        assert!(metadata["configuration"].is_object());
        assert_eq!(
            fs::read_dir(first.hdf5_path.parent().unwrap())
                .unwrap()
                .count(),
            1
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
