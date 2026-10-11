use core::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::capture_segments::{
    capture_segment_paths, validate_capture_segment, CaptureSegmentError,
};
use crate::capture_wal::{CaptureRecord, CaptureWalError, CaptureWalReader};
use crate::capture_window::CaptureWindowManifest;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureExportSummary {
    pub window_count: u64,
    pub sample_count: u64,
    pub missing_sample_count: u64,
    pub continuity_break_count: u64,
    pub first_cursor: u64,
    pub end_exclusive: u64,
}

/// Validates every sealed window in one boot-scoped stream without expanding
/// raw samples to an analysis format. Memory use is bounded by one wire chunk.
pub fn validate_capture_chain(
    capture_directory: impl AsRef<Path>,
) -> Result<CaptureExportSummary, CaptureExportError> {
    let capture_directory = capture_directory.as_ref();
    let ordinals = discover_ordinals(capture_directory)?;
    if ordinals.is_empty() {
        return Err(CaptureExportError::NoWindows);
    }
    let mut previous: Option<CaptureWindowManifest> = None;
    let mut first_manifest = None;
    let mut total_samples = 0_u64;
    let mut total_missing = 0_u64;
    let mut total_breaks = 0_u64;

    for ordinal in ordinals.iter().copied() {
        let paths = capture_segment_paths(capture_directory, ordinal);
        let validation = validate_capture_segment(paths.wal, paths.manifest)
            .map_err(CaptureExportError::Segment)?;
        let manifest = validation.manifest;
        if manifest.window_ordinal != ordinal {
            return Err(CaptureExportError::NonConsecutiveWindows);
        }
        if let Some(prior) = previous {
            if manifest.window_ordinal != prior.window_ordinal.saturating_add(1)
                || manifest.hardware_id != prior.hardware_id
                || manifest.boot_epoch != prior.boot_epoch
                || manifest.first_cursor != prior.end_exclusive
            {
                return Err(CaptureExportError::NonConsecutiveWindows);
            }
        } else {
            first_manifest = Some(manifest);
        }
        total_samples = total_samples
            .checked_add(manifest.sample_count)
            .ok_or(CaptureExportError::CounterOverflow)?;
        total_missing = total_missing
            .checked_add(manifest.missing_sample_count)
            .ok_or(CaptureExportError::CounterOverflow)?;
        total_breaks = total_breaks
            .checked_add(manifest.continuity_break_count)
            .ok_or(CaptureExportError::CounterOverflow)?;
        previous = Some(manifest);
    }

    let first = first_manifest.ok_or(CaptureExportError::NoWindows)?;
    let last = previous.ok_or(CaptureExportError::NoWindows)?;
    Ok(CaptureExportSummary {
        window_count: ordinals.len() as u64,
        sample_count: total_samples,
        missing_sample_count: total_missing,
        continuity_break_count: total_breaks,
        first_cursor: first.first_cursor.get(),
        end_exclusive: last.end_exclusive.get(),
    })
}

/// Streams one boot-scoped Capture directory into analysis-friendly CSV files.
/// Every WAL is independently validated against its manifest before any sample
/// from that window is exported. Memory use is bounded by one wire chunk.
pub fn export_capture_csv(
    capture_directory: impl AsRef<Path>,
    output_directory: impl AsRef<Path>,
) -> Result<CaptureExportSummary, CaptureExportError> {
    let capture_directory = capture_directory.as_ref();
    let output_directory = output_directory.as_ref();
    if output_directory.exists() {
        return Err(CaptureExportError::OutputExists);
    }
    let parent = output_directory
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(CaptureExportError::Io)?;
    fs::create_dir(output_directory).map_err(CaptureExportError::Io)?;

    let result = export_into(capture_directory, output_directory);
    if result.is_err() {
        let _ = fs::remove_dir_all(output_directory);
    }
    result
}

