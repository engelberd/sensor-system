#!/usr/bin/env python3
from __future__ import annotations

import argparse
import fcntl
import json
import os
import shutil
import signal
import subprocess
import sys
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, TextIO


PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from host.common.runtime_status import (  # noqa: E402
    JsonStatusWriter,
    JsonlEventWriter,
    RuntimeStatusNode,
    SupervisorChannelStatus,
    SupervisorStatusSnapshot,
)
from host.common.system_config import ChannelConfig, HostSystemConfig  # noqa: E402
from host.common.version import PROJECT_VERSION  # noqa: E402


SUPERVISOR_VERSION = PROJECT_VERSION


class StopFlag:
    def __init__(self) -> None:
        self.stop_requested = False
        self.signal_number: int | None = None
        self.signal_name: str | None = None

    def request_stop(self, signum: int, *_args: object) -> None:
        self.stop_requested = True
        self.signal_number = signum
        try:
            self.signal_name = signal.Signals(signum).name
        except ValueError:
            self.signal_name = str(signum)


@dataclass
class WorkerState:
    config: ChannelConfig
    status_file: Path
    event_log: Path
    process_log: Path
    command_file: Path
    process: subprocess.Popen[str] | None = None
    process_log_handle: TextIO | None = None
    restart_count: int = 0
    consecutive_failures: int = 0
    last_exit_code: int | None = None
    next_start_monotonic: float = 0.0
    process_started_monotonic: float | None = None
    last_status: dict[str, Any] | None = None
    last_status_updated_utc: str | None = None
    desired_running: bool = True
    failure_latched_reason: str | None = None

    @property
    def running(self) -> bool:
        return self.process is not None and self.process.poll() is None

    @property
    def process_id(self) -> int | None:
        return self.process.pid if self.running and self.process is not None else None

    def close_process_log(self) -> None:
        if self.process_log_handle is not None:
            self.process_log_handle.close()
            self.process_log_handle = None


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Sensor System multi-channel recorder supervisor")
    parser.add_argument("--config", default="host/system_config.json")
    return parser.parse_args()


def load_json(path: Path) -> dict[str, Any] | None:
    if not path.exists():
        return None
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return None


def channel_status_path(runtime_dir: Path, channel_name: str) -> Path:
    return runtime_dir / f"{channel_name}.status.json"


def channel_event_path(log_dir: Path, channel_name: str) -> Path:
    return log_dir / f"{channel_name}.events.jsonl"


def channel_process_log_path(log_dir: Path, channel_name: str) -> Path:
    return log_dir / f"{channel_name}.process.log"


def channel_command_path(runtime_dir: Path, channel_name: str) -> Path:
    return runtime_dir / f"{channel_name}.command.json"


def supervisor_command_path(runtime_dir: Path) -> Path:
    return runtime_dir / "supervisor.command.json"


def load_channel_command(path: Path) -> dict[str, Any] | None:
    if not path.exists():
        return None
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        path.unlink(missing_ok=True)
        return None
    if not isinstance(payload, dict):
        path.unlink(missing_ok=True)
        return None
    path.unlink(missing_ok=True)
    return payload


