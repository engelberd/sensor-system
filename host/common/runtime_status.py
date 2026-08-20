from __future__ import annotations

import json
import os
import sys
import time
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


@dataclass
class RuntimeStatusNode:
    node_id: int
    name: str | None
    firmware_version: str | None
    online: bool
    sensor_odr_hz: int
    output_odr_hz: float
    samples_written: int
    expected_sample_seq: int
    last_written_seq: int
    bursts_ok: int
    bursts_no_data: int
    bursts_failed: int
    gaps_detected: int
    empty_polls: int
    sensor_loss_total: int
    sensor_loss_session: int
    rx_overflow_total: int
    rx_overflow_session: int
    packet_overwrite_total: int
    packet_overwrite_session: int
    baseline_sensor_loss: int
    baseline_rx_overflow_count: int
    baseline_packet_overwrite_count: int
    last_temperature_c: float | None
    last_temperature_unix_ns: int | None
    instant_samples_per_second_5s: float | None = None
    rate_stability_percent_5s: float | None = None
    sample_flow_state: str = "unknown"
    board_revision: int | None = None


@dataclass
class RuntimeStatusSnapshot:
    schema_version: int
    updated_utc: str
    started_utc: str
    recorder_version: str
    destination: str
    active_file: str | None
    port: str
    baud: int
    channel_name: str | None
    nodes: list[RuntimeStatusNode]
    timing_mode: str = "legacy"


@dataclass
class SupervisorChannelStatus:
    name: str
    label: str | None
    enabled: bool
    desired_running: bool
    control_state: str | None
    port: str
    baud: int
    process_id: int | None
    running: bool
    restart_count: int
    last_exit_code: int | None
    updated_utc: str | None
    destination: str
    active_file: str | None
    status_file: str
    event_log: str
    process_log: str | None
    nodes: list[RuntimeStatusNode]
    timing_mode: str = "legacy"
    failure_reason: str | None = None


@dataclass
class SupervisorStatusSnapshot:
    schema_version: int
    updated_utc: str
    started_utc: str
    supervisor_version: str
    storage_root: str
    status_file: str
    event_log: str
    channels: list[SupervisorChannelStatus]
    storage_total_bytes: int = 0
    storage_free_bytes: int = 0
    storage_used_percent: float = 0.0


class JsonStatusWriter:
    def __init__(self, path: str | Path, *, warning_interval_s: float = 60.0) -> None:
        self.path = Path(path)
        self.warning_interval_s = warning_interval_s
        self._last_warning_monotonic = float("-inf")
        self._prepare_parent()

    def _prepare_parent(self) -> bool:
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            return True
        except OSError as exc:
            self._warn(exc)
            return False

    def _warn(self, exc: OSError) -> None:
        now = time.monotonic()
        if now - self._last_warning_monotonic < self.warning_interval_s:
            return
        self._last_warning_monotonic = now
        print(
            f"[WARN] runtime status write failed for {self.path}: {exc}; "
            "measurement recording will continue",
            file=sys.stderr,
        )

    def write(self, snapshot: object) -> bool:
        tmp_path = self.path.with_suffix(self.path.suffix + ".tmp")
        payload: dict[str, Any] = asdict(snapshot)
        try:
            if not self._prepare_parent():
                return False
            tmp_path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
            os.replace(tmp_path, self.path)
            return True
        except OSError as exc:
            self._warn(exc)
            try:
                tmp_path.unlink(missing_ok=True)
            except OSError:
                pass
            return False


class JsonlEventWriter:
    def __init__(
        self,
        path: str | Path,
        *,
        max_bytes: int = 5 * 1024 * 1024,
        backup_count: int = 3,
        warning_interval_s: float = 60.0,
    ) -> None:
        self.path = Path(path)
        self.max_bytes = max(0, max_bytes)
        self.backup_count = max(0, backup_count)
        self.warning_interval_s = warning_interval_s
        self._last_warning_monotonic = float("-inf")
        self._prepare_parent()

    def _prepare_parent(self) -> bool:
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            return True
        except OSError as exc:
            self._warn(exc)
            return False

    def _warn(self, exc: OSError) -> None:
        now = time.monotonic()
        if now - self._last_warning_monotonic < self.warning_interval_s:
            return
        self._last_warning_monotonic = now
        print(
            f"[WARN] event log write failed for {self.path}: {exc}; "
            "measurement recording will continue",
            file=sys.stderr,
        )

    def _rotate_if_needed(self, incoming_bytes: int) -> None:
        if self.max_bytes <= 0 or self.backup_count <= 0:
            return
        try:
            current_bytes = self.path.stat().st_size
        except FileNotFoundError:
            return
        if current_bytes + incoming_bytes <= self.max_bytes:
            return

        oldest = self.path.with_name(f"{self.path.name}.{self.backup_count}")
        oldest.unlink(missing_ok=True)
        for index in range(self.backup_count - 1, 0, -1):
            source = self.path.with_name(f"{self.path.name}.{index}")
            if source.exists():
                os.replace(source, self.path.with_name(f"{self.path.name}.{index + 1}"))
        os.replace(self.path, self.path.with_name(f"{self.path.name}.1"))

    def emit(
        self,
        event: str,
        *,
        severity: str = "info",
        node_id: int | None = None,
        fields: dict[str, Any] | None = None,
    ) -> bool:
        payload: dict[str, Any] = {
            "utc": datetime.now(timezone.utc).isoformat(),
            "severity": severity,
            "event": event,
        }
        if node_id is not None:
            payload["node_id"] = node_id
        if fields:
            payload.update(fields)
        line = json.dumps(payload, sort_keys=True) + "\n"
        try:
            if not self._prepare_parent():
                return False
            self._rotate_if_needed(len(line.encode("utf-8")))
            with self.path.open("a", encoding="utf-8") as handle:
                handle.write(line)
            return True
        except OSError as exc:
            self._warn(exc)
            return False
