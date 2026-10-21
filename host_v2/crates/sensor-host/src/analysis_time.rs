use core::fmt;
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use chrono::{DateTime, Local, Utc};
#[cfg(any(target_os = "linux", test))]
use sensor_core::ClockObservation;
use sensor_core::{BootEpoch, HardwareId};

use crate::time_wal::TimeWalError;
#[cfg(target_os = "linux")]
use crate::time_wal::{HostClockId, TimeWalReader};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalysisTimestamp {
    pub unix_time_us: u64,
    pub utc: String,
    pub local: String,
    pub local_filename_minute: String,
    pub uncertainty_us: Option<u64>,
    pub method: &'static str,
}

pub struct AnalysisTimeRequest<'a> {
    pub time_root: &'a Path,
    pub channel_name: &'a str,
    pub hardware_id: HardwareId,
    pub node_address: u8,
    pub boot_epoch: BootEpoch,
    pub first_device_time_us: u64,
    pub last_device_time_us: u64,
    pub sealed_manifest: &'a Path,
}

pub fn estimate_first_sample_timestamp(
    request: AnalysisTimeRequest<'_>,
) -> Result<AnalysisTimestamp, AnalysisTimeError> {
    if request.last_device_time_us < request.first_device_time_us {
        return Err(AnalysisTimeError::InvalidDeviceInterval);
    }
    if let Some(estimate) = estimate_from_time_wal(
        request.time_root,
        request.channel_name,
        request.hardware_id,
        request.node_address,
        request.boot_epoch,
        request.first_device_time_us,
    )? {
        return timestamp(
            estimate.unix_time_us,
            Some(estimate.uncertainty_us),
            "time WAL node-to-host fit plus current host CLOCK_BOOTTIME-to-UTC anchor",
        );
    }

    let sealed_at = fs::metadata(request.sealed_manifest)
        .and_then(|metadata| metadata.modified())
        .map_err(AnalysisTimeError::Io)?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AnalysisTimeError::BeforeUnixEpoch)?;
    let sealed_at_us =
        u64::try_from(sealed_at.as_micros()).map_err(|_| AnalysisTimeError::ArithmeticOverflow)?;
    let duration_us = request.last_device_time_us - request.first_device_time_us;
    let unix_time_us = sealed_at_us
        .checked_sub(duration_us)
        .ok_or(AnalysisTimeError::ArithmeticOverflow)?;
    timestamp(
        unix_time_us,
        None,
        "sealed manifest mtime minus exact node sample interval; fallback approximation",
    )
}

struct HostTimeEstimate {
    unix_time_us: u64,
    uncertainty_us: u64,
}

fn estimate_from_time_wal(
    time_root: &Path,
    channel_name: &str,
    hardware_id: HardwareId,
    node_address: u8,
    boot_epoch: BootEpoch,
    device_time_us: u64,
) -> Result<Option<HostTimeEstimate>, AnalysisTimeError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (
            time_root,
            channel_name,
            hardware_id,
            node_address,
            boot_epoch,
            device_time_us,
        );
        Ok(None)
    }

    #[cfg(target_os = "linux")]
    {
        let current_host_clock =
            HostClockId::read_current_linux_boot_id().map_err(AnalysisTimeError::HostClock)?;
        let hardware = hardware_id_hex(hardware_id);
        let parent = time_root.join(channel_name).join(hardware);
        let Some(path) = find_time_wal(&parent, current_host_clock, boot_epoch, node_address)?
        else {
            return Ok(None);
        };
        let mut reader = TimeWalReader::open(path).map_err(AnalysisTimeError::TimeWal)?;
        let mut observations = Vec::new();
        while let Some(observation) = reader
            .next_observation()
            .map_err(AnalysisTimeError::TimeWal)?
        {
            observations.push(observation);
        }
        let Some((host_monotonic_us, uncertainty_us)) =
            interpolate_host_time(&observations, device_time_us)?
        else {
            return Ok(None);
        };
        let boottime = clock_micros(nix::time::ClockId::CLOCK_BOOTTIME)?;
        let realtime = clock_micros(nix::time::ClockId::CLOCK_REALTIME)?;
        let unix_time_us = if host_monotonic_us <= boottime {
            realtime
                .checked_sub(boottime - host_monotonic_us)
                .ok_or(AnalysisTimeError::ArithmeticOverflow)?
        } else {
            realtime
                .checked_add(host_monotonic_us - boottime)
                .ok_or(AnalysisTimeError::ArithmeticOverflow)?
        };
        Ok(Some(HostTimeEstimate {
            unix_time_us,
            uncertainty_us,
        }))
    }
}