fn export_into(
    capture_directory: &Path,
    output_directory: &Path,
) -> Result<CaptureExportSummary, CaptureExportError> {
    let ordinals = discover_ordinals(capture_directory)?;
    if ordinals.is_empty() {
        return Err(CaptureExportError::NoWindows);
    }
    let mut samples = create_writer(output_directory.join("samples.csv"))?;
    let mut events = create_writer(output_directory.join("events.csv"))?;
    let mut windows = create_writer(output_directory.join("windows.csv"))?;
    writeln!(samples, "cursor,x_raw,y_raw,z_raw,config_revision,timing_segment_id,device_time_us_estimate,max_fit_residual_us,timing_quality_flags")?;
    writeln!(events, "cursor,event,count,reason")?;
    writeln!(windows, "window_ordinal,first_cursor,end_exclusive,sample_count,missing_sample_count,continuity_break_count")?;

    let mut previous: Option<CaptureWindowManifest> = None;
    let mut first_manifest = None;
    let mut total_samples = 0_u64;
    let mut total_missing = 0_u64;
    let mut total_breaks = 0_u64;

    for ordinal in ordinals.iter().copied() {
        let paths = capture_segment_paths(capture_directory, ordinal);
        let validation = validate_capture_segment(&paths.wal, &paths.manifest)
            .map_err(CaptureExportError::Segment)?;
        let manifest = validation.manifest;
        if manifest.window_ordinal != ordinal {
            return Err(CaptureExportError::NonConsecutiveWindows);
        }
        if let Some(prior) = previous {
            if manifest.window_ordinal != prior.window_ordinal.saturating_add(1)
                || manifest.hardware_id != prior.hardware_id
                || manifest.boot_epoch != prior.boot_epoch
                || manifest.first_cursor != prior.end_exclusive
            {
                return Err(CaptureExportError::NonConsecutiveWindows);
            }
        } else {
            first_manifest = Some(manifest);
        }

        writeln!(
            windows,
            "{},{},{},{},{},{}",
            manifest.window_ordinal,
            manifest.first_cursor.get(),
            manifest.end_exclusive.get(),
            manifest.sample_count,
            manifest.missing_sample_count,
            manifest.continuity_break_count
        )?;

        let mut reader = CaptureWalReader::open(&paths.wal).map_err(CaptureExportError::Wal)?;
        while let Some(record) = reader.next_record().map_err(CaptureExportError::Wal)? {
            match record {
                CaptureRecord::SampleChunk(chunk) => {
                    for (index, sample) in chunk.samples.iter().enumerate() {
                        let cursor = chunk
                            .header
                            .first
                            .get()
                            .checked_add(index as u64)
                            .ok_or(CaptureExportError::CounterOverflow)?;
                        let device_time = interpolate_device_time(
                            chunk.timing.first_device_time_us,
                            chunk.timing.last_device_time_us,
                            chunk.samples.len(),
                            index,
                        )?;
                        writeln!(
                            samples,
                            "{},{},{},{},{},{},{},{},{}",
                            cursor,
                            sample.x.get(),
                            sample.y.get(),
                            sample.z.get(),
                            chunk.config_revision,
                            chunk.timing.segment_id,
                            device_time,
                            chunk.timing.max_fit_residual_us,
                            chunk.timing.quality_flags
                        )?;
                    }
                }
                CaptureRecord::Gap(gap) => writeln!(
                    events,
                    "{},gap,{},{:?}",
                    gap.first_missing.get(),
                    gap.missing_count,
                    gap.reason
                )?,
                CaptureRecord::ContinuityBreak(observed) => writeln!(
                    events,
                    "{},continuity_break,1,{:?}",
                    observed.at.get(),
                    observed.reason
                )?,
                CaptureRecord::WindowSeal => {}
            }
        }
        total_samples = total_samples
            .checked_add(manifest.sample_count)
            .ok_or(CaptureExportError::CounterOverflow)?;
        total_missing = total_missing
            .checked_add(manifest.missing_sample_count)
            .ok_or(CaptureExportError::CounterOverflow)?;
        total_breaks = total_breaks
            .checked_add(manifest.continuity_break_count)
            .ok_or(CaptureExportError::CounterOverflow)?;
        previous = Some(manifest);
    }

    samples.flush()?;
    events.flush()?;
    windows.flush()?;
    let first = first_manifest.ok_or(CaptureExportError::NoWindows)?;
    let last = previous.ok_or(CaptureExportError::NoWindows)?;
    let summary = CaptureExportSummary {
        window_count: ordinals.len() as u64,
        sample_count: total_samples,
        missing_sample_count: total_missing,
        continuity_break_count: total_breaks,
        first_cursor: first.first_cursor.get(),
        end_exclusive: last.end_exclusive.get(),
    };
    let metadata = json!({
        "schema": "sensor-system-capture-csv-v1",
        "hardware_id": format_hardware_id(first.hardware_id.bytes()),
        "boot_epoch": first.boot_epoch.get(),
        "window_count": summary.window_count,
        "sample_count": summary.sample_count,
        "missing_sample_count": summary.missing_sample_count,
        "continuity_break_count": summary.continuity_break_count,
        "first_cursor": summary.first_cursor,
        "end_exclusive": summary.end_exclusive,
        "device_time_us_estimate": "linear interpolation between chunk timing anchors; inspect timing flags and max residual",
        "axis_unit": "raw ADXL355 signed sample code"
    });
    let mut metadata_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output_directory.join("metadata.json"))
        .map_err(CaptureExportError::Io)?;
    serde_json::to_writer_pretty(&mut metadata_file, &metadata)
        .map_err(CaptureExportError::Json)?;
    metadata_file.write_all(b"\n")?;
    metadata_file.sync_all()?;
    sync_directory(output_directory)?;
    Ok(summary)
}

