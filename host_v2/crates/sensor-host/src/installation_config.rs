use core::fmt;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value};

use crate::capture_profile::{CaptureInstallationProfile, CaptureProfileError};
use crate::capture_window::{CaptureWindowError, CaptureWindowPolicy};
use crate::persistence_executor::{GroupCommitPolicy, GroupedWriterError};
use crate::retention_checkpoint::{RetentionCheckpointError, RetentionPolicy};
use crate::time_profile::{
    TimePersistenceInstallationProfile, TimeProfileError, TimeSyncInstallationPolicy,
};
use crate::transaction::DurationMicros;

const CAPTURE_V2_KEYS: [&str; 7] = [
    "checkpoint_dir",
    "queue_capacity_records",
    "group_commit_max_records",
    "group_commit_max_wait_ms",
    "worker_prefix",
    "capture_grace_seconds",
    "archive_retention_seconds",
];
const TIME_V2_KEYS: [&str; 6] = [
    "root_dir",
    "queue_capacity_records",
    "worker_name",
    "sync_interval_ms",
    "sync_estimated_slot_ms",
    "sync_retry_delay_ms",
];

pub struct InstallationStorageProfile {
    capture: CaptureInstallationProfile,
    minimum_free_bytes: u64,
    checkpoint_root: PathBuf,
    retention: RetentionPolicy,
    time_persistence: TimePersistenceInstallationProfile,
    time_sync: TimeSyncInstallationPolicy,
}

impl InstallationStorageProfile {
    pub fn load(
        config_path: impl AsRef<Path>,
        repository_root: impl AsRef<Path>,
    ) -> Result<Self, InstallationConfigError> {
        let document = load_merged_document(config_path.as_ref())?;
        Self::from_document(&document, repository_root.as_ref())
    }

    pub(crate) fn from_document(
        document: &Value,
        repository_root: &Path,
    ) -> Result<Self, InstallationConfigError> {
        let root = document
            .as_object()
            .ok_or(InstallationConfigError::RootNotObject)?;
        let storage = object_field(root, "storage")?;
        let capture_v2 = object_field(storage, "capture_v2")?;
        reject_unknown_keys(capture_v2, &CAPTURE_V2_KEYS, "capture_v2")?;
        let time_v2 = object_field(storage, "time_v2")?;
        reject_unknown_keys(time_v2, &TIME_V2_KEYS, "time_v2")?;

        let capture_root = anchored_path(repository_root, string_field(storage, "root_dir")?);
        let minimum_free_bytes = nonzero_unsigned_field(storage, "min_free_bytes")?;
        let checkpoint_root =
            anchored_path(repository_root, string_field(capture_v2, "checkpoint_dir")?);
        let window_us = unsigned_field(storage, "window_seconds")?
            .checked_mul(1_000_000)
            .ok_or(InstallationConfigError::DurationOverflow)?;
        let window_policy =
            CaptureWindowPolicy::new(window_us).map_err(InstallationConfigError::WindowPolicy)?;
        let queue_capacity_records = usize_field(capture_v2, "queue_capacity_records")?;
        let group_commit_policy = GroupCommitPolicy::new(
            usize_field(capture_v2, "group_commit_max_records")?,
            Duration::from_millis(nonzero_unsigned_field(
                capture_v2,
                "group_commit_max_wait_ms",
            )?),
        )
        .map_err(InstallationConfigError::GroupCommitPolicy)?;
        let capture = CaptureInstallationProfile::new(
            capture_root,
            window_policy,
            queue_capacity_records,
            group_commit_policy,
            string_field(capture_v2, "worker_prefix")?,
        )
        .map_err(InstallationConfigError::CaptureProfile)?;
        let retention = RetentionPolicy::new(
            unsigned_field(capture_v2, "capture_grace_seconds")?,
            unsigned_field(capture_v2, "archive_retention_seconds")?,
        )
        .map_err(InstallationConfigError::RetentionPolicy)?;
        let time_persistence = TimePersistenceInstallationProfile::new(
            anchored_path(repository_root, string_field(time_v2, "root_dir")?),
            usize_field(time_v2, "queue_capacity_records")?,
            string_field(time_v2, "worker_name")?,
        )
        .map_err(InstallationConfigError::TimeProfile)?;
        let time_sync = TimeSyncInstallationPolicy::new(
            duration_micros_from_ms(time_v2, "sync_interval_ms")?,
            duration_micros_from_ms(time_v2, "sync_estimated_slot_ms")?,
            duration_micros_from_ms(time_v2, "sync_retry_delay_ms")?,
        )
        .map_err(InstallationConfigError::TimeProfile)?;

        Ok(Self {
            capture,
            minimum_free_bytes,
            checkpoint_root,
            retention,
            time_persistence,
            time_sync,
        })
    }

