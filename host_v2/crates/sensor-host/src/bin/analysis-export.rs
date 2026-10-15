use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sensor_core::HardwareId;
use sensor_host::analysis_export::{export_analysis_window, AnalysisWindowIdentity};
use sensor_host::configuration_history::read_configuration_snapshots;
use sensor_host::installation_topology::HostInstallationProfile;

fn main() -> ExitCode {
    let arguments = match parse_arguments(env::args_os().skip(1).collect()) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("analysis-export: {error}");
            return ExitCode::from(64);
        }
    };
    if arguments.print_interval {
        return match HostInstallationProfile::load(&arguments.config, &arguments.repository_root) {
            Ok(profile) => {
                println!("{}", profile.storage().analysis().scan_interval_seconds());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("analysis-export: {error}");
                ExitCode::from(2)
            }
        };
    }
    match export_new(arguments) {
        Ok(summary) => {
            println!(
                "analysis export complete: created={} existing={} failed=0",
                summary.created, summary.existing
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("analysis-export: {error}");
            ExitCode::from(2)
        }
    }
}

struct Arguments {
    config: PathBuf,
    repository_root: PathBuf,
    print_interval: bool,
}

#[derive(Default)]
struct Summary {
    created: u64,
    existing: u64,
}

fn parse_arguments(arguments: Vec<OsString>) -> Result<Arguments, String> {
    if arguments.as_slice() == ["--help"] || arguments.as_slice() == ["-h"] {
        return Err(usage());
    }
    let mut config = None;
    let mut repository_root = None;
    let mut print_interval = false;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].to_str() {
            Some("--config") if index + 1 < arguments.len() => {
                config = Some(PathBuf::from(&arguments[index + 1]));
                index += 2;
            }
            Some("--repository-root") if index + 1 < arguments.len() => {
                repository_root = Some(PathBuf::from(&arguments[index + 1]));
                index += 2;
            }
            Some("--print-interval") => {
                print_interval = true;
                index += 1;
            }
            _ => return Err(usage()),
        }
    }
    Ok(Arguments {
        config: config.ok_or_else(usage)?,
        repository_root: repository_root.ok_or_else(usage)?,
        print_interval,
    })
}

fn usage() -> String {
    "usage: analysis-export [--print-interval] --config PATH --repository-root PATH".to_owned()
}

fn export_new(arguments: Arguments) -> Result<Summary, Box<dyn std::error::Error>> {
    let profile = HostInstallationProfile::load(&arguments.config, &arguments.repository_root)?;
    let analysis = profile.storage().analysis();
    if !analysis.enabled() {
        println!("analysis export disabled by configuration");
        return Ok(Summary::default());
    }
    fs::create_dir_all(analysis.root())?;
    let mut summary = Summary::default();
    for channel in profile.topology().enabled_channels() {
        for node in channel.enabled_nodes() {
            let stream_parent = profile
                .storage()
                .capture()
                .root()
                .join(channel.name())
                .join(hardware_id_hex(node.hardware_id()));
            if !stream_parent.is_dir() {
                continue;
            }
            let configurations = read_configuration_snapshots(
                profile.storage().capture().root(),
                node.hardware_id(),
            )?;
            let Some(current) = configurations.last() else {
                continue;
            };
            let stream = profile.storage().capture().stream_directory(
                channel.name(),
                node.hardware_id(),
                current.boot_epoch,
            )?;
            let output = analysis.root().join(channel.name());
            let ordinals = pending_ordinals(
                &output,
                node.hardware_id(),
                current.boot_epoch.get(),
                sealed_ordinals(&stream)?,
                analysis.initial_backfill_windows(),
            )?;
            for ordinal in ordinals {
                let result = export_analysis_window(
                    &stream,
                    &output,
                    ordinal,
                    AnalysisWindowIdentity {
                        channel_name: channel.name(),
                        channel_id: channel.channel_id(),
                        node_address: node.node_address(),
                    },
                    &configurations,
                )?;
                if result.already_exists {
                    summary.existing += 1;
                } else {
                    summary.created += 1;
                    println!(
                        "analysis window published: path={} samples={} gaps={} breaks={} first_device_time_us={} estimated_odr_hz={}",
                        result.csv_path.display(),
                        result.sample_count,
                        result.missing_sample_count,
                        result.continuity_break_count,
                        result.first_device_time_us,
                        result
                            .estimated_odr_hz
                            .map(|value| format!("{value:.6}"))
                            .unwrap_or_else(|| "unavailable".to_owned())
                    );
                }
            }
        }
    }
    Ok(summary)
}

fn sealed_ordinals(stream: &Path) -> Result<Vec<u64>, std::io::Error> {
    let mut result = Vec::new();
    for entry in fs::read_dir(stream)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(value) = name
            .strip_prefix("window-")
            .and_then(|value| value.strip_suffix(".manifest"))
        else {
            continue;
        };
        if value.len() == 20 && value.bytes().all(|byte| byte.is_ascii_digit()) {
            if let Ok(ordinal) = value.parse() {
                result.push(ordinal);
            }
        }
    }
    result.sort_unstable();
    Ok(result)
}

fn pending_ordinals(
    output_directory: &Path,
    hardware_id: HardwareId,
    boot_epoch: u64,
    ordinals: Vec<u64>,
    initial_backfill_windows: usize,
) -> Result<Vec<u64>, std::io::Error> {
    let marker = format!("_{}_boot{boot_epoch:016x}_w", hardware_id_hex(hardware_id));
    let mut last_exported = None;
    let entries = match fs::read_dir(output_directory) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if let Some(entries) = entries {
        for entry in entries {
            let name = entry?.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(value) = name
                .strip_suffix(".csv")
                .and_then(|value| value.rsplit_once(&marker).map(|(_, value)| value))
            else {
                continue;
            };
            if value.len() == 20 && value.bytes().all(|byte| byte.is_ascii_digit()) {
                if let Ok(ordinal) = value.parse::<u64>() {
                    last_exported =
                        Some(last_exported.map_or(ordinal, |last: u64| last.max(ordinal)));
                }
            }
        }
    }
    Ok(match last_exported {
        Some(last) => ordinals
            .into_iter()
            .filter(|ordinal| *ordinal > last)
            .collect(),
        None => {
            let skip = ordinals.len().saturating_sub(initial_backfill_windows);
            ordinals.into_iter().skip(skip).collect()
        }
    })
}

fn hardware_id_hex(hardware_id: HardwareId) -> String {
    hardware_id
        .bytes()
        .iter()
        .map(|value| format!("{value:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HARDWARE: HardwareId = HardwareId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);

    #[test]
    fn first_run_exports_only_the_configured_tail() {
        let root =
            std::env::temp_dir().join(format!("analysis-export-selection-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            pending_ordinals(&root, HARDWARE, 9, vec![0, 1, 2, 3], 1).unwrap(),
            vec![3]
        );
    }

    #[test]
    fn existing_product_is_a_monotonic_export_cursor() {
        let root =
            std::env::temp_dir().join(format!("analysis-export-cursor-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("device-t00000000000000000001us_ch02_node001_0102030405060708_boot0000000000000009_w00000000000000000002.csv"),
            b"complete",
        )
        .unwrap();
        assert_eq!(
            pending_ordinals(&root, HARDWARE, 9, vec![0, 1, 2, 3, 4], 1).unwrap(),
            vec![3, 4]
        );
        fs::remove_dir_all(root).unwrap();
    }
}