fn create_writer(path: PathBuf) -> Result<BufWriter<File>, CaptureExportError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(BufWriter::new)
        .map_err(CaptureExportError::Io)
}

fn discover_ordinals(directory: &Path) -> Result<Vec<u64>, CaptureExportError> {
    let mut ordinals = Vec::new();
    for entry in fs::read_dir(directory).map_err(CaptureExportError::Io)? {
        let entry = entry.map_err(CaptureExportError::Io)?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(number) = name
            .strip_prefix("window-")
            .and_then(|value| value.strip_suffix(".wal"))
        else {
            continue;
        };
        if number.len() == 20 && number.bytes().all(|value| value.is_ascii_digit()) {
            ordinals.push(
                number
                    .parse::<u64>()
                    .map_err(|_| CaptureExportError::InvalidWindowName)?,
            );
        }
    }
    ordinals.sort_unstable();
    if ordinals
        .windows(2)
        .any(|pair| pair[1] != pair[0].saturating_add(1))
    {
        return Err(CaptureExportError::NonConsecutiveWindows);
    }
    Ok(ordinals)
}

fn interpolate_device_time(
    first: u64,
    last: u64,
    count: usize,
    index: usize,
) -> Result<u64, CaptureExportError> {
    if count == 0 || index >= count || last < first {
        return Err(CaptureExportError::InvalidTiming);
    }
    if count == 1 {
        return if index == 0 && first == last {
            Ok(first)
        } else {
            Err(CaptureExportError::InvalidTiming)
        };
    }
    let span = u128::from(last - first);
    let numerator = span
        .checked_mul(index as u128)
        .ok_or(CaptureExportError::CounterOverflow)?;
    let denominator = (count - 1) as u128;
    let offset = numerator
        .checked_add(denominator / 2)
        .ok_or(CaptureExportError::CounterOverflow)?
        / denominator;
    first
        .checked_add(u64::try_from(offset).map_err(|_| CaptureExportError::CounterOverflow)?)
        .ok_or(CaptureExportError::CounterOverflow)
}

fn format_hardware_id(bytes: [u8; 8]) -> String {
    bytes.iter().map(|value| format!("{value:02x}")).collect()
}

fn sync_directory(path: &Path) -> Result<(), CaptureExportError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(CaptureExportError::Io)
}

#[derive(Debug)]
pub enum CaptureExportError {
    Io(io::Error),
    Json(serde_json::Error),
    Segment(CaptureSegmentError),
    Wal(CaptureWalError),
    OutputExists,
    NoWindows,
    InvalidWindowName,
    NonConsecutiveWindows,
    InvalidTiming,
    CounterOverflow,
}

impl From<io::Error> for CaptureExportError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl fmt::Display for CaptureExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "capture export I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "capture metadata failed: {error}"),
            Self::Segment(error) => write!(formatter, "capture validation failed: {error}"),
            Self::Wal(error) => write!(formatter, "capture decoding failed: {error}"),
            other => write!(formatter, "capture export failed: {other:?}"),
        }
    }
}

impl std::error::Error for CaptureExportError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_interpolation_preserves_anchors_and_rounds_integers() {
        assert_eq!(interpolate_device_time(100, 110, 3, 0).unwrap(), 100);
        assert_eq!(interpolate_device_time(100, 110, 3, 1).unwrap(), 105);
        assert_eq!(interpolate_device_time(100, 110, 3, 2).unwrap(), 110);
        assert_eq!(interpolate_device_time(100, 100, 1, 0).unwrap(), 100);
        assert!(interpolate_device_time(100, 101, 1, 0).is_err());
    }
}
