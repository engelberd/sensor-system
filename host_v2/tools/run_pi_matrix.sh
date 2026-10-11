#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  run_pi_matrix.sh DURATION_SECONDS CONFIG_1 CONFIG_4 CONFIG_8 [OUTPUT_DIRECTORY]

Runs the production Raspberry Pi gate sequentially for profiles containing
exactly 1, 4 and 8 active channels. It builds sensor-host once and preserves a
separate evidence directory for each stage.
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

duration_seconds=$1
config_1=$2
config_4=$3
config_8=$4
output_directory=${5:-}

script_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_directory/.." && pwd)
repository_root=$(cd -- "$workspace/.." && pwd)
if [[ -z $output_directory ]]; then
  timestamp=$(date -u +%Y%m%dT%H%M%SZ)
  output_directory="$repository_root/var/tmp/pi-host-matrix-$timestamp"
elif [[ $output_directory != /* ]]; then
  output_directory="$PWD/$output_directory"
fi
mkdir -p -- "$output_directory"

cargo build --release -p sensor-host --manifest-path "$workspace/Cargo.toml" \
  2>&1 | tee "$output_directory/build.log"

for stage in 1 4 8; do
  case $stage in
    1) config=$config_1 ;;
    4) config=$config_4 ;;
    8) config=$config_8 ;;
  esac
  SENSOR_HOST_SKIP_BUILD=1 SENSOR_HOST_EXPECTED_CHANNELS=$stage \
    "$script_directory/run_pi_host_gate.sh" "$config" "$duration_seconds" \
    "$output_directory/${stage}-channels"
done

printf 'PASS: 1/4/8 Raspberry Pi matrix completed. Artifacts: %s\n' "$output_directory"
