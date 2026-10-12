#!/usr/bin/env bash
set -euo pipefail

SERVICE_NAME="sensor-host-v2.service"
INSTALL_ROOT="${SENSOR_HOST_INSTALL_ROOT:-/opt/sensor-system}"
UNIT_ROOT="${SENSOR_HOST_UNIT_ROOT:-/etc/systemd/system}"
CUSTOM_ROOTS=0
if [[ -n ${SENSOR_HOST_INSTALL_ROOT:-} || -n ${SENSOR_HOST_UNIT_ROOT:-} ]]; then
  CUSTOM_ROOTS=1
fi
DRY_RUN=0
REPOSITORY_ROOT=""
CONFIG_PATH=""
BINARY=""
SERVICE_USER=""
SERVICE_GROUP=""

usage() {
  cat <<'EOF'
Usage: install_systemd_service.sh [--dry-run] --repository-root PATH
       --config PATH --binary PATH --user USER --group GROUP

Installs one checksummed sensor-host binary and a generated systemd unit.
The previous artifact remains available through /opt/sensor-system/previous.
EOF
}

while (($#)); do
  case "$1" in
    --dry-run) DRY_RUN=1; shift ;;
    --repository-root) REPOSITORY_ROOT="${2:?missing repository root}"; shift 2 ;;
    --config) CONFIG_PATH="${2:?missing config path}"; shift 2 ;;
    --binary) BINARY="${2:?missing binary path}"; shift 2 ;;
    --user) SERVICE_USER="${2:?missing service user}"; shift 2 ;;
    --group) SERVICE_GROUP="${2:?missing service group}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 64 ;;
  esac
done

for value in REPOSITORY_ROOT CONFIG_PATH BINARY SERVICE_USER SERVICE_GROUP; do
  if [[ -z ${!value} ]]; then
    echo "missing required value: $value" >&2
    exit 64
  fi
done

REPOSITORY_ROOT="$(cd "$REPOSITORY_ROOT" && pwd -P)"
CONFIG_PATH="$(cd "$(dirname "$CONFIG_PATH")" && pwd -P)/$(basename "$CONFIG_PATH")"
BINARY="$(cd "$(dirname "$BINARY")" && pwd -P)/$(basename "$BINARY")"
RUNTIME_DATA_ROOT="$REPOSITORY_ROOT/var"
TEMPLATE="$REPOSITORY_ROOT/host_v2/systemd/sensor-host-v2.service.in"

[[ -f $CONFIG_PATH ]] || { echo "configuration does not exist: $CONFIG_PATH" >&2; exit 66; }
[[ -x $BINARY ]] || { echo "binary is not executable: $BINARY" >&2; exit 66; }
[[ -f $TEMPLATE ]] || { echo "unit template does not exist: $TEMPLATE" >&2; exit 66; }

case "$SERVICE_USER:$SERVICE_GROUP" in
  *[!A-Za-z0-9_.:-]*) echo "unsafe service user or group" >&2; exit 65 ;;
esac
for path in "$REPOSITORY_ROOT" "$CONFIG_PATH" "$RUNTIME_DATA_ROOT"; do
  case "$path" in
    *[[:space:]@]*) echo "unsupported character in installation path: $path" >&2; exit 65 ;;
  esac
done

checksum() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

SHA256="$(checksum "$BINARY")"
VERSION="$($BINARY --version | tr -cs 'A-Za-z0-9._-' '-')"
RELEASE_ID="${VERSION%-}-${SHA256:0:16}"
RELEASE_DIR="$INSTALL_ROOT/releases/$RELEASE_ID"
CURRENT_BINARY="$INSTALL_ROOT/current/sensor-host"

render_unit() {
  sed \
    -e "s|@REPOSITORY_ROOT@|$REPOSITORY_ROOT|g" \
    -e "s|@RUNTIME_DATA_ROOT@|$RUNTIME_DATA_ROOT|g" \
    -e "s|@CONFIG_PATH@|$CONFIG_PATH|g" \
    -e "s|@CURRENT_BINARY@|$CURRENT_BINARY|g" \
    -e "s|@SERVICE_USER@|$SERVICE_USER|g" \
    -e "s|@SERVICE_GROUP@|$SERVICE_GROUP|g" \
    "$TEMPLATE"
}

if ((DRY_RUN)); then
  echo "release_id=$RELEASE_ID"
  echo "sha256=$SHA256"
  echo "unit=$UNIT_ROOT/$SERVICE_NAME"
  render_unit
  exit 0
fi

if ((EUID != 0 && CUSTOM_ROOTS == 0)); then
  exec sudo "$0" \
    --repository-root "$REPOSITORY_ROOT" --config "$CONFIG_PATH" \
    --binary "$BINARY" --user "$SERVICE_USER" --group "$SERVICE_GROUP"
fi

atomic_replace_symlink() {
  local source="$1"
  local destination="$2"
  if mv --help 2>/dev/null | grep -q -- ' -T'; then
    mv -Tf "$source" "$destination"
  else
    mv -fh "$source" "$destination"
  fi
}

install -d -m 0755 "$INSTALL_ROOT/releases" "$RELEASE_DIR" "$UNIT_ROOT"
install -m 0755 "$BINARY" "$RELEASE_DIR/sensor-host"
ACTUAL_SHA256="$(checksum "$RELEASE_DIR/sensor-host")"
[[ $ACTUAL_SHA256 == "$SHA256" ]] || { echo "installed binary checksum mismatch" >&2; exit 74; }

if [[ -L $INSTALL_ROOT/current ]]; then
  old_target="$(readlink "$INSTALL_ROOT/current")"
  ln -sfn "$old_target" "$INSTALL_ROOT/previous.new"
  atomic_replace_symlink "$INSTALL_ROOT/previous.new" "$INSTALL_ROOT/previous"
fi
ln -sfn "$RELEASE_DIR" "$INSTALL_ROOT/current.new"
atomic_replace_symlink "$INSTALL_ROOT/current.new" "$INSTALL_ROOT/current"

unit_tmp="$(mktemp "$UNIT_ROOT/.${SERVICE_NAME}.XXXXXX")"
trap 'rm -f "$unit_tmp"' EXIT
render_unit >"$unit_tmp"
chmod 0644 "$unit_tmp"
mv -f "$unit_tmp" "$UNIT_ROOT/$SERVICE_NAME"
trap - EXIT

systemctl daemon-reload
systemctl enable "$SERVICE_NAME"
echo "installed $SERVICE_NAME release=$RELEASE_ID sha256=$SHA256"
echo "Start it explicitly with: ./hostctl start"
