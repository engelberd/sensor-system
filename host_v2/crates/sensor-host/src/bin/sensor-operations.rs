use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sensor_host::operations_ipc::{request, OPERATIONS_IPC_SCHEMA_VERSION};
use serde_json::{json, Value};

fn main() -> ExitCode {
    match parse_arguments(std::env::args_os().skip(1)) {
        Ok(Command::Status { socket, channel }) => {
            run_status(&socket, channel).unwrap_or_else(report_error)
        }
        Ok(Command::Submit {
            socket,
            channel,
            node,
            operation,
            configuration,
            operation_id,
            timeout,
        }) => run_operation(
            &socket,
            channel,
            node,
            operation,
            configuration,
            operation_id,
            timeout,
        )
        .unwrap_or_else(report_error),
        Ok(Command::Help) => {
            print_usage();
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("sensor-operations: {error}");
            print_usage();
            ExitCode::from(64)
        }
    }
}

fn run_status(socket: &Path, channel: u8) -> Result<ExitCode, String> {
    let response = exchange(
        socket,
        &json!({
            "schema_version": OPERATIONS_IPC_SCHEMA_VERSION,
            "action": "status",
            "channel_id": channel
        }),
    )?;
    print_response(&response)?;
    Ok(if response_ok(&response)? {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

fn run_operation(
    socket: &Path,
    channel: u8,
    node: Option<u8>,
    operation: &'static str,
    configuration: Option<Value>,
    operation_id: u64,
    timeout: Duration,
) -> Result<ExitCode, String> {
    eprintln!("operation_id={operation_id}");
    let mut submit = json!({
        "schema_version": OPERATIONS_IPC_SCHEMA_VERSION,
        "action": "submit",
        "channel_id": channel,
        "operation_id": operation_id,
        "operation": operation
    });
    let object = submit
        .as_object_mut()
        .expect("operations request is a JSON object");
    if let Some(node) = node {
        object.insert("node_address".to_owned(), json!(node));
    }
    if let Some(configuration) = configuration {
        object.insert("configuration".to_owned(), configuration);
    }
    let mut submit_error = None;
    let mut submitted = None;
    for _ in 0..3 {
        match exchange(socket, &submit) {
            Ok(response) => {
                submitted = Some(response);
                break;
            }
            Err(error) => submit_error = Some(error),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let submitted = submitted.ok_or_else(|| {
        format!(
            "operation {operation_id} could not be submitted after bounded retries: {}",
            submit_error.unwrap_or_else(|| "unknown IPC failure".to_owned())
        )
    })?;
    if !response_ok(&submitted)? {
        print_response(&submitted)?;
        return Ok(ExitCode::from(2));
    }

    let deadline = Instant::now() + timeout;
    loop {
        let result = match exchange(
            socket,
            &json!({
                "schema_version": OPERATIONS_IPC_SCHEMA_VERSION,
                "action": "get",
                "channel_id": channel,
                "operation_id": operation_id
            }),
        ) {
            Ok(response) => response,
            Err(error) if Instant::now() < deadline => {
                eprintln!("operation {operation_id}: transient IPC read failed: {error}");
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(error) => {
                return Err(format!(
                    "operation {operation_id} result could not be read before timeout: {error}"
                ));
            }
        };
        if !response_ok(&result)? {
            print_response(&result)?;
            return Ok(ExitCode::from(2));
        }
        match result.get("phase").and_then(Value::as_str) {
            Some("succeeded") => {
                print_response(&result)?;
                return Ok(ExitCode::SUCCESS);
            }
            Some("rejected" | "failed") => {
                print_response(&result)?;
                return Ok(ExitCode::from(2));
            }
            Some("queued" | "accepted" | "running") => {}
            _ => return Err("operations response has an unknown phase".to_owned()),
        }
        if Instant::now() >= deadline {
            eprintln!(
                "sensor-operations: operation {operation_id} did not reach a terminal state before timeout"
            );
            print_response(&result)?;
            return Ok(ExitCode::from(75));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn exchange(socket: &Path, document: &Value) -> Result<Value, String> {
    request(socket, document, Duration::from_secs(2)).map_err(|error| error.to_string())
}

fn response_ok(response: &Value) -> Result<bool, String> {
    response
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or_else(|| "operations response is missing boolean ok".to_owned())
}

fn print_response(response: &Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(response).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn report_error(error: String) -> ExitCode {
    eprintln!("sensor-operations: {error}");
    ExitCode::from(69)
}

#[derive(Debug, Eq, PartialEq)]
enum Command {
    Status {
        socket: PathBuf,
        channel: u8,
    },
    Submit {
        socket: PathBuf,
        channel: u8,
        node: Option<u8>,
        operation: &'static str,
        configuration: Option<Value>,
        operation_id: u64,
        timeout: Duration,
    },
    Help,
}

fn parse_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<Command, &'static str> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    if arguments.as_slice() == [OsStr::new("--help")]
        || arguments.as_slice() == [OsStr::new("help")]
    {
        return Ok(Command::Help);
    }
    let action = arguments.first().and_then(|value| value.to_str()).ok_or(
        "expected status, maintenance, resume, get-configuration or validate-configuration",
    )?;
    let operation =
        match action {
            "status" => None,
            "maintenance" => Some("enter_maintenance"),
            "resume" => Some("resume_recording"),
            "get-configuration" => Some("get_configuration"),
            "validate-configuration" => Some("validate_configuration"),
            _ => return Err(
                "expected status, maintenance, resume, get-configuration or validate-configuration",
            ),
        };
    let needs_node = matches!(action, "get-configuration" | "validate-configuration");
    let needs_configuration = action == "validate-configuration";
    let mut socket = None;
    let mut channel = None;
    let mut node = None;
    let mut operation_id = None;
    let mut timeout_ms = 120_000_u64;
    let mut output_rate = None;
    let mut range_g = None;
    let mut high_pass_corner = None;
    let mut filter_profile = None;
    let mut fifo_watermark = None;
    let mut offset_x = None;
    let mut offset_y = None;
    let mut offset_z = None;
    let mut index = 1;
    while index < arguments.len() {
        let flag = arguments[index].to_str().ok_or("option must be UTF-8")?;
        let value = arguments.get(index + 1).ok_or("option requires a value")?;
        match flag {
            "--socket" if socket.is_none() => socket = Some(PathBuf::from(value)),
            "--channel" if channel.is_none() => {
                channel = Some(parse_u8(value, "invalid channel ID")?)
            }
            "--node" if node.is_none() && needs_node => {
                node = Some(parse_u8(value, "invalid node address")?)
            }
            "--operation-id" if operation_id.is_none() && operation.is_some() => {
                operation_id = Some(parse_u64(value, "invalid operation ID")?)
            }
            "--timeout-ms" if operation.is_some() => {
                timeout_ms = parse_u64(value, "invalid timeout")?
            }
            "--output-rate" if output_rate.is_none() && needs_configuration => {
                output_rate = Some(parse_output_rate(value)?)
            }
            "--range" if range_g.is_none() && needs_configuration => {
                range_g = Some(parse_u8(value, "invalid range")?)
            }
            "--high-pass-corner" if high_pass_corner.is_none() && needs_configuration => {
                high_pass_corner = Some(parse_u8(value, "invalid high-pass corner")?)
            }
            "--filter-profile" if filter_profile.is_none() && needs_configuration => {
                filter_profile = Some(parse_filter_profile(value)?)
            }
            "--fifo-watermark" if fifo_watermark.is_none() && needs_configuration => {
                fifo_watermark = Some(parse_u8(value, "invalid FIFO watermark")?)
            }
            "--offset-x" if offset_x.is_none() && needs_configuration => {
                offset_x = Some(parse_i16(value, "invalid X offset")?)
            }
            "--offset-y" if offset_y.is_none() && needs_configuration => {
                offset_y = Some(parse_i16(value, "invalid Y offset")?)
            }
            "--offset-z" if offset_z.is_none() && needs_configuration => {
                offset_z = Some(parse_i16(value, "invalid Z offset")?)
            }
            _ => return Err("unknown or repeated option"),
        }
        index += 2;
    }
    let socket = socket.ok_or("--socket is required")?;
    let channel = channel.ok_or("--channel is required")?;
    if channel == 0 || channel > 16 {
        return Err("channel ID must be in 1..16");
    }
    if needs_node && !matches!(node, Some(1..=253)) {
        return Err("node address must be in 1..253");
    }
    match operation {
        None => Ok(Command::Status { socket, channel }),
        Some(operation) => {
            if timeout_ms == 0 || timeout_ms > 86_400_000 {
                return Err("timeout must be in 1..86400000 ms");
            }
            Ok(Command::Submit {
                socket,
                channel,
                node,
                operation,
                configuration: if needs_configuration {
                    Some(json!({
                        "output_rate_code": output_rate.ok_or("--output-rate is required")?,
                        "range_g": range_g.ok_or("--range is required")?,
                        "high_pass_corner": high_pass_corner
                            .ok_or("--high-pass-corner is required")?,
                        "filter_profile": filter_profile
                            .ok_or("--filter-profile is required")?,
                        "fifo_watermark": fifo_watermark
                            .ok_or("--fifo-watermark is required")?,
                        "offset_x": offset_x.unwrap_or(0),
                        "offset_y": offset_y.unwrap_or(0),
                        "offset_z": offset_z.unwrap_or(0)
                    }))
                } else {
                    None
                },
                operation_id: operation_id.unwrap_or_else(fresh_operation_id),
                timeout: Duration::from_millis(timeout_ms),
            })
        }
    }
}

fn parse_u8(value: &OsString, error: &'static str) -> Result<u8, &'static str> {
    value
        .to_str()
        .and_then(|value| value.parse().ok())
        .ok_or(error)
}

fn parse_u64(value: &OsString, error: &'static str) -> Result<u64, &'static str> {
    value
        .to_str()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value != 0)
        .ok_or(error)
}

fn parse_i16(value: &OsString, error: &'static str) -> Result<i16, &'static str> {
    value
        .to_str()
        .and_then(|value| value.parse().ok())
        .ok_or(error)
}

fn parse_output_rate(value: &OsString) -> Result<u8, &'static str> {
    match value.to_str() {
        Some("4000") => Ok(0),
        Some("2000") => Ok(1),
        Some("1000") => Ok(2),
        Some("500") => Ok(3),
        Some("250") => Ok(4),
        Some("125") => Ok(5),
        Some("62.5") => Ok(6),
        Some("31.25") => Ok(7),
        Some("15.625") => Ok(8),
        Some("7.8125") => Ok(9),
        Some("3.90625") => Ok(10),
        _ => Err("unsupported output rate"),
    }
}

fn parse_filter_profile(value: &OsString) -> Result<u8, &'static str> {
    match value.to_str() {
        Some("light") => Ok(0),
        Some("balanced") => Ok(1),
        Some("aggressive") => Ok(2),
        _ => Err("filter profile must be light, balanced or aggressive"),
    }
}

fn fresh_operation_id() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let folded = (nanos as u64) ^ ((nanos >> 64) as u64) ^ u64::from(std::process::id());
    folded.max(1)
}

fn print_usage() {
    eprintln!(
        "usage:\n  sensor-operations status --channel ID --socket PATH\n  sensor-operations maintenance --channel ID --socket PATH [--operation-id ID] [--timeout-ms MS]\n  sensor-operations resume --channel ID --socket PATH [--operation-id ID] [--timeout-ms MS]\n  sensor-operations get-configuration --channel ID --node ID --socket PATH [--operation-id ID] [--timeout-ms MS]\n  sensor-operations validate-configuration --channel ID --node ID --socket PATH --output-rate HZ --range G --high-pass-corner CODE --filter-profile light|balanced|aggressive --fifo-watermark N [--offset-x N] [--offset-y N] [--offset-z N] [--operation-id ID] [--timeout-ms MS]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_status_and_explicit_idempotent_operation() {
        assert_eq!(
            parse_arguments(strings(&[
                "status",
                "--channel",
                "3",
                "--socket",
                "/run/op.sock"
            ])),
            Ok(Command::Status {
                socket: PathBuf::from("/run/op.sock"),
                channel: 3
            })
        );
        assert_eq!(
            parse_arguments(strings(&[
                "maintenance",
                "--socket",
                "/run/op.sock",
                "--channel",
                "2",
                "--operation-id",
                "99",
                "--timeout-ms",
                "5000"
            ])),
            Ok(Command::Submit {
                socket: PathBuf::from("/run/op.sock"),
                channel: 2,
                node: None,
                operation: "enter_maintenance",
                configuration: None,
                operation_id: 99,
                timeout: Duration::from_millis(5000)
            })
        );
    }

    #[test]
    fn rejects_unbounded_or_ambiguous_arguments() {
        assert!(parse_arguments(strings(&["status", "--channel", "0", "--socket", "/x"])).is_err());
        assert!(parse_arguments(strings(&[
            "resume",
            "--channel",
            "1",
            "--socket",
            "/x",
            "--timeout-ms",
            "0"
        ]))
        .is_err());
        assert!(parse_arguments(strings(&["maintenance", "--channel", "1"])).is_err());
    }

    #[test]
    fn parses_node_configuration_commands_without_hidden_defaults() {
        assert_eq!(
            parse_arguments(strings(&[
                "get-configuration",
                "--channel",
                "1",
                "--node",
                "7",
                "--socket",
                "/run/op.sock",
                "--operation-id",
                "101"
            ])),
            Ok(Command::Submit {
                socket: PathBuf::from("/run/op.sock"),
                channel: 1,
                node: Some(7),
                operation: "get_configuration",
                configuration: None,
                operation_id: 101,
                timeout: Duration::from_millis(120_000),
            })
        );

        let parsed = parse_arguments(strings(&[
            "validate-configuration",
            "--channel",
            "1",
            "--node",
            "7",
            "--socket",
            "/run/op.sock",
            "--output-rate",
            "62.5",
            "--range",
            "2",
            "--high-pass-corner",
            "0",
            "--filter-profile",
            "balanced",
            "--fifo-watermark",
            "30",
            "--offset-x",
            "-4",
            "--operation-id",
            "102",
        ]))
        .unwrap();
        let Command::Submit {
            node,
            operation,
            configuration,
            ..
        } = parsed
        else {
            panic!("expected submit command");
        };
        assert_eq!(node, Some(7));
        assert_eq!(operation, "validate_configuration");
        assert_eq!(configuration.unwrap()["output_rate_code"], 6);
    }
}
