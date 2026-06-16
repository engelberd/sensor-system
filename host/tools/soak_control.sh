#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HOST_PYTHON="$ROOT_DIR/host/.venv/bin/python"
SUPERVISOR_CMD="./hostctl supervisor --config host/system_config.json"
WATCH_CMD="$HOST_PYTHON host/tools/soak_watch.py"
DASHBOARD_CMD="$HOST_PYTHON host/host_dashboard.py --config host/system_config.json --host 0.0.0.0 --port 8080"
SUPERVISOR_SESSION="sensor-supervisor"
WATCH_SESSION="sensor-watch"
DASHBOARD_SESSION="sensor-dashboard"
STATUS_FILE="/tmp/sensor-system_supervisor_status.json"
EVENT_LOG="/tmp/sensor-system_supervisor_events.jsonl"
WATCH_LOG="/tmp/sensor-system_soak_watch.log"
ALERT_LOG="/tmp/sensor-system_soak_alerts.log"
DASHBOARD_URL="http://127.0.0.1:8080/"
STREAM_SESSION_PREFIX="sensor-stream-"
STREAM_DEFAULT_PORT=8000

usage() {
  cat <<'EOF'
Usage:
  host/tools/soak_control.sh start [--purge-data] [--keep-logs]
  host/tools/soak_control.sh stop
  host/tools/soak_control.sh restart [--purge-data]
  host/tools/soak_control.sh status
  host/tools/soak_control.sh alerts
  host/tools/soak_control.sh watch
  host/tools/soak_control.sh web
  host/tools/soak_control.sh stream --port /dev/ttyCH9344USB3 [--node 1] [--name usb3] [--live-port 8000]
  host/tools/soak_control.sh stream-stop [--name usb3]

Commands:
  start       Stop old processes, optionally purge data, clear runtime logs, start tmux sessions and web dashboard
  stop        Stop supervisor, recorder workers, soak watcher, and web dashboard sessions/processes
  restart     Equivalent to stop + start
  status      Show tmux sessions, matching processes, and recent supervisor events
  alerts      Tail soak alert log
  watch       Tail full soak watch log
  web         Show dashboard URL and live tmux session names
  stream      Start a standalone live stream recorder for one port with web preview
  stream-stop Stop a standalone live stream recorder started by 'stream'
EOF
}

require_tmux() {
  command -v tmux >/dev/null 2>&1 || {
    echo "[ERROR] tmux is required" >&2
    exit 1
  }
}

require_host_python() {
  [[ -x "$HOST_PYTHON" ]] || {
    echo "[ERROR] missing host virtualenv python: $HOST_PYTHON" >&2
    exit 1
  }
}

tmux_has_session() {
  local session="$1"
  tmux has-session -t "$session" 2>/dev/null
}

stop_all() {
  if tmux_has_session "$SUPERVISOR_SESSION"; then
    tmux kill-session -t "$SUPERVISOR_SESSION" || true
  fi
  if tmux_has_session "$WATCH_SESSION"; then
    tmux kill-session -t "$WATCH_SESSION" || true
  fi
  if tmux_has_session "$DASHBOARD_SESSION"; then
    tmux kill-session -t "$DASHBOARD_SESSION" || true
  fi

  pkill -f 'host/host_supervisor.py' || true
  pkill -f 'host/host_recorder.py' || true
  pkill -f 'host/tools/soak_watch.py' || true
  pkill -f 'host/host_dashboard.py' || true

  sleep 2
}

