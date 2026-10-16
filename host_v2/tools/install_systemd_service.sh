#!/usr/bin/env bash
set -euo pipefail

SERVICE_NAME="sensor-host-v2.service"
ANALYSIS_SERVICE_NAME="sensor-analysis-export-v2.service"
ANALYSIS_TIMER_NAME="sensor-analysis-export-v2.timer"
SERVICE_CONTROL_NAME="sensor-service-control-v2.service"
DASHBOARD_NAME="sensor-host-dashboard-v2.service"
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
ANALYSIS_BINARY=""
HOST_PYTHON=""
SERVICE_USER=""
SERVICE_GROUP=""
DASHBOARD_HOST="127.0.0.1"
DASHBOARD_PORT="8090"

usage() {
  cat <<'EOF'
Usage: install_systemd_service.sh [--dry-run] --repository-root PATH
       --config PATH --binary PATH --analysis-binary PATH --python PATH --user USER --group GROUP
       [--dashboard-host ADDRESS] [--dashboard-port PORT]

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
    --analysis-binary) ANALYSIS_BINARY="${2:?missing analysis binary path}"; shift 2 ;;
    --python) HOST_PYTHON="${2:?missing Python interpreter}"; shift 2 ;;
    --user) SERVICE_USER="${2:?missing service user}"; shift 2 ;;
    --group) SERVICE_GROUP="${2:?missing service group}"; shift 2 ;;
    --dashboard-host) DASHBOARD_HOST="${2:?missing dashboard host}"; shift 2 ;;
    --dashboard-port) DASHBOARD_PORT="${2:?missing dashboard port}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 64 ;;
  esac
done

for value in REPOSITORY_ROOT CONFIG_PATH BINARY ANALYSIS_BINARY HOST_PYTHON SERVICE_USER SERVICE_GROUP; do
  if [[ -z ${!value} ]]; then
    echo "missing required value: $value" >&2
    exit 64
  fi
done

REPOSITORY_ROOT="$(cd "$REPOSITORY_ROOT" && pwd -P)"
CONFIG_PATH="$(cd "$(dirname "$CONFIG_PATH")" && pwd -P)/$(basename "$CONFIG_PATH")"
BINARY="$(cd "$(dirname "$BINARY")" && pwd -P)/$(basename "$BINARY")"
ANALYSIS_BINARY="$(cd "$(dirname "$ANALYSIS_BINARY")" && pwd -P)/$(basename "$ANALYSIS_BINARY")"
HOST_PYTHON="$(cd "$(dirname "$HOST_PYTHON")" && pwd -P)/$(basename "$HOST_PYTHON")"
RUNTIME_DATA_ROOT="$REPOSITORY_ROOT/var"
SYSTEM_PROFILES_ROOT="$REPOSITORY_ROOT/host/configs/systems"
TEMPLATE="$REPOSITORY_ROOT/host_v2/systemd/sensor-host-v2.service.in"
ANALYSIS_TEMPLATE="$REPOSITORY_ROOT/host_v2/systemd/sensor-analysis-export-v2.service.in"
ANALYSIS_TIMER_TEMPLATE="$REPOSITORY_ROOT/host_v2/systemd/sensor-analysis-export-v2.timer.in"
SERVICE_CONTROL_TEMPLATE="$REPOSITORY_ROOT/host_v2/systemd/sensor-service-control-v2.service.in"
DASHBOARD_TEMPLATE="$REPOSITORY_ROOT/host_v2/systemd/sensor-host-dashboard-v2.service.in"
SERVICE_CONTROL_SCRIPT="$REPOSITORY_ROOT/host/service_control_v2.py"
SERVICE_CONTROL_MODULE="$REPOSITORY_ROOT/host/dashboard/v2_service_control.py"

[[ -f $CONFIG_PATH ]] || { echo "configuration does not exist: $CONFIG_PATH" >&2; exit 66; }
[[ -x $BINARY ]] || { echo "binary is not executable: $BINARY" >&2; exit 66; }
[[ -x $ANALYSIS_BINARY ]] || { echo "binary is not executable: $ANALYSIS_BINARY" >&2; exit 66; }
[[ -x $HOST_PYTHON ]] || { echo "Python interpreter is not executable: $HOST_PYTHON" >&2; exit 66; }
[[ -f $TEMPLATE ]] || { echo "unit template does not exist: $TEMPLATE" >&2; exit 66; }
[[ -f $ANALYSIS_TEMPLATE ]] || { echo "unit template does not exist: $ANALYSIS_TEMPLATE" >&2; exit 66; }
[[ -f $ANALYSIS_TIMER_TEMPLATE ]] || { echo "unit template does not exist: $ANALYSIS_TIMER_TEMPLATE" >&2; exit 66; }
[[ -f $SERVICE_CONTROL_TEMPLATE ]] || { echo "unit template does not exist: $SERVICE_CONTROL_TEMPLATE" >&2; exit 66; }
[[ -f $DASHBOARD_TEMPLATE ]] || { echo "unit template does not exist: $DASHBOARD_TEMPLATE" >&2; exit 66; }
[[ -f $SERVICE_CONTROL_SCRIPT ]] || { echo "service control script does not exist: $SERVICE_CONTROL_SCRIPT" >&2; exit 66; }
[[ -f $SERVICE_CONTROL_MODULE ]] || { echo "service control module does not exist: $SERVICE_CONTROL_MODULE" >&2; exit 66; }

