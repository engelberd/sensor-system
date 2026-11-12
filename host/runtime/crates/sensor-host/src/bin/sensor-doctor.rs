use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use sensor_host::host_doctor::{run_doctor, DoctorMode, DoctorOptions};

fn main() -> ExitCode {
    let command = match parse_arguments(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("sensor-doctor: {error}");
            print_usage();
            return ExitCode::from(64);
        }
    };
    if command.help {
        print_usage();
        return ExitCode::SUCCESS;
    }
    let report = run_doctor(&command.config, &command.repository_root, &command.options);
    if command.json {
        match serde_json::to_string_pretty(&report.to_json()) {
            Ok(document) => println!("{document}"),
            Err(error) => {
                eprintln!("sensor-doctor: cannot serialize report: {error}");
                return ExitCode::from(70);
            }
        }
    } else {
        print!("{}", report.render_text());
    }
    ExitCode::from(report.exit_code())
}

#[derive(Debug, Eq, PartialEq)]
struct DoctorCommand {
    config: PathBuf,
    repository_root: PathBuf,
    options: DoctorOptions,
    json: bool,
    help: bool,
}

fn parse_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<DoctorCommand, String> {
    let mut config = PathBuf::from("host/system_config.json");
    let mut repository_root = std::env::current_dir().map_err(|error| error.to_string())?;
    let mut options = DoctorOptions::default();
    let mut json = false;
    let mut help = false;
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index]
            .to_str()
            .ok_or_else(|| "arguments must be valid UTF-8".to_owned())?;
        match argument {
            "--json" => json = true,
            "--offline" if options.mode == DoctorMode::Online => {
                options.mode = DoctorMode::Offline;
            }
            "--deep" if options.mode == DoctorMode::Online => {
                options.mode = DoctorMode::Deep;
            }
            "--help" | "-h" => help = true,
            "--config" | "--repository-root" | "--status-file" | "--service"
            | "--status-max-age" => {
                let value = arguments
                    .get(index + 1)
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| format!("{argument} requires a UTF-8 value"))?;
                match argument {
                    "--config" => config = PathBuf::from(value),
                    "--repository-root" => repository_root = PathBuf::from(value),
                    "--status-file" => options.status_path = PathBuf::from(value),
                    "--service" => {
                        if value.is_empty() {
                            return Err("--service cannot be empty".to_owned());
                        }
                        options.service_name = value.to_owned();
                    }
                    "--status-max-age" => {
                        let seconds = value
                            .parse::<u64>()
                            .map_err(|_| "--status-max-age must be an integer".to_owned())?;
                        if seconds == 0 {
                            return Err("--status-max-age must be positive".to_owned());
                        }
                        options.status_max_age = Duration::from_secs(seconds);
                    }
                    _ => unreachable!(),
                }
                index += 1;
            }
            "--offline" | "--deep" => {
                return Err("--offline and --deep are mutually exclusive".to_owned());
            }
            _ => return Err(format!("unknown option: {argument}")),
        }
        index += 1;
    }
    Ok(DoctorCommand {
        config,
        repository_root,
        options,
        json,
        help,
    })
}

fn print_usage() {
    println!(
        "usage: sensor-doctor [--config <profile.json>] [--repository-root <path>] [--status-file <path>] [--service <unit>] [--status-max-age <seconds>] [--json] [--offline | --deep]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_deep_json_with_explicit_paths() {
        let command = parse_arguments(strings(&[
            "--config",
            "/etc/sensor.json",
            "--repository-root",
            "/opt/sensor",
            "--status-file",
            "/run/status.json",
            "--deep",
            "--json",
        ]))
        .unwrap();
        assert_eq!(command.config, PathBuf::from("/etc/sensor.json"));
        assert_eq!(command.repository_root, PathBuf::from("/opt/sensor"));
        assert_eq!(
            command.options.status_path,
            PathBuf::from("/run/status.json")
        );
        assert_eq!(command.options.mode, DoctorMode::Deep);
        assert!(command.json);
    }

    #[test]
    fn rejects_conflicting_modes_and_zero_age() {
        assert!(parse_arguments(strings(&["--offline", "--deep"])).is_err());
        assert!(parse_arguments(strings(&["--status-max-age", "0"])).is_err());
    }
}
