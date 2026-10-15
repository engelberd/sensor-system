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
            let streams = directories(&stream_parent)?;
            if streams.is_empty() {
                continue;
            }
            let configurations = read_configuration_snapshots(
                profile.storage().capture().root(),
                node.hardware_id(),
            )?;
            for stream in streams {
                for ordinal in sealed_ordinals(&stream)? {
                    let result = export_analysis_window(
                        &stream,
                        &analysis.root().join(channel.name()),
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
    }
    Ok(summary)
}

fn directories(parent: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut result = Vec::new();
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(result),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("boot-"))
        {
            result.push(entry.path());
        }
    }
    result.sort();
    Ok(result)
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

fn hardware_id_hex(hardware_id: HardwareId) -> String {
    hardware_id
        .bytes()
        .iter()
        .map(|value| format!("{value:02x}"))
        .collect()
}
