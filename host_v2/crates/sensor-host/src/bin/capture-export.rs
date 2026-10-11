use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use sensor_host::capture_export::export_capture_csv;

fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if arguments.len() != 2 {
        eprintln!("usage: capture-export <capture-directory> <output-directory>");
        return ExitCode::from(64);
    }
    match export_capture_csv(PathBuf::from(&arguments[0]), PathBuf::from(&arguments[1])) {
        Ok(summary) => {
            println!(
                "capture export complete: windows={} samples={} gaps={} continuity_breaks={} cursor_range=[{},{}) output={}",
                summary.window_count,
                summary.sample_count,
                summary.missing_sample_count,
                summary.continuity_break_count,
                summary.first_cursor,
                summary.end_exclusive,
                arguments[1]
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("capture-export: {error}");
            ExitCode::from(2)
        }
    }
}
