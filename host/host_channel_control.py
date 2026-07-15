#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path
from typing import Any

ROOT_DIR = Path(__file__).resolve().parent.parent
if str(ROOT_DIR) not in sys.path:
    sys.path.insert(0, str(ROOT_DIR))

from host.host_configurator import (
    CMD_GET_CONFIG,
    CMD_RESTART,
    HostConfig,
    ProtocolClient,
    load_json_config,
    send_and_wait,
)

import serial


DEFAULT_SYSTEM_CONFIG_PATH = "host/system_config.json"


def _resolve_system_config(path: str) -> Path:
    config_path = Path(path)
    if not config_path.is_absolute() and not config_path.exists():
        config_path = Path(__file__).resolve().parent.parent / path
    return config_path


def _load_system_config(path: Path) -> dict[str, Any]:
    data = load_json_config(path)
    if not isinstance(data, dict):
        raise RuntimeError(f"system config '{path}' is invalid")
    return data


def _find_channel(data: dict[str, Any], channel_name: str) -> dict[str, Any]:
    channels = data.get("channels")
    if not isinstance(channels, list):
        raise RuntimeError("system config does not define channels")
    for channel in channels:
        if isinstance(channel, dict) and str(channel.get("name")) == channel_name:
            return channel
    raise RuntimeError(f"channel '{channel_name}' not found in system config")


def _runtime_dir(data: dict[str, Any], system_config_path: Path) -> Path:
    supervisor = data.get("supervisor")
    if not isinstance(supervisor, dict):
        raise RuntimeError("system config does not define supervisor section")
    runtime_dir = supervisor.get("channel_runtime_dir")
    if not runtime_dir:
        raise RuntimeError("system config does not define supervisor.channel_runtime_dir")
    path = Path(str(runtime_dir))
    if not path.is_absolute():
        path = system_config_path.resolve().parent.parent / path
    return path


def _supervisor_status_path(data: dict[str, Any], system_config_path: Path) -> Path:
    supervisor = data.get("supervisor")
    if not isinstance(supervisor, dict):
        raise RuntimeError("system config does not define supervisor section")
    status_file = supervisor.get("status_file")
    if not status_file:
        raise RuntimeError("system config does not define supervisor.status_file")
    path = Path(str(status_file))
    if not path.is_absolute():
        path = system_config_path.resolve().parent.parent / path
    return path


