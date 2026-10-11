#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  run_pi_host_gate.sh CONFIG DURATION_SECONDS [OUTPUT_DIRECTORY]

Runs one Raspberry Pi hardware gate: build, configuration validation, read-only
discovery, a bounded production recording, controlled SIGTERM and evidence
collection. The CONFIG must contain real hardware identities and stable Linux
device paths. DURATION_SECONDS must be a positive integer.

Set SENSOR_HOST_SKIP_BUILD=1 when a matrix wrapper has already built the binary.
EOF
}

if [[ ${1:-} == "--help" || ${1:-} == "-h" ]]; then
  usage
  exit 0
fi
if (( $# < 2 || $# > 3 )); then
  usage >&2
  exit 64
fi

config=$1
duration_seconds=$2
output_directory=${3:-}

if [[ ! $duration_seconds =~ ^[1-9][0-9]*$ ]]; then
  printf 'DURATION_SECONDS must be a positive integer\n' >&2
  exit 64
fi
if [[ ! -r $config ]]; then
  printf 'CONFIG is not readable: %s\n' "$config" >&2
  exit 66
fi
if [[ ! -r /proc/sys/kernel/random/boot_id ]]; then
  printf 'This gate requires Linux with /proc/sys/kernel/random/boot_id\n' >&2
  exit 69
fi
for command_name in cargo timeout grep tee; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    printf 'Required command is missing: %s\n' "$command_name" >&2
    exit 69
  fi
done

script_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_directory/.." && pwd)
repository_root=$(cd -- "$workspace/.." && pwd)
config_directory=$(cd -- "$(dirname -- "$config")" && pwd)
config="$config_directory/$(basename -- "$config")"

if [[ -z $output_directory ]]; then
  timestamp=$(date -u +%Y%m%dT%H%M%SZ)
  output_directory="$repository_root/var/tmp/pi-host-gate-$timestamp"
elif [[ $output_directory != /* ]]; then
  output_directory="$PWD/$output_directory"
fi
mkdir -p -- "$output_directory"

binary="$workspace/target/release/sensor-host"
capture_validator="$workspace/target/release/capture-validate"
if [[ ${SENSOR_HOST_SKIP_BUILD:-0} != 1 ]]; then
  cargo build --release -p sensor-host --manifest-path "$workspace/Cargo.toml" \
    2>&1 | tee "$output_directory/build.log"
fi
if [[ ! -x $binary ]]; then
  printf 'Production binary is missing after build: %s\n' "$binary" >&2
  exit 70
fi
if [[ ! -x $capture_validator ]]; then
  printf 'Capture validator is missing after build: %s\n' "$capture_validator" >&2
  exit 70
fi

printf 'config=%s\nduration_seconds=%s\nrepository_root=%s\n' \
  "$config" "$duration_seconds" "$repository_root" \
  > "$output_directory/invocation.txt"
uname -a > "$output_directory/uname.txt"
cat /proc/sys/kernel/random/boot_id > "$output_directory/boot-id.txt"
"$binary" --version > "$output_directory/version.txt"

"$binary" validate --config "$config" --repository-root "$repository_root" \
  2>&1 | tee "$output_directory/validate.log"
if [[ -n ${SENSOR_HOST_EXPECTED_CHANNELS:-} ]] && \
   ! grep -q " active_channels=${SENSOR_HOST_EXPECTED_CHANNELS} " \
     "$output_directory/validate.log"; then
  printf 'Gate failed: profile does not contain %s active channels\n' \
    "$SENSOR_HOST_EXPECTED_CHANNELS" >&2
  exit 1
fi
"$binary" discover --config "$config" --repository-root "$repository_root" \
  2>&1 | tee "$output_directory/discover.log"

vmstat_pid=
cleanup() {
  if [[ -n $vmstat_pid ]]; then
    kill "$vmstat_pid" >/dev/null 2>&1 || true
    wait "$vmstat_pid" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT INT TERM

if command -v vmstat >/dev/null 2>&1; then
  vmstat 1 > "$output_directory/vmstat.log" &
  vmstat_pid=$!
fi

run_log="$output_directory/run.log"
status_file="$output_directory/runtime-status.json"
if [[ -x /usr/bin/time ]]; then
  /usr/bin/time -v -o "$output_directory/resource-usage.txt" \
    timeout --preserve-status --signal=TERM --kill-after=120s \
    "${duration_seconds}s" "$binary" run \
    --config "$config" --repository-root "$repository_root" \
    --status-file "$status_file" \
    > "$run_log" 2>&1
else
  timeout --preserve-status --signal=TERM --kill-after=120s \
    "${duration_seconds}s" "$binary" run \
    --config "$config" --repository-root "$repository_root" \
    --status-file "$status_file" \
    > "$run_log" 2>&1
fi
cleanup
vmstat_pid=
cat "$run_log"

if ! grep -q '^host shutdown: clean=true ' "$run_log"; then
  printf 'Gate failed: controlled shutdown was not clean\n' >&2
  exit 1
fi
if ! grep -q '^channel metrics: ' "$run_log"; then
  printf 'Gate failed: no channel metrics were recorded\n' >&2
  exit 1
fi

grep '^channel metrics: ' "$run_log" > "$output_directory/channel-metrics.txt"
grep '^channel shutdown: ' "$run_log" > "$output_directory/channel-shutdown.txt"

capture_validation="$output_directory/capture-validation.log"
: > "$capture_validation"
validated_streams=0
while IFS= read -r discovery_line; do
  capture_stream=${discovery_line##* capture_stream=}
  "$capture_validator" "$capture_stream" 2>&1 | tee -a "$capture_validation"
  validated_streams=$((validated_streams + 1))
done < <(grep '^node: .* capture_stream=' "$output_directory/discover.log")
if (( validated_streams == 0 )); then
  printf 'Gate failed: discovery did not report any Capture streams\n' >&2
  exit 1
fi

printf 'PASS: Raspberry Pi host gate completed. Artifacts: %s\n' "$output_directory"
