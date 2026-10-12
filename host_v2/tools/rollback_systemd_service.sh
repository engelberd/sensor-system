#!/usr/bin/env bash
set -euo pipefail

SERVICE_NAME="${SENSOR_HOST_SERVICE_NAME:-sensor-host-v2.service}"
INSTALL_ROOT="${SENSOR_HOST_INSTALL_ROOT:-/opt/sensor-system}"
CUSTOM_ROOT=0
[[ -n ${SENSOR_HOST_INSTALL_ROOT:-} ]] && CUSTOM_ROOT=1

if (($#)); then
  echo "rollback_systemd_service.sh accepts no arguments" >&2
  exit 64
fi

if ((EUID != 0 && CUSTOM_ROOT == 0)); then
  exec sudo "$0"
fi

CURRENT="$INSTALL_ROOT/current"
PREVIOUS="$INSTALL_ROOT/previous"
[[ -L $CURRENT ]] || { echo "current release symlink is missing: $CURRENT" >&2; exit 66; }
[[ -L $PREVIOUS ]] || { echo "previous release symlink is missing: $PREVIOUS" >&2; exit 66; }

current_target="$(readlink "$CURRENT")"
previous_target="$(readlink "$PREVIOUS")"
[[ -x $previous_target/sensor-host ]] || {
  echo "previous sensor-host is not executable: $previous_target/sensor-host" >&2
  exit 66
}
"$previous_target/sensor-host" --version >/dev/null

atomic_replace_symlink() {
  local source="$1"
  local destination="$2"
  if mv --help 2>/dev/null | grep -q -- ' -T'; then
    mv -Tf "$source" "$destination"
  else
    mv -fh "$source" "$destination"
  fi
}

# systemctl waits until the service's SIGTERM shutdown and WAL drain finish.
systemctl stop "$SERVICE_NAME"
ln -sfn "$previous_target" "$INSTALL_ROOT/current.new"
ln -sfn "$current_target" "$INSTALL_ROOT/previous.new"
atomic_replace_symlink "$INSTALL_ROOT/current.new" "$CURRENT"
atomic_replace_symlink "$INSTALL_ROOT/previous.new" "$PREVIOUS"
systemctl daemon-reload
systemctl start "$SERVICE_NAME"

echo "rolled back $SERVICE_NAME"
echo "current=$(readlink "$CURRENT")"
echo "previous=$(readlink "$PREVIOUS")"
