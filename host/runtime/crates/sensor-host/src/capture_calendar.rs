//! Calendar boundaries use a frozen, durable node/UTC anchor for each WAL.
//! All I/O runs in the existing Capture storage worker, never the RS485 loop.
use sensor_protocol::SampleChunk;
use serde_json::json;
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CalendarSource {
    pub time_root: PathBuf,
    pub channel_name: String,
    pub timezone: chrono_tz::Tz,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CalendarWindow {
    pub anchor_device_us: u64,
    pub anchor_unix_us: u64,
    pub end_device_us: u64,
    pub end_unix_us: u64,
    pub uncertainty_us: Option<u64>,
    pub timezone: chrono_tz::Tz,
}
impl CalendarWindow {
    pub fn new(
        device_us: u64,
        unix_us: u64,
        period_us: u64,
        uncertainty_us: Option<u64>,
    ) -> io::Result<Self> {
        Self::in_timezone(
            device_us,
            unix_us,
            period_us,
            uncertainty_us,
            chrono_tz::UTC,
        )
    }
    pub fn in_timezone(
        device_us: u64,
        unix_us: u64,
        period_us: u64,
        uncertainty_us: Option<u64>,
        timezone: chrono_tz::Tz,
    ) -> io::Result<Self> {
        use chrono::Offset;
        if period_us == 0 {
            return Err(invalid("zero calendar period"));
        }
        let utc = chrono::DateTime::<chrono::Utc>::from_timestamp_micros(
            i64::try_from(unix_us).map_err(|_| invalid("calendar overflow"))?,
        )
        .ok_or_else(|| invalid("calendar overflow"))?;
        let offset = i128::from(
            utc.with_timezone(&timezone)
                .offset()
                .fix()
                .local_minus_utc(),
        ) * 1_000_000;
        let local = i128::from(unix_us) + offset;
        let end_unix_us = u64::try_from(
            (local.div_euclid(i128::from(period_us)) + 1) * i128::from(period_us) - offset,
        )
        .map_err(|_| invalid("calendar overflow"))?;
        let end_device_us = device_us
            .checked_add(end_unix_us - unix_us)
            .ok_or_else(|| invalid("device deadline overflow"))?;
        Ok(Self {
            anchor_device_us: device_us,
            anchor_unix_us: unix_us,
            end_device_us,
            end_unix_us,
            uncertainty_us,
            timezone,
        })
    }
    pub fn unix_time(&self, device_us: u64) -> io::Result<u64> {
        let unix = i128::from(self.anchor_unix_us) + i128::from(device_us)
            - i128::from(self.anchor_device_us);
        u64::try_from(unix).map_err(|_| invalid("calendar time outside range"))
    }
    pub fn save(&self, directory: &Path, ordinal: u64) -> io::Result<()> {
        let path = path(directory, ordinal);
        let temp = path.with_extension("calendar.tmp");
        let mut value = json!({"schema":1,"timezone":self.timezone.name(),"anchor_device_us":self.anchor_device_us,"anchor_unix_us":self.anchor_unix_us,
            "end_device_us":self.end_device_us,"end_unix_us":self.end_unix_us,"uncertainty_us":self.uncertainty_us});
        value["window_ordinal"] = json!(ordinal);
        value["crc32c"] = json!(sensor_protocol::crc32c(&serde_json::to_vec(&value)?));
        let mut file = File::create(&temp)?;
        file.write_all(serde_json::to_string_pretty(&value)?.as_bytes())?;
        file.sync_all()?;
        fs::rename(temp, path)?;
        File::open(directory)?.sync_all()
    }
    pub fn load(directory: &Path, ordinal: u64) -> io::Result<Option<Self>> {
        let bytes = match fs::read(path(directory, ordinal)) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut v: serde_json::Value = serde_json::from_slice(&bytes)?;
        let checksum = v
            .as_object_mut()
            .and_then(|o| o.remove("crc32c"))
            .and_then(|v| v.as_u64())
            .ok_or_else(|| invalid("missing calendar CRC"))?;
        if u64::from(sensor_protocol::crc32c(&serde_json::to_vec(&v)?)) != checksum
            || v["window_ordinal"].as_u64() != Some(ordinal)
        {
            return Err(invalid("calendar CRC or ordinal mismatch"));
        }
        if v["schema"].as_u64() != Some(1) {
            return Err(invalid("unsupported calendar schema"));
        }
        let number = |key: &str| {
            v[key]
                .as_u64()
                .ok_or_else(|| invalid("invalid calendar field"))
        };
        let window = Self {
            anchor_device_us: number("anchor_device_us")?,
            anchor_unix_us: number("anchor_unix_us")?,
            end_device_us: number("end_device_us")?,
            end_unix_us: number("end_unix_us")?,
            uncertainty_us: v["uncertainty_us"].as_u64(),
            timezone: v["timezone"]
                .as_str()
                .ok_or_else(|| invalid("missing timezone"))?
                .parse()
                .map_err(|_| invalid("invalid timezone"))?,
        };
        if window.end_device_us <= window.anchor_device_us
            || window.end_unix_us <= window.anchor_unix_us
            || window.end_device_us - window.anchor_device_us
                != window.end_unix_us - window.anchor_unix_us
        {
            return Err(invalid("inconsistent calendar deadline"));
        }
        Ok(Some(window))
    }
}
fn path(directory: &Path, ordinal: u64) -> PathBuf {
    directory.join(format!("window-{ordinal:020}.calendar.json"))
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

impl CalendarSource {
    pub fn estimate(
        &self,
        hardware: sensor_core::HardwareId,
        boot: sensor_core::BootEpoch,
        chunk: &SampleChunk,
        period: u64,
    ) -> io::Result<CalendarWindow> {
        let device = chunk.timing.first_device_time_us;
        // The first window may precede the first two TimeSync observations. Its
        // receive-time fallback is explicitly uncertain. Later windows use Time WAL.
        let now = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| invalid("clock before epoch"))?
                .as_micros(),
        )
        .map_err(|_| invalid("clock overflow"))?;
        let hardware_hex: String = hardware
            .bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let parent = self.time_root.join(&self.channel_name).join(hardware_hex);
        if let Ok(host_boots) = fs::read_dir(parent) {
            for entry in host_boots {
                let Ok(entry) = entry else { continue };
                let Ok(wals) = fs::read_dir(entry.path()) else {
                    continue;
                };
                for wal in wals {
                    let Ok(wal) = wal else { continue };
                    let Ok(reader) = crate::time_wal::TimeWalReader::open(wal.path()) else {
                        continue;
                    };
                    let config = reader.config();
                    if config.boot_epoch != boot || config.hardware_id != hardware {
                        continue;
                    }
                    if let Ok(Some(estimate)) = crate::analysis_time::estimate_from_time_wal(
                        &self.time_root,
                        &self.channel_name,
                        hardware,
                        config.node_address,
                        boot,
                        device,
                    ) {
                        return CalendarWindow::in_timezone(
                            device,
                            estimate.unix_time_us,
                            period,
                            Some(estimate.uncertainty_us),
                            self.timezone,
                        );
                    }
                }
            }
        }
        CalendarWindow::in_timezone(
            device,
            now.checked_sub(chunk.timing.last_device_time_us - device)
                .ok_or_else(|| invalid("receive anchor underflow"))?,
            period,
            None,
            self.timezone,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};
    #[test]
    fn start_is_partial_and_exact_boundary_belongs_to_next_window() {
        let unix = chrono::Utc
            .with_ymd_and_hms(2026, 11, 2, 11, 3, 27)
            .unwrap()
            .timestamp_micros() as u64;
        let first = CalendarWindow::in_timezone(
            100,
            unix,
            600_000_000,
            Some(10),
            chrono_tz::Europe::Warsaw,
        )
        .unwrap();
        assert_eq!(first.end_device_us, 100 + 393_000_000);
        let next = CalendarWindow::in_timezone(
            first.end_device_us,
            first.end_unix_us,
            600_000_000,
            Some(10),
            first.timezone,
        )
        .unwrap();
        assert_eq!(next.end_device_us - first.end_device_us, 600_000_000);
    }
    #[test]
    fn dst_and_fractional_offset_keep_distinct_utc_boundaries() {
        for hour in [0, 1] {
            let unix = chrono::Utc
                .with_ymd_and_hms(2026, 10, 25, hour, 5, 0)
                .unwrap()
                .timestamp_micros() as u64;
            let w =
                CalendarWindow::in_timezone(0, unix, 600_000_000, None, chrono_tz::Europe::Warsaw)
                    .unwrap();
            assert_eq!(w.end_unix_us, unix + 300_000_000);
        }
        let unix = chrono::Utc
            .with_ymd_and_hms(2026, 11, 2, 0, 0, 0)
            .unwrap()
            .timestamp_micros() as u64;
        let w = CalendarWindow::in_timezone(0, unix, 600_000_000, None, chrono_tz::Asia::Kathmandu)
            .unwrap();
        let utc =
            chrono::DateTime::<chrono::Utc>::from_timestamp_micros(w.end_unix_us as i64).unwrap();
        assert_eq!(utc.with_timezone(&w.timezone).minute() % 10, 0);
    }
    #[test]
    fn frozen_anchor_survives_restart_and_corruption_fails_closed() {
        let root = std::env::temp_dir().join(format!("calendar-check-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let w = CalendarWindow::new(100, 1700000000000000, 600_000_000, None).unwrap();
        w.save(&root, 0).unwrap();
        assert_eq!(CalendarWindow::load(&root, 0).unwrap(), Some(w));
        let p = path(&root, 0);
        let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        v["anchor_unix_us"] = json!(1);
        fs::write(p, serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(CalendarWindow::load(&root, 0).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