def _truncate_file(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("", encoding="utf-8")


def _remove_channel_entries_from_jsonl(path: Path, channel_name: str) -> None:
    if not path.exists():
        return
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError:
        return
    kept: list[str] = []
    for line in lines:
        if not line.strip():
            continue
        try:
            payload = json.loads(line)
        except json.JSONDecodeError:
            kept.append(line)
            continue
        if isinstance(payload, dict) and payload.get("channel_name") == channel_name:
            continue
        kept.append(line)
    text = "\n".join(kept)
    if text:
        text += "\n"
    path.write_text(text, encoding="utf-8")


def channel_output_dir(system_config: HostSystemConfig, channel: ChannelConfig) -> str:
    if channel.output_dir:
        return channel.output_dir
    return str(Path(system_config.storage.root_dir) / channel.name)


def build_worker_command(
    python_executable: str,
    recorder_script: Path,
    channel: ChannelConfig,
    system_config: HostSystemConfig,
    status_file: Path,
    event_log: Path,
) -> list[str]:
    command = [
        python_executable,
        str(recorder_script),
        "--channel-name",
        channel.name,
        "--channel-id",
        str(channel.channel_id or 1),
        "--port",
        channel.port,
        "--baud",
        str(channel.baud),
        "--nodes",
        ",".join(str(node_id) for node_id in channel.node_ids),
        "--output-dir",
        channel_output_dir(system_config, channel),
        "--format",
        system_config.storage.format,
        "--compression",
        system_config.storage.compression,
        "--capture-schema",
        str(system_config.storage.capture_schema),
        "--sensor-label",
        (
            channel.nodes[0].sensor_label
            or chr(ord("A") + (channel.channel_id or 1) - 1)
        ),
        "--sensor-id",
        channel.nodes[0].sensor_id or "",
        "--hardware-id",
        channel.nodes[0].hardware_id or "",
        "--window-seconds",
        str(system_config.storage.window_seconds),
        "--min-free-bytes",
        str(system_config.storage.min_free_bytes),
        "--window-timezone",
        system_config.system.timezone or "local",
        "--timing-mode",
        channel.timing_mode,
        "--start-from",
        channel.start_from,
        "--grant-packets",
        str(channel.grant_packets),
        "--timeout",
        str(channel.timeout),
        "--burst-idle-timeout",
        str(channel.burst_idle_timeout),
        "--burst-session-timeout",
        str(channel.burst_session_timeout),
        "--status-interval",
        str(channel.status_interval_s),
        "--console-status-interval",
        str(system_config.supervisor.console_status_interval_s),
        "--flush-interval",
        str(channel.flush_interval_s),
        "--stats-interval",
        str(channel.stats_interval_s),
        "--temperature-interval",
        str(channel.temperature_interval_s),
        "--idle-sleep",
        str(channel.idle_sleep_s),
        "--error-sleep",
        str(channel.error_sleep_s),
        "--status-file",
        str(status_file),
        "--event-log",
        str(event_log),
    ]
    if channel.nodes[0].board_revision is not None:
        command.extend(["--board-revision", str(channel.nodes[0].board_revision)])
    return command


def rotate_process_log(path: Path, max_bytes: int, backup_count: int) -> bool:
    """Copy-truncate an active subprocess log without invalidating its stdout fd."""
    if max_bytes <= 0 or backup_count <= 0:
        return False
    try:
        if path.stat().st_size <= max_bytes:
            return False
        oldest = path.with_name(f"{path.name}.{backup_count}")
        oldest.unlink(missing_ok=True)
        for index in range(backup_count - 1, 0, -1):
            source = path.with_name(f"{path.name}.{index}")
            if source.exists():
                os.replace(source, path.with_name(f"{path.name}.{index + 1}"))
        temporary = path.with_name(f"{path.name}.1.tmp")
        shutil.copyfile(path, temporary)
        os.replace(temporary, path.with_name(f"{path.name}.1"))
        with path.open("w", encoding="utf-8"):
            pass
        return True
    except (FileNotFoundError, OSError):
        return False


def spawn_worker(
    state: WorkerState,
    python_executable: str,
    recorder_script: Path,
    system_config: HostSystemConfig,
    event_writer: JsonlEventWriter,
) -> None:
    if not state.config.enabled:
        return
    command = build_worker_command(
        python_executable,
        recorder_script,
        state.config,
        system_config,
        state.status_file,
        state.event_log,
    )
    env = dict(os.environ)
    env.setdefault("PYTHONUNBUFFERED", "1")
    state.process_log.parent.mkdir(parents=True, exist_ok=True)
    try:
        state.process_log_handle = state.process_log.open("a", encoding="utf-8", buffering=1)
        process_output: TextIO | int = state.process_log_handle
    except OSError as exc:
        state.process_log_handle = None
        process_output = subprocess.DEVNULL
        event_writer.emit(
            "channel_process_log_unavailable",
            severity="error",
            fields={
                "channel_name": state.config.name,
                "process_log": str(state.process_log),
                "error": str(exc),
                "fallback": "devnull",
            },
        )
    state.process = subprocess.Popen(
        command,
        env=env,
        text=True,
        stdout=process_output,
        stderr=subprocess.STDOUT,
    )
    state.process_started_monotonic = time.monotonic()
    event_writer.emit(
        "channel_started",
        fields={
            "channel_name": state.config.name,
            "port": state.config.port,
            "pid": state.process.pid,
            "process_log": str(state.process_log),
        },
    )


def stop_worker(state: WorkerState, event_writer: JsonlEventWriter) -> None:
    if state.process is None:
        state.close_process_log()
        return
    process = state.process
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5.0)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5.0)
    event_writer.emit(
        "channel_stopped",
        fields={
            "channel_name": state.config.name,
            "port": state.config.port,
            "exit_code": process.returncode,
            "process_log": str(state.process_log),
        },
    )
    state.last_exit_code = process.returncode
    state.process = None
    state.process_started_monotonic = None
    state.close_process_log()