clear_runtime_logs() {
  mkdir -p /tmp/sensor-system_channels
  truncate -s 0 \
    "$EVENT_LOG" \
    "$WATCH_LOG" \
    "$ALERT_LOG" \
    /tmp/sensor-system_channels/*.events.jsonl \
    /tmp/sensor-system_channels/*.process.log 2>/dev/null || true
}

purge_data() {
  rm -rf /opt/sensor-system/data/*
}

stream_session_name() {
  local name="$1"
  echo "${STREAM_SESSION_PREFIX}${name}"
}

show_status() {
  echo "[tmux]"
  tmux list-sessions 2>/dev/null || true
  echo
  echo "[web]"
  echo "dashboard: $DASHBOARD_URL"
  echo
  echo "[processes]"
  ps -ef | grep -E 'host_supervisor|host_recorder|soak_watch.py|host_dashboard.py' | grep -v grep || true
  echo
  echo "[recent events]"
  tail -n 20 "$EVENT_LOG" 2>/dev/null || true
}

start_all() {
  local purge_data_flag="$1"
  local keep_logs_flag="$2"

  require_tmux
  require_host_python

  echo "[INFO] stopping old soak processes"
  stop_all

  if [[ "$purge_data_flag" == "1" ]]; then
    echo "[INFO] purging /data/sensor-system"
    purge_data
  fi

  if [[ "$keep_logs_flag" != "1" ]]; then
    echo "[INFO] clearing runtime logs"
    clear_runtime_logs
  fi

  echo "[INFO] starting supervisor in tmux session '$SUPERVISOR_SESSION'"
  tmux new-session -d -s "$SUPERVISOR_SESSION" "cd '$ROOT_DIR' && $SUPERVISOR_CMD"

  echo "[INFO] starting watcher in tmux session '$WATCH_SESSION'"
  tmux new-session -d -s "$WATCH_SESSION" "cd '$ROOT_DIR' && $WATCH_CMD"

  echo "[INFO] starting dashboard in tmux session '$DASHBOARD_SESSION'"
  tmux new-session -d -s "$DASHBOARD_SESSION" "cd '$ROOT_DIR' && $DASHBOARD_CMD"

  sleep 3
  show_status
  echo
  echo "[OK] use 'tmux attach -t $SUPERVISOR_SESSION', 'tmux attach -t $WATCH_SESSION', or 'tmux attach -t $DASHBOARD_SESSION' to inspect"
  echo "[OK] open web dashboard: $DASHBOARD_URL"
  echo "[OK] tail alerts with: tail -f $ALERT_LOG"
}

start_stream() {
  local port=""
  local node="1"
  local name=""
  local live_port="$STREAM_DEFAULT_PORT"

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --port) port="${2:-}"; shift ;;
      --node) node="${2:-}"; shift ;;
      --name) name="${2:-}"; shift ;;
      --live-port) live_port="${2:-}"; shift ;;
      *)
        echo "[ERROR] unknown option for stream: $1" >&2
        usage
        exit 2
        ;;
    esac
    shift
  done

  [[ -n "$port" ]] || {
    echo "[ERROR] stream requires --port" >&2
    exit 2
  }

  if [[ -z "$name" ]]; then
    name="$(basename "$port")"
  fi

  require_tmux
  require_host_python
  mkdir -p "$ROOT_DIR/data"

  local session
  session="$(stream_session_name "$name")"
  local output_path="$ROOT_DIR/data/${name}_live.h5"
  local status_path="/tmp/${name}_live.status.json"
  local event_path="/tmp/${name}_live.events.jsonl"
  local cmd="./hostctl recorder --channel-name '$name' --port '$port' --nodes '$node' --output '$output_path' --overwrite --format hdf5 --duration 0 --temperature-interval 0 --status-file '$status_path' --event-log '$event_path' --live --live-host 0.0.0.0 --live-port '$live_port'"

  if tmux_has_session "$session"; then
    tmux kill-session -t "$session" || true
    sleep 1
  fi

  echo "[INFO] starting live stream recorder '$name' on $port"
  tmux new-session -d -s "$session" "cd '$ROOT_DIR' && $cmd"
  sleep 2
  echo "[OK] stream session: $session"
  echo "[OK] stream web: http://127.0.0.1:${live_port}/"
  echo "[OK] output file: $output_path"
  echo "[OK] event log: $event_path"
}

stop_stream() {
  local name=""

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --name) name="${2:-}"; shift ;;
      *)
        echo "[ERROR] unknown option for stream-stop: $1" >&2
        usage
        exit 2
        ;;
    esac
    shift
  done

  [[ -n "$name" ]] || {
    echo "[ERROR] stream-stop requires --name" >&2
    exit 2
  }

  local session
  session="$(stream_session_name "$name")"
  if tmux_has_session "$session"; then
    tmux kill-session -t "$session" || true
    echo "[OK] stopped stream session: $session"
  else
    echo "[INFO] no stream session found: $session"
  fi
}

main() {
  local command="${1:-}"
  shift || true

  case "$command" in
    start)
      local purge_data_flag=0
      local keep_logs_flag=0
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --purge-data) purge_data_flag=1 ;;
          --keep-logs) keep_logs_flag=1 ;;
          *)
            echo "[ERROR] unknown option for start: $1" >&2
            usage
            exit 2
            ;;
        esac
        shift
      done
      start_all "$purge_data_flag" "$keep_logs_flag"
      ;;
    stop)
      stop_all
      show_status
      ;;
    restart)
      local purge_data_flag=0
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --purge-data) purge_data_flag=1 ;;
          *)
            echo "[ERROR] unknown option for restart: $1" >&2
            usage
            exit 2
            ;;
        esac
        shift
      done
      start_all "$purge_data_flag" "0"
      ;;
    status)
      require_tmux
      show_status
      ;;
    alerts)
      tail -f "$ALERT_LOG"
      ;;
    watch)
      tail -f "$WATCH_LOG"
      ;;
    web)
      echo "[web] dashboard: $DASHBOARD_URL"
      echo "[web] tmux session: $DASHBOARD_SESSION"
      echo "[web] local open: $DASHBOARD_URL"
      ;;
    stream)
      start_stream "$@"
      ;;
    stream-stop)
      stop_stream "$@"
      ;;
    help|-h|--help|"")
      usage
      ;;
    *)
      echo "[ERROR] unknown command: $command" >&2
      usage
      exit 2
      ;;
  esac
}

main "$@"
