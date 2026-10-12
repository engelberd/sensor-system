#!/usr/bin/env bash
set -euo pipefail

SERVICE_NAME="${SENSOR_HOST_SERVICE_NAME:-sensor-host-v2.service}"
SYSTEMCTL="${SENSOR_HOST_SYSTEMCTL:-systemctl}"
JOURNALCTL="${SENSOR_HOST_JOURNALCTL:-journalctl}"
ACTION="${1:-}"
shift || true

case "$ACTION" in
  start|stop|restart)
    exec "$SYSTEMCTL" "$ACTION" "$SERVICE_NAME" "$@"
    ;;
  status)
    exec "$SYSTEMCTL" status "$SERVICE_NAME" --no-pager "$@"
    ;;
  logs)
    exec "$JOURNALCTL" --unit "$SERVICE_NAME" --no-pager "$@"
    ;;
  *)
    echo "expected start, stop, restart, status or logs" >&2
    exit 64
    ;;
esac