def purge_worker_runtime(state: WorkerState, event_writer: JsonlEventWriter) -> None:
    state.close_process_log()
    state.event_log.unlink(missing_ok=True)
    state.status_file.unlink(missing_ok=True)
    _truncate_file(state.process_log)
    _remove_channel_entries_from_jsonl(event_writer.path, state.config.name)
    state.last_status = None
    state.last_status_updated_utc = None
    state.last_exit_code = None
    state.restart_count = 0
    state.consecutive_failures = 0
    state.failure_latched_reason = None


def control_state_label(state: WorkerState, now_monotonic: float) -> str:
    if not state.config.enabled:
        return "disabled"
    if state.failure_latched_reason is not None:
        return "failed-storage"
    if not state.desired_running:
        return "stopped-manual"
    if state.running:
        return "running"
    if state.next_start_monotonic > now_monotonic:
        return "restart-pending"
    return "waiting"


def restart_delay_for(failure_count: int, base_delay_s: float, max_delay_s: float) -> float:
    if failure_count <= 0:
        return max(0.0, base_delay_s)
    exponent = min(failure_count - 1, 20)
    return min(max(0.0, max_delay_s), max(0.0, base_delay_s) * (2 ** exponent))


def register_worker_exit(
    state: WorkerState,
    *,
    exit_code: int,
    process_runtime_s: float,
    now_monotonic: float,
    restart_delay_s: float,
    restart_delay_max_s: float,
    event_writer: JsonlEventWriter,
) -> None:
    """Record an exit and latch fatal storage failures for operator action."""
    if process_runtime_s >= 300.0:
        state.consecutive_failures = 0
    state.consecutive_failures += 1
    state.last_exit_code = exit_code
    state.process = None
    state.process_started_monotonic = None
    state.restart_count += 1
    if exit_code == 3:
        state.desired_running = False
        state.failure_latched_reason = "storage-error"
    delay = restart_delay_for(
        state.consecutive_failures,
        restart_delay_s,
        restart_delay_max_s,
    )
    state.next_start_monotonic = now_monotonic + delay
    state.close_process_log()
    event_writer.emit(
        "channel_exited",
        severity="warning" if exit_code == 0 else "error",
        fields={
            "channel_name": state.config.name,
            "port": state.config.port,
            "exit_code": exit_code,
            "restart_count": state.restart_count,
            "consecutive_failures": state.consecutive_failures,
            "restart_delay_s": delay,
            "process_runtime_s": process_runtime_s,
            "process_log": str(state.process_log),
        },
    )
    if exit_code == 3:
        event_writer.emit(
            "channel_failure_latched",
            severity="critical",
            fields={
                "channel_name": state.config.name,
                "port": state.config.port,
                "reason": state.failure_latched_reason,
                "process_log": str(state.process_log),
                "operator_action": "repair storage and start/restart channel",
            },
        )


def apply_channel_command(
    state: WorkerState,
    action: str,
    *,
    now_monotonic: float,
    restart_delay_s: float,
    event_writer: JsonlEventWriter,
) -> None:
    if action == "stop":
        state.desired_running = False
        state.next_start_monotonic = 0.0
        if state.running:
            stop_worker(state, event_writer)
        event_writer.emit(
            "channel_stop_requested",
            severity="warning",
            fields={"channel_name": state.config.name, "port": state.config.port},
        )
        return

    if action == "start":
        if not state.config.enabled:
            event_writer.emit(
                "channel_start_rejected",
                severity="error",
                fields={"channel_name": state.config.name, "reason": "disabled-in-config"},
            )
            return
        state.desired_running = True
        state.failure_latched_reason = None
        state.consecutive_failures = 0
        state.next_start_monotonic = now_monotonic
        event_writer.emit(
            "channel_start_requested",
            fields={"channel_name": state.config.name, "port": state.config.port},
        )
        return

    if action == "restart":
        if not state.config.enabled:
            event_writer.emit(
                "channel_restart_rejected",
                severity="error",
                fields={"channel_name": state.config.name, "reason": "disabled-in-config"},
            )
            return
        state.desired_running = True
        state.failure_latched_reason = None
        state.consecutive_failures = 0
        if state.running:
            stop_worker(state, event_writer)
            state.next_start_monotonic = now_monotonic + restart_delay_s
        else:
            state.next_start_monotonic = now_monotonic
        event_writer.emit(
            "channel_restart_requested",
            severity="warning",
            fields={"channel_name": state.config.name, "port": state.config.port},
        )
        return

    if action == "purge":
        if not state.config.enabled:
            event_writer.emit(
                "channel_purge_rejected",
                severity="error",
                fields={"channel_name": state.config.name, "reason": "disabled-in-config"},
            )
            return
        state.desired_running = True
        if state.running:
            stop_worker(state, event_writer)
            state.next_start_monotonic = now_monotonic + restart_delay_s
        else:
            state.next_start_monotonic = now_monotonic
        purge_worker_runtime(state, event_writer)
        event_writer.emit(
            "channel_purged",
            fields={"channel_name": state.config.name, "port": state.config.port},
        )
        return


