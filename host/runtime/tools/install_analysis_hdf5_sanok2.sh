#!/usr/bin/env bash
# Replace only the offline exporter; recorder bytes and process are preserved.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
[[ $(hostname) == rpi-sanok2 ]] || { echo 'This rollout is restricted to rpi-sanok2' >&2; exit 78; }
BINARY="${1:-$ROOT/host/runtime/target/release/analysis-export}"
PYTHON="$ROOT/host/.venv/bin/python"
CONFIG="$ROOT/host/system_config.json"
"$PYTHON" -c 'import h5py, numpy'
"$BINARY" --print-interval --config "$CONFIG" --repository-root "$ROOT"
SHA="$(sha256sum "$BINARY" | cut -d' ' -f1)"
PREVIOUS="$(readlink -f /opt/sensor-system/current)"
RELEASE="/opt/sensor-system/releases/$(basename "$PREVIOUS" | sed 's/-hdf5-.*//')-hdf5-${SHA:0:12}"
[[ $PREVIOUS == /opt/sensor-system/releases/* ]] || exit 78
sudo systemctl stop sensor-analysis-export-v2.timer sensor-analysis-export-v2.service
sudo mkdir -p "$RELEASE"
if [[ $PREVIOUS != "$RELEASE" ]]; then
  sudo cp -a "$PREVIOUS/." "$RELEASE/"
fi
sudo install -m 0755 "$BINARY" "$RELEASE/analysis-export"
[[ $(sha256sum "$RELEASE/analysis-export" | cut -d' ' -f1) == "$SHA" ]]
cmp "$PREVIOUS/sensor-host" "$RELEASE/sensor-host"
sudo ln -sfn "$RELEASE" /opt/sensor-system/.current-hdf5
sudo mv -Tf /opt/sensor-system/.current-hdf5 /opt/sensor-system/current
sudo mkdir -p /etc/systemd/system/sensor-analysis-export-v2.service.d
printf '[Unit]\nDescription=Sensor System sealed Capture to analysis HDF5 exporter\n\n[Service]\nEnvironment="SENSOR_ANALYSIS_PYTHON=%s"\n' "$PYTHON" |
  sudo tee /etc/systemd/system/sensor-analysis-export-v2.service.d/hdf5.conf >/dev/null
sudo systemctl daemon-reload
sudo systemctl start sensor-analysis-export-v2.timer
sudo systemctl start sensor-analysis-export-v2.service
printf 'Installed analysis HDF5 sha256=%s\nPrevious release: %s\nCurrent release: %s\n' "$SHA" "$PREVIOUS" "$RELEASE"
"$ROOT/hostctl" doctor
