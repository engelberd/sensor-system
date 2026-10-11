#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  run_manual_rs485_hil.sh DEVICE NODE_ADDRESS HARDWARE_ID BOARD_REVISION [SAMPLE_TARGET] [CHUNK_LIMIT]

Example:
  ./host_v2/tools/run_manual_rs485_hil.sh \
    /dev/cu.usbserial-0001 1 0011223344556677 2 2500 48

Communication uses DEVICE at 115200 baud. USB connected directly to the node
may provide power, but must not be passed as DEVICE.
EOF
}

if [[ ${1:-} == "--help" || ${1:-} == "-h" ]]; then
  usage
  exit 0
fi

if (( $# < 4 || $# > 6 )); then
  usage >&2
  exit 64
fi

device=$1
node_address=$2
hardware_id=$3
board_revision=$4
sample_target=${5:-2500}
chunk_limit=${6:-48}
baud_rate=115200

if [[ ! -e $device ]]; then
  printf 'RS485 device does not exist: %s\n' "$device" >&2
  exit 66
fi
if [[ ! $node_address =~ ^[0-9]+$ ]] || (( node_address < 1 || node_address > 254 )); then
  printf 'NODE_ADDRESS must be in range 1..254\n' >&2
  exit 64
fi
if [[ ! $hardware_id =~ ^[[:xdigit:]]{16}$ ]]; then
  printf 'HARDWARE_ID must contain exactly 16 hexadecimal characters\n' >&2
  exit 64
fi
if [[ ! $board_revision =~ ^[0-9]+$ ]] || (( board_revision < 1 )); then
  printf 'BOARD_REVISION must be a positive integer\n' >&2
  exit 64
fi
if [[ ! $sample_target =~ ^[0-9]+$ ]] || (( sample_target < 1 )); then
  printf 'SAMPLE_TARGET must be a positive integer\n' >&2
  exit 64
fi
if [[ ! $chunk_limit =~ ^[0-9]+$ ]] || (( chunk_limit < 1 || chunk_limit > 48 )); then
  printf 'CHUNK_LIMIT must be in range 1..48\n' >&2
  exit 64
fi

script_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repository_root=$(cd -- "$script_directory/../.." && pwd)
binary="$repository_root/host_v2/target/release/sensor-hil"
export_binary="$repository_root/host_v2/target/release/capture-export"
timestamp=$(date -u +%Y%m%dT%H%M%SZ)
result_directory="$repository_root/var/tmp/manual-rs485-hil-$timestamp"
capture_output="$result_directory/capture"
transcript="$result_directory/transcript.log"
window_duration_us=${SENSOR_SYSTEM_HIL_WINDOW_DURATION_US:-2000000}
record_limit_seconds=${SENSOR_SYSTEM_HIL_RECORD_LIMIT_SECONDS:-300}
minimum_windows=${SENSOR_SYSTEM_HIL_MIN_WINDOWS:-2}
export_analysis=${SENSOR_SYSTEM_HIL_EXPORT_ANALYSIS:-0}

if [[ $export_analysis != 0 && $export_analysis != 1 ]]; then
  printf 'SENSOR_SYSTEM_HIL_EXPORT_ANALYSIS must be 0 or 1\n' >&2
  exit 64
fi

mkdir -p "$result_directory"

safe_stop() {
  local exit_status=$?
  if [[ -x $binary ]]; then
    "$binary" safe-stop \
      "$device" "$node_address" "$hardware_id" "$board_revision" "$baud_rate" \
      >>"$transcript" 2>&1 || true
  fi
  if (( exit_status != 0 )); then
    printf 'Test failed. Transcript: %s\n' "$transcript" >&2
  fi
}
trap safe_stop EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

cd "$repository_root/host_v2"
printf 'Building sensor-hil release binary...\n' | tee "$transcript"
cargo build --release -p sensor-host --bin sensor-hil 2>&1 | tee -a "$transcript"

printf '\n[1/3] Read-only discovery over RS485\n' | tee -a "$transcript"
"$binary" identify "$device" "$node_address" "$baud_rate" 2>&1 | tee -a "$transcript"

printf '\n[2/3] Fail-closed handshake\n' | tee -a "$transcript"
"$binary" handshake \
  "$device" "$node_address" "$hardware_id" "$board_revision" "$baud_rate" \
  2>&1 | tee -a "$transcript"

printf '\n[3/3] Start -> production Capture WAL -> release -> Stop\n' | tee -a "$transcript"
SENSOR_SYSTEM_HIL_RECORD_LIMIT_SECONDS="$record_limit_seconds" \
  "$binary" capture-cycle \
  "$device" "$node_address" "$hardware_id" "$board_revision" \
  "$capture_output" "$window_duration_us" \
  "$sample_target" "$chunk_limit" "$baud_rate" \
  2>&1 | tee -a "$transcript"

manifest_count=$(find "$capture_output" -type f -name '*.manifest' | wc -l | tr -d ' ')
if (( manifest_count < minimum_windows )); then
  printf 'Expected at least %s sealed Capture windows, found %s\n' \
    "$minimum_windows" "$manifest_count" \
    | tee -a "$transcript" >&2
  exit 1
fi
if ! grep -q 'record-cycle persisted continuity break: .*reason=ProcessingReset' "$transcript"; then
  printf 'Expected the final ProcessingReset marker after Stop\n' \
    | tee -a "$transcript" >&2
  exit 1
fi
if ! grep -Eq 'gaps=0 continuity_breaks=1 .*decode_errors=0 transaction_timeouts=0 state=Maintenance' "$transcript"; then
  printf 'Stable-link criteria failed; inspect the transcript and Capture files\n' \
    | tee -a "$transcript" >&2
  exit 1
fi

if [[ $export_analysis == 1 ]]; then
  printf '\nExporting validated Capture for analysis\n' | tee -a "$transcript"
  cargo build --release -p sensor-host --bin capture-export 2>&1 | tee -a "$transcript"
  "$export_binary" "$capture_output" "$result_directory/analysis" \
    2>&1 | tee -a "$transcript"
fi

printf '\nPASS\nArtifacts: %s\n' "$result_directory" | tee -a "$transcript"