def to_runtime_nodes(raw_nodes: list[dict[str, Any]], channel: ChannelConfig) -> list[RuntimeStatusNode]:
    node_names = channel.node_name_map()
    nodes: list[RuntimeStatusNode] = []
    for raw in raw_nodes:
        node_id = int(raw.get("node_id", 0))
        nodes.append(
            RuntimeStatusNode(
                node_id=node_id,
                name=node_names.get(node_id),
                firmware_version=raw.get("firmware_version"),
                board_revision=(
                    int(raw["board_revision"])
                    if raw.get("board_revision") is not None
                    else None
                ),
                online=bool(raw.get("online", False)),
                sensor_odr_hz=int(raw.get("sensor_odr_hz", 0)),
                output_odr_hz=float(raw.get("output_odr_hz", 0.0)),
                samples_written=int(raw.get("samples_written", 0)),
                instant_samples_per_second_5s=float(raw.get("instant_samples_per_second_5s")) if raw.get("instant_samples_per_second_5s") is not None else None,
                rate_stability_percent_5s=float(raw.get("rate_stability_percent_5s")) if raw.get("rate_stability_percent_5s") is not None else None,
                sample_flow_state=str(raw.get("sample_flow_state", "unknown")),
                expected_sample_seq=int(raw.get("expected_sample_seq", 0)),
                last_written_seq=int(raw.get("last_written_seq", 0)),
                bursts_ok=int(raw.get("bursts_ok", 0)),
                bursts_no_data=int(raw.get("bursts_no_data", 0)),
                bursts_failed=int(raw.get("bursts_failed", 0)),
                gaps_detected=int(raw.get("gaps_detected", 0)),
                empty_polls=int(raw.get("empty_polls", 0)),
                sensor_loss_total=int(raw.get("sensor_loss_total", 0)),
                sensor_loss_session=int(raw.get("sensor_loss_session", 0)),
                rx_overflow_total=int(raw.get("rx_overflow_total", 0)),
                rx_overflow_session=int(raw.get("rx_overflow_session", 0)),
                packet_overwrite_total=int(raw.get("packet_overwrite_total", 0)),
                packet_overwrite_session=int(raw.get("packet_overwrite_session", 0)),
                baseline_sensor_loss=int(raw.get("baseline_sensor_loss", 0)),
                baseline_rx_overflow_count=int(raw.get("baseline_rx_overflow_count", 0)),
                baseline_packet_overwrite_count=int(raw.get("baseline_packet_overwrite_count", 0)),
                last_temperature_c=raw.get("last_temperature_c"),
                last_temperature_unix_ns=raw.get("last_temperature_unix_ns"),
            )
        )
    return nodes


def apply_supervisor_command(
    states: list[WorkerState],
    action: str,
    *,
    now_monotonic: float,
    restart_delay_s: float,
    event_writer: JsonlEventWriter,
) -> None:
    normalized_action = action.strip().lower()
    if normalized_action not in {"restart_all", "purge_all"}:
        event_writer.emit(
            "supervisor_command_rejected",
            severity="error",
            fields={"action": normalized_action or "missing"},
        )
        return

    enabled_states = [state for state in states if state.config.enabled]
    any_running = False
    for state in enabled_states:
        state.desired_running = True
        state.failure_latched_reason = None
        state.consecutive_failures = 0
        if state.running:
            any_running = True
            stop_worker(state, event_writer)
        if normalized_action == "purge_all":
            purge_worker_runtime(state, event_writer)
            event_writer.emit(
                "channel_purged",
                fields={"channel_name": state.config.name, "port": state.config.port, "scope": "all"},
            )

    restart_at = now_monotonic + restart_delay_s if any_running else now_monotonic
    for state in enabled_states:
        state.next_start_monotonic = restart_at
        event_writer.emit(
            "channel_restart_requested",
            severity="warning",
            fields={"channel_name": state.config.name, "port": state.config.port, "scope": "all"},
        )

    event_writer.emit(
        "supervisor_restart_all_requested" if normalized_action == "restart_all" else "supervisor_purge_all_requested",
        severity="warning",
        fields={
            "channel_count": len(enabled_states),
            "had_running_channels": any_running,
        },
    )


