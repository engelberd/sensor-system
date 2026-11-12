use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sensor_host::capture_export::validate_sealed_capture_chain;

fn main() -> ExitCode {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.as_slice() == ["--help"] || arguments.as_slice() == ["-h"] {
        println!("usage: capture-validate <capture-root>");
        return ExitCode::SUCCESS;
    }
    if arguments.len() != 1 {
        eprintln!("usage: capture-validate <capture-root>");
        return ExitCode::from(64);
    }
    let root = PathBuf::from(&arguments[0]);
    match discover_streams(&root).and_then(validate_streams) {
        Ok(summary) => {
            println!(
                "capture tree valid: streams={} windows={} samples={} gaps={} continuity_breaks={}",
                summary.streams,
                summary.windows,
                summary.samples,
                summary.gaps,
                summary.continuity_breaks
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("capture-validate: {error}");
            ExitCode::from(2)
        }
    }
}

#[derive(Default)]
struct TreeSummary {
    streams: u64,
    windows: u64,
    samples: u64,
    gaps: u64,
    continuity_breaks: u64,
}

fn validate_streams(streams: BTreeSet<PathBuf>) -> Result<TreeSummary, String> {
    if streams.is_empty() {
        return Err("no sealed Capture streams found".to_owned());
    }
    let mut total = TreeSummary::default();
    for stream in streams {
        let summary = validate_sealed_capture_chain(&stream)
            .map_err(|error| format!("{}: {error}", stream.display()))?;
        println!(
            "capture stream valid: path={} windows={} samples={} gaps={} continuity_breaks={} cursor_range=[{},{})",
            stream.display(),
            summary.window_count,
            summary.sample_count,
            summary.missing_sample_count,
            summary.continuity_break_count,
            summary.first_cursor,
            summary.end_exclusive
        );
        total.streams = checked_add(total.streams, 1)?;
        total.windows = checked_add(total.windows, summary.window_count)?;
        total.samples = checked_add(total.samples, summary.sample_count)?;
        total.gaps = checked_add(total.gaps, summary.missing_sample_count)?;
        total.continuity_breaks =
            checked_add(total.continuity_breaks, summary.continuity_break_count)?;
    }
    Ok(total)
}

fn checked_add(left: u64, right: u64) -> Result<u64, String> {
    left.checked_add(right)
        .ok_or_else(|| "capture summary counter overflow".to_owned())
}

fn discover_streams(root: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let mut streams = BTreeSet::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let file_type = entry.file_type().map_err(|error| error.to_string())?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file()
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("window-") && name.ends_with(".manifest"))
            {
                streams.insert(directory.clone());
            }
        }
    }
    Ok(streams)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_an_empty_capture_tree() {
        let root =
            std::env::temp_dir().join(format!("capture-validate-empty-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        let streams = discover_streams(&root).unwrap();
        assert!(validate_streams(streams).is_err());
        fs::remove_dir(root).unwrap();
    }
}