def _write_channel_command(runtime_dir: Path, channel_name: str, action: str) -> Path:
    runtime_dir.mkdir(parents=True, exist_ok=True)
    command_file = runtime_dir / f"{channel_name}.command.json"
    payload = {
        "action": action,
        "channel_name": channel_name,
        "requested_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    tmp_path = command_file.with_suffix(command_file.suffix + ".tmp")
    tmp_path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(tmp_path, command_file)
    return command_file


def _read_supervisor_channel_status(status_path: Path, channel_name: str) -> dict[str, Any] | None:
    if not status_path.exists():
        return None
    try:
        payload = json.loads(status_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    channels = payload.get("channels")
    if not isinstance(channels, list):
        return None
    for channel in channels:
        if isinstance(channel, dict) and str(channel.get("name")) == channel_name:
            return channel
    return None


def _wait_for_channel_running(status_path: Path,
                              channel_name: str,
                              expected_running: bool,
                              timeout_s: float) -> bool:
    deadline = time.monotonic() + max(timeout_s, 0.1)
    while time.monotonic() < deadline:
        state = _read_supervisor_channel_status(status_path, channel_name)
        if state is not None and bool(state.get("running", False)) == expected_running:
            return True
        time.sleep(0.2)
    return False


def _restart_node(port: str, baud: int, node_id: int, timeout: float) -> None:
    ser = serial.Serial(
        port=port,
        baudrate=baud,
        timeout=0.05,
        write_timeout=0.5,
    )
    try:
        client = ProtocolClient(ser)
        try:
            send_and_wait(client, node_id, bytes([CMD_RESTART]), timeout)
        except (RuntimeError, serial.SerialException) as exc:
            text = str(exc).lower()
            if "no response" not in text and "returned no data" not in text and "disconnected" not in text:
                raise
    finally:
        ser.close()


def _wait_for_node_ready(port: str,
                         baud: int,
                         node_id: int,
                         request_timeout_s: float,
                         ready_timeout_s: float,
                         poll_interval_s: float) -> float:
    started_at = time.monotonic()
    deadline = started_at + max(ready_timeout_s, request_timeout_s)
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        ser = None
        try:
            ser = serial.Serial(
                port=port,
                baudrate=baud,
                timeout=0.05,
                write_timeout=0.5,
            )
            client = ProtocolClient(ser)
            send_and_wait(client, node_id, bytes([CMD_GET_CONFIG]), request_timeout_s)
            return time.monotonic() - started_at
        except (RuntimeError, serial.SerialException) as exc:
            last_error = exc
        finally:
            if ser is not None:
                ser.close()
        remaining = deadline - time.monotonic()
        if remaining > 0:
            time.sleep(min(max(poll_interval_s, 0.05), remaining))
    raise RuntimeError(
        f"node={node_id} did not become ready within {ready_timeout_s:.1f}s: {last_error}"
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Channel-level control for sensor-system host runtime")
    parser.add_argument("--system-config", default=DEFAULT_SYSTEM_CONFIG_PATH)
    sub = parser.add_subparsers(dest="command", required=True)

    restart_remote = sub.add_parser(
        "restart-remote",
        help="Restart the remote node over RS485 and request recorder worker restart for the channel",
    )
    restart_remote.add_argument("--channel", required=True)
    restart_remote.add_argument("--node", type=int, default=1)
    restart_remote.add_argument("--timeout", type=float, default=2.0)
    restart_remote.add_argument(
        "--settle-ms",
        type=int,
        default=6000,
        help="Minimum quiet time before readiness probes; covers the bootloader maintenance window",
    )
    restart_remote.add_argument("--ready-timeout", type=float, default=20.0)
    restart_remote.add_argument("--ready-poll-ms", type=int, default=250)
    restart_remote.add_argument("--worker-timeout", type=float, default=10.0)
    restart_remote.add_argument("--skip-node", action="store_true")
    restart_remote.add_argument("--skip-recorder", action="store_true")

    return parser


def main() -> int:
    args = build_parser().parse_args()
    system_config_path = _resolve_system_config(args.system_config)
    data = _load_system_config(system_config_path)

    if args.command == "restart-remote":
        channel = _find_channel(data, args.channel)
        port = str(channel.get("port", ""))
        baud = int(channel.get("baud", 115200))
        if not port:
            raise RuntimeError(f"channel '{args.channel}' does not define port")

        status_path = _supervisor_status_path(data, system_config_path)
        runtime_dir = _runtime_dir(data, system_config_path)

        operation_error: Exception | None = None
        recorder_restore_required = False
        try:
            if not args.skip_recorder:
                recorder_restore_required = True
                stop_command = _write_channel_command(runtime_dir, args.channel, "stop")
                print(f"[OK] recorder stop requested for channel={args.channel} via {stop_command}")
                if not _wait_for_channel_running(status_path, args.channel, False, args.worker_timeout):
                    raise RuntimeError(
                        f"channel '{args.channel}' did not stop within {args.worker_timeout:.1f}s"
                    )
                print(f"[OK] recorder worker for channel={args.channel} is stopped")

            if not args.skip_node:
                try:
                    _restart_node(port, baud, args.node, args.timeout)
                except serial.SerialException as exc:
                    raise RuntimeError(f"could not restart node={args.node}: {exc}") from exc
                print(
                    f"[OK] restart command sent to node={args.node} on channel={args.channel} "
                    f"port={port} baud={baud}"
                )
                time.sleep(max(args.settle_ms, 0) / 1000.0)
                ready_after_s = _wait_for_node_ready(
                    port,
                    baud,
                    args.node,
                    min(args.timeout, 1.0),
                    args.ready_timeout,
                    max(args.ready_poll_ms, 50) / 1000.0,
                )
                print(
                    f"[OK] node={args.node} application protocol ready "
                    f"after probe_wait={ready_after_s:.2f}s "
                    f"total_boot_wait={max(args.settle_ms, 0) / 1000.0 + ready_after_s:.2f}s"
                )
        except (RuntimeError, OSError) as exc:
            operation_error = exc
        finally:
            if recorder_restore_required:
                try:
                    command_file = _write_channel_command(runtime_dir, args.channel, "start")
                    print(f"[OK] recorder restart requested for channel={args.channel} via {command_file}")
                    if not _wait_for_channel_running(status_path, args.channel, True, args.worker_timeout):
                        raise RuntimeError(
                            f"channel '{args.channel}' did not start within {args.worker_timeout:.1f}s"
                        )
                    print(f"[OK] recorder worker for channel={args.channel} is running again")
                except (RuntimeError, OSError) as exc:
                    if operation_error is None:
                        operation_error = exc
                    else:
                        operation_error = RuntimeError(
                            f"{operation_error}; additionally recorder recovery failed: {exc}"
                        )

        if operation_error is not None:
            raise RuntimeError(str(operation_error)) from operation_error

        return 0

    raise RuntimeError(f"unsupported command '{args.command}'")


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RuntimeError as exc:
        print(f"[ERROR] {exc}", file=sys.stderr)
        raise SystemExit(2)
