#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  run_manual_1h_capture.sh DEVICE NODE_ADDRESS HARDWARE_ID BOARD_REVISION [OUTPUT_RATE_HZ]

OUTPUT_RATE_HZ must describe the current node configuration: 250, 125 or 62.5.
The script does not change node configuration.

Example:
  ./host_v2/tools/run_manual_1h_capture.sh \
    /dev/cu.usbserial-B003C07K 1 2f13349450fa4084 2 125
EOF
}

if [[ ${1:-} == "--help" || ${1:-} == "-h" ]]; then
  usage
  exit 0
fi

if (( $# < 4 || $# > 5 )); then
  usage >&2
  exit 64
fi

output_rate=${5:-125}

case "$output_rate" in
  250) sample_target=900000 ;;
  125) sample_target=450000 ;;
  62.5) sample_target=225000 ;;
  *)
    printf 'OUTPUT_RATE_HZ must be 250, 125 or 62.5\n' >&2
    exit 64
    ;;
esac

script_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)

SENSOR_SYSTEM_HIL_WINDOW_DURATION_US=3600000000 \
SENSOR_SYSTEM_HIL_RECORD_LIMIT_SECONDS=4200 \
SENSOR_SYSTEM_HIL_MIN_WINDOWS=1 \
SENSOR_SYSTEM_HIL_EXPORT_ANALYSIS=1 \
  "$script_directory/run_manual_rs485_hil.sh" \
  "$1" "$2" "$3" "$4" "$sample_target" 48