def build_supervisor_snapshot(
    system_config: HostSystemConfig,
    status_writer: JsonStatusWriter,
    event_log: Path,
    started_utc: str,
    states: list[WorkerState],
) -> SupervisorStatusSnapshot:
    now_monotonic = time.monotonic()
    try:
        storage_usage = shutil.disk_usage(system_config.storage.root_dir)
        storage_total_bytes = storage_usage.total
        storage_free_bytes = storage_usage.free
        storage_used_percent = (
            100.0 * storage_usage.used / storage_usage.total
            if storage_usage.total > 0
            else 0.0
        )
    except OSError:
        storage_total_bytes = 0
        storage_free_bytes = 0
        storage_used_percent = 0.0
    channels: list[SupervisorChannelStatus] = []
    for state in states:
        if state.status_file.exists():
            status_payload = load_json(state.status_file)
            if status_payload is not None:
                state.last_status = status_payload
                state.last_status_updated_utc = status_payload.get("updated_utc")

        nodes = to_runtime_nodes(state.last_status.get("nodes", []), state.config) if state.last_status else []
        destination = channel_output_dir(system_config, state.config)
        channels.append(
            SupervisorChannelStatus(
                name=state.config.name,
                label=state.config.label,
                enabled=state.config.enabled,
                desired_running=state.desired_running,
                control_state=control_state_label(state, now_monotonic),
                port=state.config.port,
                baud=state.config.baud,
                process_id=state.process_id,
                running=state.running,
                restart_count=state.restart_count,
                last_exit_code=state.last_exit_code,
                updated_utc=state.last_status_updated_utc,
                destination=state.last_status.get("destination", destination)
                if state.last_status
                else destination,
                active_file=state.last_status.get("active_file") if state.last_status else None,
                status_file=str(state.status_file),
                event_log=str(state.event_log),
                process_log=str(state.process_log),
                nodes=nodes,
                timing_mode=(
                    str(state.last_status.get(
                        "timing_mode",
                        state.config.timing_mode,
                    ))
                    if state.last_status
                    else state.config.timing_mode
                ),
                failure_reason=state.failure_latched_reason,
            )
        )

    return SupervisorStatusSnapshot(
        schema_version=1,
        updated_utc=datetime.now(timezone.utc).isoformat(),
        started_utc=started_utc,
        supervisor_version=SUPERVISOR_VERSION,
        storage_root=system_config.storage.root_dir,
        status_file=str(status_writer.path),
        event_log=str(event_log),
        channels=channels,
        storage_total_bytes=storage_total_bytes,
        storage_free_bytes=storage_free_bytes,
        storage_used_percent=storage_used_percent,
    )


def ensure_storage_root(path: str) -> None:
    root = Path(path)
    try:
        root.mkdir(parents=True, exist_ok=True)
    except PermissionError as exc:
        raise RuntimeError(
            f"cannot create storage root '{root}'. "
            "Choose a writable path in host/system_config.json, for example 'runs/sensor-system'."
        ) from exc
    except OSError as exc:
        raise RuntimeError(f"cannot prepare storage root '{root}': {exc}") from exc


