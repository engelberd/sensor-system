from __future__ import annotations

import json
import os
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


@dataclass
class RuntimeStatusNode:
    node_id: int
    name: str | None
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


@dataclass
class SupervisorChannelStatus:
    name: str
    label: str | None
    enabled: bool
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


class JsonStatusWriter:
    def __init__(self, path: str | Path) -> None:
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True)

    def write(self, snapshot: object) -> None:
        tmp_path = self.path.with_suffix(self.path.suffix + ".tmp")
        payload: dict[str, Any] = asdict(snapshot)
        tmp_path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        os.replace(tmp_path, self.path)


class JsonlEventWriter:
    def __init__(self, path: str | Path) -> None:
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True)

    def emit(
        self,
        event: str,
        *,
        severity: str = "info",
        node_id: int | None = None,
        fields: dict[str, Any] | None = None,
    ) -> None:
        payload: dict[str, Any] = {
            "utc": datetime.now(timezone.utc).isoformat(),
            "severity": severity,
            "event": event,
        }
        if node_id is not None:
            payload["node_id"] = node_id
        if fields:
            payload.update(fields)
        with self.path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(payload, sort_keys=True) + "\n")
