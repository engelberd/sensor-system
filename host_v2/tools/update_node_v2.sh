#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HIL_BINARY="${SENSOR_HOST_HIL_BINARY:-$ROOT_DIR/host_v2/target/release/sensor-hil}"
PYTHON="${SENSOR_HOST_BOOTLOADER_PYTHON:-$ROOT_DIR/host/.venv/bin/python}"
PACKAGE_PYTHON="${SENSOR_HOST_PACKAGE_PYTHON:-$PYTHON}"
UPLOADER="$ROOT_DIR/host/tools/bootloader_cli.py"
PACKAGE_VALIDATOR="$ROOT_DIR/host_v2/python/tools/validate_update_package.py"
IMAGE="$ROOT_DIR/node/build/sensor-system-node-update-package.json"
PORT=""
NODE=""
HARDWARE_ID=""
BOARD_REVISION=""
EXPECTED_FIRMWARE=""
BAUD=115200
RESUME=0

usage() {
  cat <<'EOF'
Usage: ./hostctl update --port DEVICE --node ADDRESS --hardware-id HEX
       --board-revision 1|2 --expected-firmware MAJOR.MINOR.PATCH
       [--baud 115200] [--image PACKAGE.json] [--resume-bootloader]

The normal path performs protocol-5 PrepareFirmwareUpdate, uploads the A/B
image through the bootloader, then confirms the exact trial version over
protocol 5. Use --resume-bootloader only when an interrupted upload already
left the same target in bootloader recovery.
EOF
}

while (($#)); do
  case "$1" in
    --port) PORT="${2:?missing port}"; shift 2 ;;
    --node) NODE="${2:?missing node address}"; shift 2 ;;
    --hardware-id) HARDWARE_ID="${2:?missing hardware id}"; shift 2 ;;
    --board-revision) BOARD_REVISION="${2:?missing board revision}"; shift 2 ;;
    --expected-firmware) EXPECTED_FIRMWARE="${2:?missing expected firmware}"; shift 2 ;;
    --baud) BAUD="${2:?missing baud rate}"; shift 2 ;;
    --image) IMAGE="${2:?missing image path}"; shift 2 ;;
    --resume-bootloader) RESUME=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "[ERROR] unknown update option: $1" >&2; usage >&2; exit 64 ;;
  esac
done

for value in PORT NODE HARDWARE_ID BOARD_REVISION EXPECTED_FIRMWARE; do
  [[ -n ${!value} ]] || { echo "[ERROR] missing required option: $value" >&2; usage >&2; exit 64; }
done
[[ $NODE =~ ^[1-9][0-9]*$ ]] && ((NODE <= 253)) || { echo "[ERROR] node must be 1..253" >&2; exit 64; }
[[ $HARDWARE_ID =~ ^[0-9A-Fa-f]{16}$ ]] || { echo "[ERROR] hardware-id must contain 16 hexadecimal digits" >&2; exit 64; }
[[ $BOARD_REVISION == 1 || $BOARD_REVISION == 2 ]] || { echo "[ERROR] board-revision must be 1 or 2" >&2; exit 64; }
[[ $BAUD == 115200 ]] || { echo "[ERROR] production RS485 baud rate must be 115200" >&2; exit 64; }
[[ $EXPECTED_FIRMWARE =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "[ERROR] expected-firmware must be MAJOR.MINOR.PATCH" >&2; exit 64; }
[[ -x $HIL_BINARY ]] || { echo "[ERROR] missing sensor-hil: $HIL_BINARY; run ./hostctl build" >&2; exit 69; }
[[ -x $PYTHON ]] || { echo "[ERROR] missing bootloader Python runtime: $PYTHON" >&2; exit 69; }
[[ -x $PACKAGE_PYTHON ]] || { echo "[ERROR] missing package validator Python: $PACKAGE_PYTHON" >&2; exit 69; }
[[ -f $UPLOADER ]] || { echo "[ERROR] missing bootloader uploader: $UPLOADER" >&2; exit 69; }
[[ -f $IMAGE ]] || { echo "[ERROR] update package does not exist: $IMAGE" >&2; exit 66; }
PACKAGE_IDENTITY="$("$PACKAGE_PYTHON" "$PACKAGE_VALIDATOR" "$IMAGE")"
read -r PACKAGE_FIRMWARE PACKAGE_BOARD_REVISION <<<"$PACKAGE_IDENTITY"
[[ $PACKAGE_FIRMWARE == "$EXPECTED_FIRMWARE" ]] || { echo "[ERROR] package firmware $PACKAGE_FIRMWARE does not match expected $EXPECTED_FIRMWARE" >&2; exit 65; }
[[ $PACKAGE_BOARD_REVISION == "$BOARD_REVISION" ]] || { echo "[ERROR] package board revision $PACKAGE_BOARD_REVISION does not match target $BOARD_REVISION" >&2; exit 65; }

SYSTEMCTL="${SENSOR_HOST_SYSTEMCTL:-systemctl}"
SERVICE_NAME="${SENSOR_HOST_SERVICE_NAME:-sensor-host-v2.service}"
if command -v "$SYSTEMCTL" >/dev/null 2>&1 && "$SYSTEMCTL" is-active --quiet "$SERVICE_NAME" 2>/dev/null; then
  echo "[ERROR] $SERVICE_NAME is active; stop it before updating a node" >&2
  exit 75
fi

if ((RESUME == 0)); then
  echo "[1/3] Protocol-5 prepare update"
  "$HIL_BINARY" prepare-update "$PORT" "$NODE" "$HARDWARE_ID" "$BOARD_REVISION" "$BAUD"
else
  echo "[1/3] Resuming an explicitly confirmed bootloader recovery session"
fi

echo "[2/3] A/B bootloader upload"
"$PYTHON" "$UPLOADER" --port "$PORT" --baud "$BAUD" --node "$NODE" --enter none --timeout "${SENSOR_SYSTEM_UPDATE_TIMEOUT:-10}" --hello-retries "${SENSOR_SYSTEM_UPDATE_HELLO_RETRIES:-20}" --hello-retry-delay "${SENSOR_SYSTEM_UPDATE_HELLO_RETRY_DELAY:-0.5}" --image "$IMAGE" --board-revision "$BOARD_REVISION" --no-verify-app --force-busy upload

echo "[3/3] Exact-version trial confirmation"
"$HIL_BINARY" confirm-trial "$PORT" "$NODE" "$HARDWARE_ID" "$BOARD_REVISION" "$EXPECTED_FIRMWARE" "$BAUD"
echo "update complete: node=$NODE firmware=$EXPECTED_FIRMWARE"