case "$SERVICE_USER:$SERVICE_GROUP" in
  *[!A-Za-z0-9_.:-]*) echo "unsafe service user or group" >&2; exit 65 ;;
esac
[[ $DASHBOARD_HOST =~ ^[A-Za-z0-9.:-]+$ ]] || { echo "unsafe dashboard host" >&2; exit 65; }
[[ $DASHBOARD_PORT =~ ^[1-9][0-9]{0,4}$ ]] && ((DASHBOARD_PORT <= 65535)) || { echo "invalid dashboard port" >&2; exit 65; }
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
ANALYSIS_SHA256="$(checksum "$ANALYSIS_BINARY")"
SERVICE_CONTROL_SCRIPT_SHA256="$(checksum "$SERVICE_CONTROL_SCRIPT")"
SERVICE_CONTROL_MODULE_SHA256="$(checksum "$SERVICE_CONTROL_MODULE")"
VERSION="$($BINARY --version | tr -cs 'A-Za-z0-9._-' '-')"
RELEASE_ID="${VERSION%-}-${SHA256:0:12}-${ANALYSIS_SHA256:0:12}-${SERVICE_CONTROL_SCRIPT_SHA256:0:6}${SERVICE_CONTROL_MODULE_SHA256:0:6}"
RELEASE_DIR="$INSTALL_ROOT/releases/$RELEASE_ID"
CURRENT_BINARY="$INSTALL_ROOT/current/sensor-host"
CURRENT_ANALYSIS_BINARY="$INSTALL_ROOT/current/analysis-export"
CURRENT_SERVICE_CONTROL="$INSTALL_ROOT/current/service-control-v2.py"
SCAN_INTERVAL_SECONDS="$($ANALYSIS_BINARY --print-interval --config "$CONFIG_PATH" --repository-root "$REPOSITORY_ROOT")"
[[ $SCAN_INTERVAL_SECONDS =~ ^[1-9][0-9]*$ ]] || { echo "invalid analysis scan interval" >&2; exit 78; }

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

render_analysis_unit() {
  sed \
    -e "s|@REPOSITORY_ROOT@|$REPOSITORY_ROOT|g" \
    -e "s|@RUNTIME_DATA_ROOT@|$RUNTIME_DATA_ROOT|g" \
    -e "s|@CONFIG_PATH@|$CONFIG_PATH|g" \
    -e "s|@CURRENT_ANALYSIS_BINARY@|$CURRENT_ANALYSIS_BINARY|g" \
    -e "s|@SERVICE_USER@|$SERVICE_USER|g" \
    -e "s|@SERVICE_GROUP@|$SERVICE_GROUP|g" \
    "$ANALYSIS_TEMPLATE"
}

render_analysis_timer() {
  sed -e "s|@SCAN_INTERVAL_SECONDS@|$SCAN_INTERVAL_SECONDS|g" "$ANALYSIS_TIMER_TEMPLATE"
}

render_service_control_unit() {
  sed \
    -e "s|@REPOSITORY_ROOT@|$REPOSITORY_ROOT|g" \
    -e "s|@INSTALL_ROOT@|$INSTALL_ROOT|g" \
    -e "s|@HOST_PYTHON@|$HOST_PYTHON|g" \
    -e "s|@CURRENT_SERVICE_CONTROL@|$CURRENT_SERVICE_CONTROL|g" \
    -e "s|@SERVICE_GROUP@|$SERVICE_GROUP|g" \
    "$SERVICE_CONTROL_TEMPLATE"
}

render_dashboard_unit() {
  sed \
    -e "s|@REPOSITORY_ROOT@|$REPOSITORY_ROOT|g" \
    -e "s|@CONFIG_PATH@|$CONFIG_PATH|g" \
    -e "s|@SYSTEM_PROFILES_ROOT@|$SYSTEM_PROFILES_ROOT|g" \
    -e "s|@DASHBOARD_PYTHON@|$REPOSITORY_ROOT/host/.venv/bin/python|g" \
    -e "s|@DASHBOARD_HOST@|$DASHBOARD_HOST|g" \
    -e "s|@DASHBOARD_PORT@|$DASHBOARD_PORT|g" \
    -e "s|@SERVICE_USER@|$SERVICE_USER|g" \
    -e "s|@SERVICE_GROUP@|$SERVICE_GROUP|g" \
    "$DASHBOARD_TEMPLATE"
}

