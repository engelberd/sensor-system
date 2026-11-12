use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sensor_core::HardwareId;
use sensor_host::analysis_export::{
    analysis_product_index, export_analysis_window, AnalysisWindowIdentity,
};
use sensor_host::configuration_history::read_configuration_snapshots;
use sensor_host::installation_topology::HostInstallationProfile;
use sensor_host::telemetry_profile::DiscoveredTelemetryStream;

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
                "analysis export complete: created={} existing={} failed={}",
                summary.created, summary.existing, summary.failed
            );
            if summary.failed == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            }
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
    backfill_all: bool,
}

#[derive(Default)]
struct Summary {
    created: u64,
    existing: u64,
    failed: u64,
}

fn parse_arguments(arguments: Vec<OsString>) -> Result<Arguments, String> {
    if arguments.as_slice() == ["--help"] || arguments.as_slice() == ["-h"] {
        return Err(usage());
    }
    let mut config = None;
    let mut repository_root = None;
    let mut print_interval = false;
    let mut backfill_all = false;
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
            Some("--backfill-all") => {
                backfill_all = true;
                index += 1;
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
        backfill_all,
    })
}

fn usage() -> String {
    "usage: analysis-export [--print-interval] [--backfill-all] --config PATH --repository-root PATH".to_owned()
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
            let epochs: std::collections::BTreeSet<_> =
                configurations.iter().map(|c| c.boot_epoch.get()).collect();
            for epoch in epochs {
                let boot_epoch = sensor_core::BootEpoch::new(epoch);
                let stream = profile.storage().capture().stream_directory(
                    channel.name(),
                    node.hardware_id(),
                    boot_epoch,
                )?;
                if !stream.is_dir() {
                    continue;
                }
                let output = analysis.root().to_path_buf();
                let telemetry = profile.storage().telemetry_persistence().stream_spec(
                    DiscoveredTelemetryStream {
                        channel_name: channel.name().to_owned(),
                        hardware_id: node.hardware_id(),
                        node_address: node.node_address(),
                        boot_epoch,
                    },
                )?;
                let ordinals = pending_ordinals(
                    &output,
                    node.hardware_id(),
                    boot_epoch.get(),
                    sealed_ordinals(&stream)?,
                    if arguments.backfill_all {
                        usize::MAX
                    } else {
                        analysis.initial_backfill_windows()
                    },
                )?;
                let ordinals = if arguments.backfill_all {
                    sealed_ordinals(&stream)?
                } else {
                    ordinals
                };
                for ordinal in ordinals {
                    let result = match export_analysis_window(
                        &stream,
                        &output,
                        profile.storage().time_persistence().root(),
                        &telemetry.path,
                        ordinal,
                        AnalysisWindowIdentity {
                            timezone: profile.system_timezone(),
                            channel_name: channel.name(),
                            channel_id: channel.channel_id(),
                            node_address: node.node_address(),
                        },
                        &configurations,
                    ) {
                        Ok(result) => result,
                        Err(error) => {
                            summary.failed = summary.failed.saturating_add(1);
                            eprintln!(
                                "analysis window skipped: channel={} node={} ordinal={} error={}",
                                channel.name(),
                                node.node_address(),
                                ordinal,
                                error
                            );
                            continue;
                        }
                    };
                    if result.already_exists {
                        summary.existing += 1;
                    } else {
                        summary.created += 1;
                        println!(
                        "analysis window published: path={} samples={} gaps={} breaks={} first_device_time_us={} estimated_odr_hz={}",
                        result.hdf5_path.display(),
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
    let mut exported = std::collections::BTreeSet::new();
    let index = analysis_product_index(output_directory).map_err(std::io::Error::other)?;
    for metadata in index
        .as_array()
        .ok_or_else(|| std::io::Error::other("invalid HDF5 product index"))?
    {
        if metadata["hardware_id"].as_str() == Some(hardware_id_hex(hardware_id).as_str())
            && metadata["boot_epoch"].as_u64() == Some(boot_epoch)
        {
            if let Some(ordinal) = metadata["window_ordinal"].as_u64() {
                exported.insert(ordinal);
            }
        }
    }
    Ok(if let Some(first) = exported.first().copied() {
        ordinals
            .into_iter()
            .filter(|ordinal| *ordinal >= first && !exported.contains(ordinal))
            .collect()
    } else {
        let skip = ordinals.len().saturating_sub(initial_backfill_windows);
        ordinals.into_iter().skip(skip).collect()
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
    fn hdf5_metadata_preserves_holes_independently_of_filename() {
        let root =
            std::env::temp_dir().join(format!("analysis-hdf5-cursor-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let status = std::process::Command::new(std::env::var_os("SENSOR_ANALYSIS_PYTHON").unwrap_or_else(|| "python3".into()))
            .arg("-c").arg("import h5py,sys; from pathlib import Path; root=Path(sys.argv[1]);\nfor ordinal,name in [(1,'line-b_2026-11-11_16-38.h5'),(3,'arbitrary-renamed.h5')]:\n with h5py.File(root/name,'w') as f: f.attrs.update(schema='sensor-system-analysis-hdf5-v6',hardware_id='0102030405060708',boot_epoch=9,window_ordinal=ordinal)")
            .arg(&root).status().unwrap();
        assert!(status.success());
        fs::write(root.join(".unfinished.h5.tmp"), b"partial").unwrap();
        assert_eq!(
            pending_ordinals(&root, HARDWARE, 9, vec![0, 1, 2, 3, 4], 1).unwrap(),
            vec![2, 4]
        );
        fs::remove_dir_all(root).unwrap();
    }
}