    pub const fn capture(&self) -> &CaptureInstallationProfile {
        &self.capture
    }

    pub const fn minimum_free_bytes(&self) -> u64 {
        self.minimum_free_bytes
    }

    pub fn checkpoint_root(&self) -> &Path {
        &self.checkpoint_root
    }

    pub const fn retention(&self) -> RetentionPolicy {
        self.retention
    }

    pub const fn time_persistence(&self) -> &TimePersistenceInstallationProfile {
        &self.time_persistence
    }

    pub const fn time_sync(&self) -> TimeSyncInstallationPolicy {
        self.time_sync
    }
}

pub(crate) fn load_merged_document(path: &Path) -> Result<Value, InstallationConfigError> {
    load_document(path, &mut Vec::new())
}

fn load_document(path: &Path, stack: &mut Vec<PathBuf>) -> Result<Value, InstallationConfigError> {
    let canonical = fs::canonicalize(path).map_err(InstallationConfigError::Io)?;
    if stack.contains(&canonical) {
        return Err(InstallationConfigError::CircularExtends);
    }
    stack.push(canonical.clone());
    let bytes = fs::read(&canonical).map_err(InstallationConfigError::Io)?;
    let mut current: Value =
        serde_json::from_slice(&bytes).map_err(InstallationConfigError::Json)?;
    let current_object = current
        .as_object_mut()
        .ok_or(InstallationConfigError::RootNotObject)?;
    let extends = current_object.remove("extends");
    let result = if let Some(extends) = extends {
        let relative = extends
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or(InstallationConfigError::InvalidExtends)?;
        let base_path = canonical
            .parent()
            .ok_or(InstallationConfigError::MissingParent)?
            .join(relative);
        let mut base = load_document(&base_path, stack)?;
        merge_values(&mut base, current, None)?;
        base
    } else {
        current
    };
    stack.pop();
    Ok(result)
}

fn merge_values(
    base: &mut Value,
    overlay: Value,
    section: Option<&str>,
) -> Result<(), InstallationConfigError> {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                if let Some(existing) = base.get_mut(&key) {
                    merge_values(existing, value, Some(&key))?;
                } else {
                    base.insert(key, value);
                }
            }
        }
        (Value::Array(base), Value::Array(overlay))
            if matches!(section, Some("channels" | "nodes")) =>
        {
            merge_identity_array(base, overlay, section.expect("matched section"))?;
        }
        (base, overlay) => *base = overlay,
    }
    Ok(())
}

fn merge_identity_array(
    base: &mut Vec<Value>,
    overlay: Vec<Value>,
    section: &str,
) -> Result<(), InstallationConfigError> {
    for item in overlay {
        let identity = item_identity(&item, section)?;
        let mut existing = None;
        for (index, candidate) in base.iter().enumerate() {
            if item_identity(candidate, section)? == identity {
                existing = Some(index);
                break;
            }
        }
        if let Some(index) = existing {
            merge_values(&mut base[index], item, Some(section))?;
        } else {
            base.push(item);
        }
    }
    Ok(())
}

fn item_identity(value: &Value, section: &str) -> Result<String, InstallationConfigError> {
    if section == "nodes" {
        if let Some(identity) = value.as_u64() {
            return Ok(format!("n:{identity}"));
        }
    }
    let object = value
        .as_object()
        .ok_or(InstallationConfigError::InvalidIdentityArray)?;
    let identity = if section == "channels" {
        object.get("name")
    } else {
        object.get("id").or_else(|| object.get("node_id"))
    }
    .ok_or(InstallationConfigError::InvalidIdentityArray)?;
    match identity {
        Value::String(value) if !value.is_empty() => Ok(format!("s:{value}")),
        Value::Number(value) if value.as_u64().is_some() => Ok(format!("n:{value}")),
        _ => Err(InstallationConfigError::InvalidIdentityArray),
    }
}