#[cfg(target_os = "linux")]
fn find_time_wal(
    parent: &Path,
    current_host_clock: HostClockId,
    boot_epoch: BootEpoch,
    node_address: u8,
) -> Result<Option<std::path::PathBuf>, AnalysisTimeError> {
    let expected = format!("boot-{:016x}-node-{node_address:02x}.wal", boot_epoch.get());
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AnalysisTimeError::Io(error)),
    };
    for entry in entries {
        let path = entry.map_err(AnalysisTimeError::Io)?.path().join(&expected);
        if !path.is_file() {
            continue;
        }
        let reader = TimeWalReader::open(&path).map_err(AnalysisTimeError::TimeWal)?;
        if reader.config().host_clock_id == current_host_clock {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

#[cfg(any(target_os = "linux", test))]
fn interpolate_host_time(
    observations: &[ClockObservation],
    device_time_us: u64,
) -> Result<Option<(u64, u64)>, AnalysisTimeError> {
    if observations.len() < 2 {
        return Ok(None);
    }
    let upper = observations
        .partition_point(|observation| observation.node_midpoint_us() <= device_time_us);
    let (first, second) = if upper == 0 {
        (observations[0], observations[1])
    } else if upper == observations.len() {
        (
            observations[observations.len() - 2],
            observations[observations.len() - 1],
        )
    } else {
        (observations[upper - 1], observations[upper])
    };
    let node_first = first.node_midpoint_us();
    let node_second = second.node_midpoint_us();
    let host_first = first.host_midpoint_us();
    let host_second = second.host_midpoint_us();
    if node_second <= node_first || host_second <= host_first {
        return Err(AnalysisTimeError::InvalidTimeAnchors);
    }
    let node_delta = i128::from(node_second - node_first);
    let host_delta = i128::from(host_second - host_first);
    let target_delta = i128::from(device_time_us) - i128::from(node_first);
    let host_offset = target_delta
        .checked_mul(host_delta)
        .ok_or(AnalysisTimeError::ArithmeticOverflow)?
        / node_delta;
    let host_time = i128::from(host_first)
        .checked_add(host_offset)
        .ok_or(AnalysisTimeError::ArithmeticOverflow)?;
    let host_time = u64::try_from(host_time).map_err(|_| AnalysisTimeError::ArithmeticOverflow)?;
    let outside_us = if device_time_us < node_first {
        node_first - device_time_us
    } else {
        device_time_us.saturating_sub(node_second)
    };
    // 100 ppm covers the RP2350 oscillator budget used by the host model;
    // path uncertainty remains the dominant term near an observation.
    let drift_uncertainty = outside_us.div_ceil(10_000);
    let uncertainty = first
        .uncertainty_us()
        .max(second.uncertainty_us())
        .checked_add(drift_uncertainty)
        .ok_or(AnalysisTimeError::ArithmeticOverflow)?;
    Ok(Some((host_time, uncertainty)))
}

#[cfg(target_os = "linux")]
fn clock_micros(clock: nix::time::ClockId) -> Result<u64, AnalysisTimeError> {
    let value = clock.now().map_err(|_| AnalysisTimeError::ClockRead)?;
    let seconds = u64::try_from(value.tv_sec()).map_err(|_| AnalysisTimeError::ClockRead)?;
    let nanoseconds = u64::try_from(value.tv_nsec()).map_err(|_| AnalysisTimeError::ClockRead)?;
    seconds
        .checked_mul(1_000_000)
        .and_then(|micros| micros.checked_add(nanoseconds / 1_000))
        .ok_or(AnalysisTimeError::ArithmeticOverflow)
}

fn timestamp(
    unix_time_us: u64,
    uncertainty_us: Option<u64>,
    method: &'static str,
) -> Result<AnalysisTimestamp, AnalysisTimeError> {
    let (utc, _) = format_unix_time(unix_time_us, false)?;
    let (local, local_filename_minute) = format_unix_time(unix_time_us, true)?;
    Ok(AnalysisTimestamp {
        unix_time_us,
        utc,
        local,
        local_filename_minute,
        uncertainty_us,
        method,
    })
}

fn format_unix_time(unix_time_us: u64, local: bool) -> Result<(String, String), AnalysisTimeError> {
    let seconds = i64::try_from(unix_time_us / 1_000_000)
        .map_err(|_| AnalysisTimeError::ArithmeticOverflow)?;
    let nanoseconds = u32::try_from((unix_time_us % 1_000_000) * 1_000)
        .map_err(|_| AnalysisTimeError::ArithmeticOverflow)?;
    let utc = DateTime::<Utc>::from_timestamp(seconds, nanoseconds)
        .ok_or(AnalysisTimeError::CalendarConversion)?;
    if local {
        let value = utc.with_timezone(&Local);
        Ok((
            value.format("%Y-%m-%dT%H:%M:%S%.6f%:z").to_string(),
            value.format("%H_%M-%d_%m_%Y").to_string(),
        ))
    } else {
        Ok((
            utc.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string(),
            String::new(),
        ))
    }
}

#[cfg(target_os = "linux")]
fn hardware_id_hex(hardware_id: HardwareId) -> String {
    use core::fmt::Write as _;
    let mut output = String::with_capacity(16);
    for byte in hardware_id.bytes() {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

#[derive(Debug)]
pub enum AnalysisTimeError {
    Io(std::io::Error),
    TimeWal(TimeWalError),
    HostClock(crate::time_wal::HostClockIdError),
    InvalidDeviceInterval,
    InvalidTimeAnchors,
    ClockRead,
    BeforeUnixEpoch,
    CalendarConversion,
    ArithmeticOverflow,
}

impl fmt::Display for AnalysisTimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "analysis time reconstruction failed: {self:?}")
    }
}

impl std::error::Error for AnalysisTimeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_between_time_observations() {
        let boot = BootEpoch::new(7);
        let observations = [
            ClockObservation::new(boot, 1_000, 500, 500, 1_020).unwrap(),
            ClockObservation::new(boot, 2_000, 1_500, 1_500, 2_020).unwrap(),
        ];
        assert_eq!(
            interpolate_host_time(&observations, 1_000).unwrap(),
            Some((1_510, 10))
        );
    }

    #[test]
    fn analysis_filename_minute_is_portable() {
        let (_, label) = format_unix_time(1_799_000_040_000_000, true).unwrap();
        assert_eq!(label.len(), 16);
        assert_eq!(label.as_bytes()[2], b'_');
        assert_eq!(label.as_bytes()[5], b'-');
        assert_eq!(label.as_bytes()[8], b'_');
        assert_eq!(label.as_bytes()[11], b'_');
        assert!(!label.contains(':'));
        assert!(!label.contains('.'));
    }
}
