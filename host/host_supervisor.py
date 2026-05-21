#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
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


SUPERVISOR_VERSION = "0.3.0"


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
    process: subprocess.Popen[str] | None = None
    process_log_handle: TextIO | None = None
    restart_count: int = 0
    last_exit_code: int | None = None
    next_start_monotonic: float = 0.0
    last_status: dict[str, Any] | None = None
    last_status_updated_utc: str | None = None

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


def channel_event_path(runtime_dir: Path, channel_name: str) -> Path:
    return runtime_dir / f"{channel_name}.events.jsonl"


def channel_process_log_path(runtime_dir: Path, channel_name: str) -> Path:
    return runtime_dir / f"{channel_name}.process.log"


def build_worker_command(
    python_executable: str,
    recorder_script: Path,
    channel: ChannelConfig,
    system_config: HostSystemConfig,
    status_file: Path,
    event_log: Path,
) -> list[str]:
    return [
        python_executable,
        str(recorder_script),
        "--channel-name",
        channel.name,
        "--port",
        channel.port,
        "--baud",
        str(channel.baud),
        "--nodes",
        ",".join(str(node_id) for node_id in channel.node_ids),
        "--output-dir",
        system_config.storage.root_dir,
        "--format",
        system_config.storage.format,
        "--compression",
        system_config.storage.compression,
        "--window-seconds",
        str(system_config.storage.window_seconds),
        "--window-timezone",
        system_config.system.timezone or "local",
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
    state.process_log_handle = state.process_log.open("a", encoding="utf-8", buffering=1)
    state.process = subprocess.Popen(
        command,
        env=env,
        text=True,
        stdout=state.process_log_handle,
        stderr=subprocess.STDOUT,
    )
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
    state.close_process_log()


def to_runtime_nodes(raw_nodes: list[dict[str, Any]], channel: ChannelConfig) -> list[RuntimeStatusNode]:
    node_names = channel.node_name_map()
    nodes: list[RuntimeStatusNode] = []
    for raw in raw_nodes:
        node_id = int(raw.get("node_id", 0))
        nodes.append(
            RuntimeStatusNode(
                node_id=node_id,
                name=node_names.get(node_id),
                online=bool(raw.get("online", False)),
                sensor_odr_hz=int(raw.get("sensor_odr_hz", 0)),
                output_odr_hz=float(raw.get("output_odr_hz", 0.0)),
                samples_written=int(raw.get("samples_written", 0)),
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


def build_supervisor_snapshot(
    system_config: HostSystemConfig,
    status_writer: JsonStatusWriter,
    event_log: Path,
    started_utc: str,
    states: list[WorkerState],
) -> SupervisorStatusSnapshot:
    channels: list[SupervisorChannelStatus] = []
    for state in states:
        if state.status_file.exists():
            status_payload = load_json(state.status_file)
            if status_payload is not None:
                state.last_status = status_payload
                state.last_status_updated_utc = status_payload.get("updated_utc")

        nodes = to_runtime_nodes(state.last_status.get("nodes", []), state.config) if state.last_status else []
        channels.append(
            SupervisorChannelStatus(
                name=state.config.name,
                label=state.config.label,
                enabled=state.config.enabled,
                port=state.config.port,
                baud=state.config.baud,
                process_id=state.process_id,
                running=state.running,
                restart_count=state.restart_count,
                last_exit_code=state.last_exit_code,
                updated_utc=state.last_status_updated_utc,
                destination=state.last_status.get("destination", system_config.storage.root_dir)
                if state.last_status
                else system_config.storage.root_dir,
                active_file=state.last_status.get("active_file") if state.last_status else None,
                status_file=str(state.status_file),
                event_log=str(state.event_log),
                process_log=str(state.process_log),
                nodes=nodes,
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


def main() -> int:
    args = parse_args()
    system_config = HostSystemConfig.load(args.config)
    ensure_storage_root(system_config.storage.root_dir)
    runtime_dir = Path(system_config.supervisor.channel_runtime_dir)
    runtime_dir.mkdir(parents=True, exist_ok=True)

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
            event_log=channel_event_path(runtime_dir, channel.name),
            process_log=channel_process_log_path(runtime_dir, channel.name),
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
            for state in states:
                if not state.config.enabled:
                    continue
                if state.process is not None:
                    exit_code = state.process.poll()
                    if exit_code is not None:
                        state.last_exit_code = exit_code
                        state.process = None
                        state.restart_count += 1
                        state.next_start_monotonic = now + system_config.supervisor.restart_delay_s
                        state.close_process_log()
                        event_writer.emit(
                            "channel_exited",
                            severity="warning" if exit_code == 0 else "error",
                            fields={
                                "channel_name": state.config.name,
                                "port": state.config.port,
                                "exit_code": exit_code,
                                "restart_count": state.restart_count,
                                "process_log": str(state.process_log),
                            },
                        )

                if (
                    state.process is None
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

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