fn anchored_path(repository_root: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        repository_root.join(path)
    }
}

fn object_field<'a>(
    object: &'a Map<String, Value>,
    key: &'static str,
) -> Result<&'a Map<String, Value>, InstallationConfigError> {
    object
        .get(key)
        .ok_or(InstallationConfigError::MissingField(key))?
        .as_object()
        .ok_or(InstallationConfigError::InvalidFieldType(key))
}

fn string_field<'a>(
    object: &'a Map<String, Value>,
    key: &'static str,
) -> Result<&'a str, InstallationConfigError> {
    object
        .get(key)
        .ok_or(InstallationConfigError::MissingField(key))?
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or(InstallationConfigError::InvalidFieldType(key))
}

fn unsigned_field(
    object: &Map<String, Value>,
    key: &'static str,
) -> Result<u64, InstallationConfigError> {
    object
        .get(key)
        .ok_or(InstallationConfigError::MissingField(key))?
        .as_u64()
        .ok_or(InstallationConfigError::InvalidFieldType(key))
}

fn nonzero_unsigned_field(
    object: &Map<String, Value>,
    key: &'static str,
) -> Result<u64, InstallationConfigError> {
    let value = unsigned_field(object, key)?;
    if value == 0 {
        Err(InstallationConfigError::ZeroField(key))
    } else {
        Ok(value)
    }
}

fn usize_field(
    object: &Map<String, Value>,
    key: &'static str,
) -> Result<usize, InstallationConfigError> {
    usize::try_from(nonzero_unsigned_field(object, key)?)
        .map_err(|_| InstallationConfigError::FieldOverflow(key))
}

fn duration_micros_from_ms(
    object: &Map<String, Value>,
    key: &'static str,
) -> Result<DurationMicros, InstallationConfigError> {
    let micros = nonzero_unsigned_field(object, key)?
        .checked_mul(1_000)
        .ok_or(InstallationConfigError::FieldOverflow(key))?;
    DurationMicros::new(micros).map_err(|_| InstallationConfigError::ZeroField(key))
}

fn reject_unknown_keys(
    object: &Map<String, Value>,
    allowed: &[&str],
    section: &'static str,
) -> Result<(), InstallationConfigError> {
    let allowed: HashSet<&str> = allowed.iter().copied().collect();
    if let Some(key) = object.keys().find(|key| !allowed.contains(key.as_str())) {
        Err(InstallationConfigError::UnknownSectionField {
            section,
            key: key.clone(),
        })
    } else {
        Ok(())
    }
}

#[derive(Debug)]
pub enum InstallationConfigError {
    Io(io::Error),
    Json(serde_json::Error),
    RootNotObject,
    InvalidExtends,
    CircularExtends,
    MissingParent,
    InvalidIdentityArray,
    MissingField(&'static str),
    InvalidFieldType(&'static str),
    ZeroField(&'static str),
    FieldOverflow(&'static str),
    UnknownSectionField { section: &'static str, key: String },
    DurationOverflow,
    WindowPolicy(CaptureWindowError),
    GroupCommitPolicy(GroupedWriterError),
    CaptureProfile(CaptureProfileError),
    RetentionPolicy(RetentionCheckpointError),
    TimeProfile(TimeProfileError),
}

impl fmt::Display for InstallationConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "installation config I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "installation config JSON failed: {error}"),
            other => write!(formatter, "installation config error: {other:?}"),
        }
    }
}

impl std::error::Error for InstallationConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::WindowPolicy(error) => Some(error),
            Self::GroupCommitPolicy(error) => Some(error),
            Self::CaptureProfile(error) => Some(error),
            Self::RetentionPolicy(error) => Some(error),
            Self::TimeProfile(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sensor-host-installation-config-{}-{id}",
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

    fn common_json(extra_capture: &str, extra_time: &str) -> String {
        format!(
            r#"{{
                "storage": {{
                    "root_dir": "var/recordings",
                    "window_seconds": 600,
                    "min_free_bytes": 1073741824,
                    "capture_v2": {{
                        "checkpoint_dir": "var/recordings/.retention",
                        "queue_capacity_records": 64,
                        "group_commit_max_records": 8,
                        "group_commit_max_wait_ms": 5,
                        "worker_prefix": "capture",
                        "capture_grace_seconds": 172800,
                        "archive_retention_seconds": 2592000
                        {extra_capture}
                    }},
                    "time_v2": {{
                        "root_dir": "var/recordings/time",
                        "queue_capacity_records": 64,
                        "worker_name": "time-storage",
                        "sync_interval_ms": 10000,
                        "sync_estimated_slot_ms": 20,
                        "sync_retry_delay_ms": 250
                        {extra_time}
                    }}
                }},
                "channels": [{{"name": "line-a", "port": "a", "nodes": [{{"id": 1}}]}}]
            }}"#
        )
    }