class SupervisorInstanceLock:
    def __init__(self, path: Path) -> None:
        self.path = path
        self.handle: TextIO | None = None

    def acquire(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.handle = self.path.open("a+", encoding="utf-8")
        try:
            fcntl.flock(self.handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise RuntimeError(
                f"another supervisor instance is already running (lock: {self.path})"
            ) from exc
        self.handle.seek(0)
        self.handle.truncate()
        self.handle.write(f"{os.getpid()}\n")
        self.handle.flush()

    def release(self) -> None:
        if self.handle is None:
            return
        try:
            fcntl.flock(self.handle.fileno(), fcntl.LOCK_UN)
        finally:
            self.handle.close()
            self.handle = None


def main() -> int:
    args = parse_args()
    system_config = HostSystemConfig.load(args.config)
    ensure_storage_root(system_config.storage.root_dir)
    runtime_dir = Path(system_config.supervisor.channel_runtime_dir)
    runtime_dir.mkdir(parents=True, exist_ok=True)
    log_dir = Path(system_config.supervisor.log_dir)
    log_dir.mkdir(parents=True, exist_ok=True)
    instance_lock = SupervisorInstanceLock(runtime_dir / "supervisor.lock")
    instance_lock.acquire()
    supervisor_commands = supervisor_command_path(runtime_dir)

    recorder_script = Path(__file__).resolve().parent / "host_recorder.py"
    status_writer = JsonStatusWriter(system_config.supervisor.status_file)
    event_writer = JsonlEventWriter(system_config.supervisor.event_log)
    stop_flag = StopFlag()
    signal.signal(signal.SIGINT, stop_flag.request_stop)
    signal.signal(signal.SIGTERM, stop_flag.request_stop)

    started_utc = datetime.now(timezone.utc).isoformat()
    states = [
        WorkerState(
            config=channel,
            status_file=channel_status_path(runtime_dir, channel.name),
            event_log=channel_event_path(log_dir, channel.name),
            process_log=channel_process_log_path(log_dir, channel.name),
            command_file=channel_command_path(runtime_dir, channel.name),
            desired_running=channel.enabled,
        )
        for channel in system_config.channels
    ]

    event_writer.emit(
        "supervisor_started",
        fields={
            "config_path": str(Path(args.config).resolve()),
            "channel_count": len(states),
        },
    )

    try:
        for state in states:
            if state.config.enabled:
                spawn_worker(state, sys.executable, recorder_script, system_config, event_writer)

        while not stop_flag.stop_requested:
            now = time.monotonic()
            supervisor_command = load_channel_command(supervisor_commands)
            if supervisor_command is not None:
                apply_supervisor_command(
                    states,
                    str(supervisor_command.get("action", "")),
                    now_monotonic=now,
                    restart_delay_s=system_config.supervisor.restart_delay_s,
                    event_writer=event_writer,
                )
            for state in states:
                rotate_process_log(
                    state.process_log,
                    system_config.supervisor.process_log_max_bytes,
                    system_config.supervisor.process_log_backup_count,
                )
                command = load_channel_command(state.command_file)
                if command is not None:
                    action = str(command.get("action", "")).strip().lower()
                    if action in {"start", "stop", "restart", "purge"}:
                        apply_channel_command(
                            state,
                            action,
                            now_monotonic=now,
                            restart_delay_s=system_config.supervisor.restart_delay_s,
                            event_writer=event_writer,
                        )
                    else:
                        event_writer.emit(
                            "channel_command_rejected",
                            severity="error",
                            fields={"channel_name": state.config.name, "action": action or "missing"},
                        )

                if not state.config.enabled:
                    state.desired_running = False
                    if state.running:
                        stop_worker(state, event_writer)
                    continue
                if state.process is not None:
                    exit_code = state.process.poll()
                    if exit_code is not None:
                        process_runtime_s = (
                            now - state.process_started_monotonic
                            if state.process_started_monotonic is not None
                            else 0.0
                        )
                        register_worker_exit(
                            state,
                            exit_code=exit_code,
                            process_runtime_s=process_runtime_s,
                            now_monotonic=now,
                            restart_delay_s=(
                                system_config.supervisor.restart_delay_s
                            ),
                            restart_delay_max_s=(
                                system_config.supervisor.restart_delay_max_s
                            ),
                            event_writer=event_writer,
                        )

                if (
                    state.process is None
                    and state.desired_running
                    and not stop_flag.stop_requested
                    and now >= state.next_start_monotonic
                ):
                    spawn_worker(state, sys.executable, recorder_script, system_config, event_writer)

            snapshot = build_supervisor_snapshot(
                system_config,
                status_writer,
                Path(system_config.supervisor.event_log),
                started_utc,
                states,
            )
            status_writer.write(snapshot)
            time.sleep(max(0.1, system_config.supervisor.status_interval_s))
    finally:
        if stop_flag.stop_requested:
            event_writer.emit(
                "supervisor_stop_requested",
                severity="warning",
                fields={
                    "signal_number": stop_flag.signal_number,
                    "signal_name": stop_flag.signal_name,
                },
            )
        for state in states:
            stop_worker(state, event_writer)

        status_writer.write(
            build_supervisor_snapshot(
                system_config,
                status_writer,
                Path(system_config.supervisor.event_log),
                started_utc,
                states,
            )
        )
        event_writer.emit(
            "supervisor_stopped",
            fields={
                "signal_number": stop_flag.signal_number,
                "signal_name": stop_flag.signal_name,
            },
        )
        instance_lock.release()

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
