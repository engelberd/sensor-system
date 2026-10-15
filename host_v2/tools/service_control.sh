#!/usr/bin/env bash
set -euo pipefail

SERVICE_NAME="${SENSOR_HOST_SERVICE_NAME:-sensor-host-v2.service}"
ANALYSIS_TIMER_NAME="${SENSOR_ANALYSIS_TIMER_NAME:-sensor-analysis-export-v2.timer}"
SYSTEMCTL="${SENSOR_HOST_SYSTEMCTL:-systemctl}"
JOURNALCTL="${SENSOR_HOST_JOURNALCTL:-journalctl}"
ACTION="${1:-}"
shift || true

case "$ACTION" in
  start|restart)
    exec "$SYSTEMCTL" "$ACTION" "$SERVICE_NAME" "$ANALYSIS_TIMER_NAME" "$@"
    ;;
  stop)
    exec "$SYSTEMCTL" "$ACTION" "$ANALYSIS_TIMER_NAME" "$SERVICE_NAME" "$@"
    ;;
  status)
    exec "$SYSTEMCTL" status "$SERVICE_NAME" "$ANALYSIS_TIMER_NAME" --no-pager "$@"
    ;;
  logs)
    exec "$JOURNALCTL" --unit "$SERVICE_NAME" --unit sensor-analysis-export-v2.service --no-pager "$@"
    ;;
  *)
    echo "expected start, stop, restart, status or logs" >&2
    exit 64
    ;;
esac