    #[test]
    fn tracked_profile_extends_common_and_anchors_runtime_paths() {
        let temp = TempDirectory::new();
        fs::write(temp.0.join("common.json"), common_json("", "")).unwrap();
        fs::write(
            temp.0.join("system.json"),
            r#"{
                "extends": "common.json",
                "storage": {"capture_v2": {"group_commit_max_records": 16}},
                "channels": [{"name": "line-a", "enabled": false}]
            }"#,
        )
        .unwrap();

        let profile =
            InstallationStorageProfile::load(temp.0.join("system.json"), &temp.0).unwrap();
        assert_eq!(
            profile.checkpoint_root(),
            temp.0.join("var/recordings/.retention")
        );
        assert_eq!(profile.retention().capture_grace_seconds, 172800);
        assert_eq!(profile.minimum_free_bytes(), 1_073_741_824);
        assert_eq!(profile.retention().archive_retention_seconds, 2592000);
        assert_eq!(
            profile.time_persistence().root(),
            temp.0.join("var/recordings/time")
        );
        assert_eq!(profile.time_sync().schedule().interval.get(), 10_000_000);
    }

    #[test]
    fn capture_v2_rejects_unknown_fields_and_circular_extends() {
        let temp = TempDirectory::new();
        fs::write(
            temp.0.join("unknown.json"),
            common_json(", \"group_comit_max_records\": 3", ""),
        )
        .unwrap();
        assert!(matches!(
            InstallationStorageProfile::load(temp.0.join("unknown.json"), &temp.0),
            Err(InstallationConfigError::UnknownSectionField {
                section: "capture_v2",
                ..
            })
        ));

        fs::write(temp.0.join("a.json"), r#"{"extends":"b.json"}"#).unwrap();
        fs::write(temp.0.join("b.json"), r#"{"extends":"a.json"}"#).unwrap();
        assert!(matches!(
            InstallationStorageProfile::load(temp.0.join("a.json"), &temp.0),
            Err(InstallationConfigError::CircularExtends)
        ));
    }

    #[test]
    fn time_v2_rejects_unknown_fields_and_invalid_schedule() {
        let temp = TempDirectory::new();
        fs::write(
            temp.0.join("unknown.json"),
            common_json("", ", \"sync_intervall_ms\": 3"),
        )
        .unwrap();
        assert!(matches!(
            InstallationStorageProfile::load(temp.0.join("unknown.json"), &temp.0),
            Err(InstallationConfigError::UnknownSectionField {
                section: "time_v2",
                ..
            })
        ));

        fs::write(
            temp.0.join("invalid.json"),
            common_json(
                "",
                ", \"sync_interval_ms\": 10, \"sync_estimated_slot_ms\": 11",
            ),
        )
        .unwrap();
        assert!(matches!(
            InstallationStorageProfile::load(temp.0.join("invalid.json"), &temp.0),
            Err(InstallationConfigError::TimeProfile(_))
        ));
    }

    #[test]
    fn tracked_repository_profile_remains_loadable() {
        let manifest_directory = Path::new(env!("CARGO_MANIFEST_DIR"));
        let repository_root = manifest_directory.ancestors().nth(3).unwrap();
        let profile = InstallationStorageProfile::load(
            repository_root.join("host/configs/systems/rpi-sanok.json"),
            repository_root,
        )
        .unwrap();
        assert_eq!(
            profile.checkpoint_root(),
            repository_root.join("var/recordings/.retention")
        );
        assert_eq!(profile.retention(), RetentionPolicy::DEFAULT);
    }
}