if ((DRY_RUN)); then
  echo "release_id=$RELEASE_ID"
  echo "sha256=$SHA256"
  echo "unit=$UNIT_ROOT/$SERVICE_NAME"
  render_unit
  echo "analysis_sha256=$ANALYSIS_SHA256"
  echo "unit=$UNIT_ROOT/$ANALYSIS_SERVICE_NAME"
  render_analysis_unit
  echo "unit=$UNIT_ROOT/$ANALYSIS_TIMER_NAME"
  render_analysis_timer
  echo "unit=$UNIT_ROOT/$SERVICE_CONTROL_NAME"
  render_service_control_unit
  echo "unit=$UNIT_ROOT/$DASHBOARD_NAME"
  render_dashboard_unit
  exit 0
fi

if ((EUID != 0 && CUSTOM_ROOTS == 0)); then
  exec sudo "$0" \
    --repository-root "$REPOSITORY_ROOT" --config "$CONFIG_PATH" \
    --binary "$BINARY" --analysis-binary "$ANALYSIS_BINARY" \
    --python "$HOST_PYTHON" \
    --user "$SERVICE_USER" --group "$SERVICE_GROUP" \
    --dashboard-host "$DASHBOARD_HOST" --dashboard-port "$DASHBOARD_PORT"
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
install -m 0755 "$ANALYSIS_BINARY" "$RELEASE_DIR/analysis-export"
install -m 0755 "$SERVICE_CONTROL_SCRIPT" "$RELEASE_DIR/service-control-v2.py"
install -m 0644 "$SERVICE_CONTROL_MODULE" "$RELEASE_DIR/v2_service_control.py"
ACTUAL_SHA256="$(checksum "$RELEASE_DIR/sensor-host")"
[[ $ACTUAL_SHA256 == "$SHA256" ]] || { echo "installed binary checksum mismatch" >&2; exit 74; }
[[ $(checksum "$RELEASE_DIR/analysis-export") == "$ANALYSIS_SHA256" ]] || { echo "installed analysis binary checksum mismatch" >&2; exit 74; }
[[ $(checksum "$RELEASE_DIR/service-control-v2.py") == "$SERVICE_CONTROL_SCRIPT_SHA256" ]] || { echo "installed service control script checksum mismatch" >&2; exit 74; }
[[ $(checksum "$RELEASE_DIR/v2_service_control.py") == "$SERVICE_CONTROL_MODULE_SHA256" ]] || { echo "installed service control module checksum mismatch" >&2; exit 74; }

if [[ -L $INSTALL_ROOT/current ]]; then
  old_target="$(readlink "$INSTALL_ROOT/current")"
  ln -sfn "$old_target" "$INSTALL_ROOT/previous.new"
  atomic_replace_symlink "$INSTALL_ROOT/previous.new" "$INSTALL_ROOT/previous"
fi
ln -sfn "$RELEASE_DIR" "$INSTALL_ROOT/current.new"
atomic_replace_symlink "$INSTALL_ROOT/current.new" "$INSTALL_ROOT/current"

unit_tmp="$(mktemp "$UNIT_ROOT/.${SERVICE_NAME}.XXXXXX")"
analysis_unit_tmp="$(mktemp "$UNIT_ROOT/.${ANALYSIS_SERVICE_NAME}.XXXXXX")"
analysis_timer_tmp="$(mktemp "$UNIT_ROOT/.${ANALYSIS_TIMER_NAME}.XXXXXX")"
service_control_tmp="$(mktemp "$UNIT_ROOT/.${SERVICE_CONTROL_NAME}.XXXXXX")"
dashboard_tmp="$(mktemp "$UNIT_ROOT/.${DASHBOARD_NAME}.XXXXXX")"
trap 'rm -f "$unit_tmp" "$analysis_unit_tmp" "$analysis_timer_tmp" "$service_control_tmp" "$dashboard_tmp"' EXIT
render_unit >"$unit_tmp"
render_analysis_unit >"$analysis_unit_tmp"
render_analysis_timer >"$analysis_timer_tmp"
render_service_control_unit >"$service_control_tmp"
render_dashboard_unit >"$dashboard_tmp"
chmod 0644 "$unit_tmp" "$analysis_unit_tmp" "$analysis_timer_tmp" "$service_control_tmp" "$dashboard_tmp"
mv -f "$unit_tmp" "$UNIT_ROOT/$SERVICE_NAME"
mv -f "$analysis_unit_tmp" "$UNIT_ROOT/$ANALYSIS_SERVICE_NAME"
mv -f "$analysis_timer_tmp" "$UNIT_ROOT/$ANALYSIS_TIMER_NAME"
mv -f "$service_control_tmp" "$UNIT_ROOT/$SERVICE_CONTROL_NAME"
mv -f "$dashboard_tmp" "$UNIT_ROOT/$DASHBOARD_NAME"
trap - EXIT

systemctl daemon-reload
systemctl enable "$SERVICE_NAME"
systemctl enable "$ANALYSIS_TIMER_NAME"
systemctl enable --now "$SERVICE_CONTROL_NAME"
systemctl enable --now "$DASHBOARD_NAME"
echo "installed $SERVICE_NAME release=$RELEASE_ID sha256=$SHA256"
echo "Start it explicitly with: ./hostctl start"
