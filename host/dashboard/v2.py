from __future__ import annotations

import json
import re
import subprocess
import threading
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from host.dashboard.v2_operations import (
    DEFAULT_OPERATIONS_SOCKET,
    OperationsIpcClient,
    OperationsIpcClientError,
    fresh_operation_id,
)


DEFAULT_STATUS_PATH = Path("/run/sensor-system/host-v2-status.json")
DEFAULT_SERVICE_UNIT = "sensor-host-v2.service"
MAX_STATUS_BYTES = 4 * 1024 * 1024
STATUS_STALE_AFTER_S = 30.0
JOURNAL_CACHE_SECONDS = 5.0


def _deep_merge(base: dict[str, Any], overlay: dict[str, Any]) -> dict[str, Any]:
    merged = dict(base)
    for key, value in overlay.items():
        if key == "extends":
            continue
        if isinstance(value, dict) and isinstance(merged.get(key), dict):
            merged[key] = _deep_merge(merged[key], value)
        else:
            merged[key] = value
    return merged


def load_v2_profile(path: Path, seen: set[Path] | None = None) -> dict[str, Any]:
    resolved = path.resolve()
    visited = set() if seen is None else set(seen)
    if resolved in visited:
        raise ValueError(f"circular config extends: {resolved}")
    visited.add(resolved)
    payload = json.loads(resolved.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise ValueError(f"config root must be an object: {resolved}")
    parent = payload.get("extends")
    if parent is None:
        return _deep_merge({}, payload)
    if not isinstance(parent, str) or not parent.strip():
        raise ValueError(f"invalid config extends: {resolved}")
    parent_path = Path(parent)
    if not parent_path.is_absolute():
        parent_path = resolved.parent / parent_path
    return _deep_merge(load_v2_profile(parent_path, visited), payload)


def is_v2_profile(path: Path) -> bool:
    try:
        payload = load_v2_profile(path)
    except (OSError, ValueError, json.JSONDecodeError):
        return False
    channels = payload.get("channels")
    return isinstance(channels, list) and any(
        isinstance(channel, dict) and isinstance(channel.get("transport"), dict)
        for channel in channels
    )


def _unix_ms_to_iso(value: Any) -> str | None:
    try:
        milliseconds = int(value)
    except (TypeError, ValueError):
        return None
    return datetime.fromtimestamp(milliseconds / 1000.0, tz=timezone.utc).isoformat()


def _load_status(path: Path) -> dict[str, Any] | None:
    try:
        if path.stat().st_size > MAX_STATUS_BYTES:
            return None
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError, json.JSONDecodeError):
        return None
    if not isinstance(payload, dict) or payload.get("schema_version") != 1:
        return None
    return payload


def _safe_int(value: Any, default: int = 0) -> int:
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


class V2DashboardRepository:
    read_only = True
    supports_v2_operations = True
    supports_live_preview = False
    supports_data_browser = False

    def __init__(
        self,
        config_path: str | Path,
        default_event_limit: int = 40,
        *,
        status_path: str | Path = DEFAULT_STATUS_PATH,
        service_unit: str = DEFAULT_SERVICE_UNIT,
        operations_socket: str | Path = DEFAULT_OPERATIONS_SOCKET,
        operations_client: OperationsIpcClient | None = None,
    ) -> None:
        self.config_path = Path(config_path)
        self.default_event_limit = default_event_limit
        self.status_path = Path(status_path)
        self.service_unit = service_unit
        self.operations = operations_client or OperationsIpcClient(operations_socket)
        self._journal_lock = threading.Lock()
        self._journal_cache: tuple[float, int, list[dict[str, Any]]] | None = None

    def operator_capabilities_payload(self) -> dict[str, Any]:
        return {
            "schema_version": 1,
            "operations_transport": "host-v2-unix-datagram",
            "submission": "asynchronous",
            "service_control": False,
            "channel_operations": [
                "enter_maintenance",
                "resume_recording",
                "scan_unassigned",
                "assign_address",
            ],
            "node_operations": [
                "get_configuration",
                "validate_configuration",
                "apply_configuration",
                "reset_configuration",
                "restart_node",
            ],
        }

    def operation_status_payload(self, channel_id: int) -> dict[str, Any]:
        self._validate_channel_id(channel_id)
        return self.operations.status(channel_id)

    def operation_result_payload(self, channel_id: int, operation_id: int) -> dict[str, Any]:
        self._validate_channel_id(channel_id)
        if operation_id <= 0 or operation_id >= 1 << 64:
            raise ValueError("operation_id must be in 1..2^64-1")
        return self.operations.get(channel_id, operation_id)

    def submit_operation_payload(
        self,
        channel_id: int,
        payload: dict[str, Any],
    ) -> dict[str, Any]:
        self._validate_channel_id(channel_id)
        operation = payload.get("operation")
        allowed = {
            "enter_maintenance",
            "resume_recording",
            "get_configuration",
            "validate_configuration",
            "apply_configuration",
            "reset_configuration",
            "restart_node",
            "scan_unassigned",
            "assign_address",
        }
        if operation not in allowed:
            raise ValueError("unsupported host-v2 operation")

        operation_id = payload.get("operation_id", fresh_operation_id())
        if isinstance(operation_id, bool) or not isinstance(operation_id, int):
            raise ValueError("operation_id must be an integer")
        if operation_id <= 0 or operation_id >= 1 << 64:
            raise ValueError("operation_id must be in 1..2^64-1")

        request: dict[str, Any] = {
            "channel_id": channel_id,
            "operation_id": operation_id,
            "operation": operation,
        }
        fields_by_operation = {
            "get_configuration": ("node_address",),
            "validate_configuration": ("node_address", "configuration"),
            "apply_configuration": (
                "node_address",
                "expected_revision",
                "configuration",
            ),
            "reset_configuration": ("node_address", "expected_revision"),
            "restart_node": ("node_address",),
            "scan_unassigned": ("round_nonce", "slot_count"),
            "assign_address": (
                "round_nonce",
                "hardware_id",
                "new_node_address",
            ),
        }
        for field in fields_by_operation.get(operation, ()):
            if field not in payload:
                raise ValueError(f"{field} is required for {operation}")
            request[field] = payload[field]
        return self.operations.submit(request)

    def _validate_channel_id(self, channel_id: int) -> None:
        if isinstance(channel_id, bool) or not 1 <= channel_id <= 16:
            raise ValueError("channel_id must be in 1..16")
        channel_ids = {
            _safe_int(channel.get("channel_id"))
            for channel in self._profile().get("channels", [])
            if isinstance(channel, dict) and bool(channel.get("enabled", True))
        }
        if channel_id not in channel_ids:
            raise ValueError("channel is not enabled in the installation profile")

    def _profile(self) -> dict[str, Any]:
        return load_v2_profile(self.config_path)

    def config_payload(self) -> dict[str, Any]:
        profile = self._profile()
        storage = dict(profile.get("storage", {}))
        storage["root_dir"] = storage.get("root_dir", "var/recordings")
        return {
            "path": str(self.config_path),
            "mode": "host-v2-read-only",
            "system": profile.get("system", {}),
            "storage": storage,
            "supervisor": {
                "status_file": str(self.status_path),
                "event_log": f"journald:{self.service_unit}",
                "channel_runtime_dir": "runtime status v1",
            },
            "channels": profile.get("channels", []),
        }

    def _status_age(self, status: dict[str, Any] | None) -> tuple[str | None, float | None, bool]:
        if status is None:
            return None, None, True
        updated = _unix_ms_to_iso(status.get("process", {}).get("updated_unix_ms"))
        if updated is None:
            return None, None, True
        updated_dt = datetime.fromisoformat(updated)
        age = max(0.0, (datetime.now(timezone.utc) - updated_dt).total_seconds())
        return updated, age, age > STATUS_STALE_AFTER_S

    def _journal_entries(self, limit: int) -> list[dict[str, Any]]:
        bounded = max(1, min(int(limit), 200))
        now = time.monotonic()
        with self._journal_lock:
            cached = self._journal_cache
            if cached is not None and cached[0] > now and cached[1] >= bounded:
                return [dict(entry) for entry in cached[2][-bounded:]]
        try:
            completed = subprocess.run(
                [
                    "journalctl",
                    "--unit",
                    self.service_unit,
                    "--lines",
                    str(bounded),
                    "--output",
                    "json",
                    "--no-pager",
                ],
                check=False,
                capture_output=True,
                text=True,
                timeout=2.0,
            )
        except (OSError, subprocess.TimeoutExpired):
            return []
        entries: list[dict[str, Any]] = []
        for line in completed.stdout.splitlines():
            try:
                raw = json.loads(line)
            except json.JSONDecodeError:
                continue
            message = str(raw.get("MESSAGE", "")).strip()
            if not message:
                continue
            priority = _safe_int(raw.get("PRIORITY"), 6)
            severity = "error" if priority <= 3 else "warning" if priority == 4 else "info"
            timestamp_us = _safe_int(raw.get("__REALTIME_TIMESTAMP"))
            utc = (
                datetime.fromtimestamp(timestamp_us / 1_000_000.0, tz=timezone.utc).isoformat()
                if timestamp_us > 0
                else None
            )
            channel_name = None
            match = re.search(r"\b(?:channel(?:_id)?|id)=([1-9][0-9]*)\b", message)
            if match:
                channel_name = f"line-{chr(96 + int(match.group(1)))}" if int(match.group(1)) <= 26 else None
            entries.append(
                {
                    "utc": utc,
                    "severity": severity,
                    "event": "host_runtime_log",
                    "channel_name": channel_name,
                    "summary": message,
                    "line": message,
                    "source_label": "journald",
                }
            )
        with self._journal_lock:
            self._journal_cache = (now + JOURNAL_CACHE_SECONDS, bounded, entries[-bounded:])
        return entries[-bounded:]

    def _build_channels(
        self,
        profile: dict[str, Any],
        status: dict[str, Any] | None,
        updated_utc: str | None,
        status_stale: bool,
    ) -> list[dict[str, Any]]:
        runtime_channels = {
            _safe_int(channel.get("channel_id")): channel
            for channel in (status or {}).get("channels", [])
            if isinstance(channel, dict)
        }
        runtime_nodes = {
            (_safe_int(node.get("channel_id")), _safe_int(node.get("node_address"))): node
            for node in (status or {}).get("nodes", [])
            if isinstance(node, dict)
        }
        storage_root = str((status or {}).get("storage", {}).get("root", profile.get("storage", {}).get("root_dir", "-")))
        result: list[dict[str, Any]] = []
        for raw_channel in profile.get("channels", []):
            if not isinstance(raw_channel, dict):
                continue
            channel_id = _safe_int(raw_channel.get("channel_id"))
            runtime = runtime_channels.get(channel_id, {})
            phase = str(runtime.get("phase", "offline"))
            metrics = runtime.get("metrics", {}) if isinstance(runtime.get("metrics"), dict) else {}
            enabled = bool(raw_channel.get("enabled", True))
            running = phase == "running" and not status_stale
            operations_mode = None
            active_operation_id = None
            operations_available = False
            if enabled:
                try:
                    operations_status = self.operations.status(channel_id)
                except OperationsIpcClientError:
                    operations_status = None
                if operations_status is not None and operations_status.get("ok") is True:
                    operations_mode = operations_status.get("mode")
                    active_operation_id = operations_status.get("active_operation_id")
                    operations_available = operations_mode in {
                        "recording",
                        "entering_maintenance",
                        "maintenance",
                        "resuming_recording",
                        "faulted",
                        "offline",
                    }
            channel_alerts: list[str] = []
            if phase == "faulted":
                channel_alerts.append("kanał faulted")
            if status_stale:
                channel_alerts.append("status runtime nieaktualny")
            for key, label in (
                ("decode_errors", "decode errors"),
                ("transaction_timeouts", "timeouts"),
                ("measurement_retries", "measurement retries"),
                ("persistence_backpressure", "persistence backpressure"),
                ("time_sync_backpressure", "time sync backpressure"),
                ("storage_failed_groups", "storage failures"),
            ):
                value = _safe_int(metrics.get(key))
                if value:
                    channel_alerts.append(f"{label}: {value}")
            nodes: list[dict[str, Any]] = []
            for raw_node in raw_channel.get("nodes", []):
                if not isinstance(raw_node, dict):
                    continue
                address = _safe_int(raw_node.get("id", raw_node.get("node_id")))
                node = runtime_nodes.get((channel_id, address))
                alerts = list(channel_alerts)
                cursors = node.get("cursors") if isinstance(node, dict) else None
                measurement_age_ms = node.get("last_measurement_age_ms") if isinstance(node, dict) else None
                time_sync_age_ms = node.get("last_time_sync_age_ms") if isinstance(node, dict) else None
                online = bool(node is not None and running and measurement_age_ms is not None)
                if node is None:
                    alerts.append("brak node w statusie runtime")
                elif cursors is None:
                    alerts.append("cursory niedostępne")
                received = _safe_int(cursors.get("received")) if isinstance(cursors, dict) else 0
                durable = _safe_int(cursors.get("durable")) if isinstance(cursors, dict) else 0
                released = _safe_int(cursors.get("released")) if isinstance(cursors, dict) else 0
                produced = _safe_int(cursors.get("produced")) if isinstance(cursors, dict) else 0
                nodes.append(
                    {
                        "node_id": address,
                        "name": raw_node.get("name"),
                        "enabled": bool(raw_node.get("enabled", True)),
                        "runtime_v2": True,
                        "has_runtime": node is not None,
                        "online": online,
                        "receiving_samples": online,
                        "sample_flow_state": "flowing" if online else "offline",
                        "firmware_version": node.get("firmware") if isinstance(node, dict) else None,
                        "hardware_id": node.get("hardware_id") if isinstance(node, dict) else raw_node.get("hardware_id"),
                        "sensor_odr_hz": 0,
                        "output_odr_hz": float(node.get("output_rate_hz", raw_node.get("output_rate_hz", 0))) if isinstance(node, dict) else float(raw_node.get("output_rate_hz", 0)),
                        "expected_odr_hz": float(raw_node.get("output_rate_hz", 0)),
                        "samples_written": durable,
                        "expected_sample_seq": produced,
                        "instant_samples_per_second_5s": None,
                        "rate_stability_percent_5s": None,
                        "gaps_detected": 0,
                        "bursts_no_data": _safe_int(metrics.get("measurement_no_data")),
                        "rx_overflow_session": _safe_int(metrics.get("decode_errors")),
                        "packet_overwrite_session": _safe_int(metrics.get("wire_attempt_failures")),
                        "last_temperature_c": None,
                        "last_temperature_unix_ns": None,
                        "received_cursor": received,
                        "durable_cursor": durable,
                        "released_cursor": released,
                        "produced_cursor": produced,
                        "last_measurement_age_ms": measurement_age_ms,
                        "last_time_sync_age_ms": time_sync_age_ms,
                        "alerts": alerts,
                    }
                )
            transport = raw_channel.get("transport", {}) if isinstance(raw_channel.get("transport"), dict) else {}
            health = "faulted" if phase == "faulted" else "degraded" if channel_alerts else "healthy"
            result.append(
                {
                    "name": str(raw_channel.get("name", f"channel-{channel_id}")),
                    "label": raw_channel.get("label"),
                    "channel_id": channel_id,
                    "runtime_v2": True,
                    "enabled": enabled,
                    "desired_running": enabled,
                    "controls_enabled": operations_available,
                    "control_state": operations_mode or "unavailable",
                    "operation_mode": operations_mode,
                    "active_operation_id": active_operation_id,
                    "running": running,
                    "health": health,
                    "phase": phase,
                    "port": transport.get("endpoint", "-"),
                    "baud": _safe_int(transport.get("baud_rate")),
                    "updated_utc": updated_utc,
                    "destination": storage_root,
                    "active_file": None,
                    "restart_count": 0,
                    "last_exit_code": None,
                    "instant_samples_per_second_5s": None,
                    "last_samples_per_second": None,
                    "attention_count": len(channel_alerts) + sum(len(node["alerts"]) for node in nodes),
                    "event_log": f"journald:{self.service_unit}",
                    "process_log": f"journald:{self.service_unit}",
                    "metrics": metrics,
                    "nodes": nodes,
                }
            )
        return result

    def dashboard_payload(self, limit: int | None = None) -> dict[str, Any]:
        profile = self._profile()
        status = _load_status(self.status_path)
        updated_utc, age_s, stale = self._status_age(status)
        channels = self._build_channels(profile, status, updated_utc, stale)
        events = self.events_payload(limit)
        enabled_channels = [channel for channel in channels if channel["enabled"]]
        enabled_nodes = [node for channel in enabled_channels for node in channel["nodes"] if node["enabled"]]
        running_channels = sum(1 for channel in enabled_channels if channel["running"])
        online_nodes = sum(1 for node in enabled_nodes if node["online"])
        attention_count = sum(channel["attention_count"] for channel in enabled_channels)
        storage = (status or {}).get("storage", {})
        process = (status or {}).get("process", {})
        phase = str(process.get("phase", "offline"))
        if status is None:
            summary = "Brak poprawnego Runtime Status v1"
        elif stale:
            summary = f"Runtime Status jest nieaktualny od {int(age_s or 0)} s"
        elif phase == "degraded":
            summary = "Recorder działa w trybie degraded; zdrowe kanały nadal nagrywają"
        elif phase == "recording":
            summary = "Recorder działa i wszystkie aktywne kanały są zdrowe"
        else:
            summary = f"Stan procesu: {phase}"
        severity_counts = {"info": 0, "warning": 0, "error": 0}
        for event in events:
            severity = str(event.get("severity", "info"))
            severity_counts[severity] = severity_counts.get(severity, 0) + 1
        overview = {
            "channels_total": len(channels),
            "channels_enabled": len(enabled_channels),
            "channels_running": running_channels,
            "nodes_total": len([node for channel in channels for node in channel["nodes"]]),
            "nodes_enabled": len(enabled_nodes),
            "nodes_online": online_nodes,
            "nodes_receiving_samples": online_nodes,
            "nodes_without_samples": sum(1 for node in enabled_nodes if node["has_runtime"] and not node["online"]),
            "samples_written_total": sum(_safe_int(node["durable_cursor"]) for node in enabled_nodes),
            "gaps_detected_total": 0,
            "restart_count_total": 0,
            "attention_count": attention_count + (1 if stale else 0),
            "events_by_severity": severity_counts,
            "status_age_s": age_s,
            "status_stale": stale,
            "storage_free_percent": None,
            "storage_low": str(storage.get("state", "unknown")) != "healthy",
            "status_summary": summary,
        }
        config = self.config_payload()
        supervisor = {
            "has_status": status is not None,
            "status_file": str(self.status_path),
            "event_log": f"journald:{self.service_unit}",
            "updated_utc": updated_utc,
            "started_utc": _unix_ms_to_iso(process.get("started_unix_ms")),
            "supervisor_version": (status or {}).get("software", {}).get("host_version"),
            "storage_root": storage.get("root", config["storage"].get("root_dir")),
            "status_age_s": age_s,
            "status_stale": stale,
            "storage_total_bytes": None,
            "storage_free_bytes": storage.get("available_bytes"),
            "storage_used_percent": None,
            "storage_low": overview["storage_low"],
        }
        return {
            "dashboard_version": "v2-observer-1",
            "dashboard_mode": "host-v2-operations",
            "read_only": True,
            "operator_api_available": any(
                channel.get("controls_enabled", False) for channel in channels
            ),
            "data_available": False,
            "live_preview_available": False,
            "generated_utc": datetime.now(timezone.utc).isoformat(),
            "config": config,
            "supervisor": supervisor,
            "overview": overview,
            "channels": channels,
            "events": events,
        }

    def events_payload(self, limit: int | None = None) -> list[dict[str, Any]]:
        bounded = self.default_event_limit if limit is None else max(1, min(int(limit), 500))
        status = _load_status(self.status_path)
        events: list[dict[str, Any]] = []
        if status is not None:
            utc = _unix_ms_to_iso(status.get("process", {}).get("updated_unix_ms"))
            for reason in status.get("health", {}).get("reasons", []):
                if not isinstance(reason, dict):
                    continue
                events.append(
                    {
                        "utc": utc,
                        "severity": "warning",
                        "event": str(reason.get("code", "health_reason")),
                        "channel_name": f"line-{chr(96 + _safe_int(reason.get('channel_id')))}" if 1 <= _safe_int(reason.get("channel_id")) <= 26 else None,
                    }
                )
            if status.get("last_error") is not None:
                events.append({"utc": utc, "severity": "error", "event": "last_error", "details": status["last_error"]})
        journal = self._journal_entries(max(bounded, 20))
        return (journal + events)[-bounded:]

    def logs_payload(self, limit: int | None = None, channel_name: str | None = None) -> dict[str, Any]:
        bounded = 12 if limit is None else max(1, min(int(limit), 200))
        dashboard = self.dashboard_payload(limit=1)
        journal = self._journal_entries(bounded * 4)
        filtered = [entry for entry in journal if channel_name is None or entry.get("channel_name") == channel_name]
        channel_logs = []
        for channel in dashboard["channels"]:
            if channel_name is not None and channel["name"] != channel_name:
                continue
            entries = [entry for entry in filtered if entry.get("channel_name") in {None, channel["name"]}][-bounded:]
            alerts = [entry for entry in entries if entry.get("severity") in {"warning", "error"}]
            channel_logs.append(
                {
                    "name": channel["name"],
                    "label": channel.get("label"),
                    "running": channel["running"],
                    "health": channel["health"],
                    "events": alerts,
                    "process_lines": [entry["line"] for entry in entries],
                    "alert_count": len(alerts),
                }
            )
        alerts = [entry for entry in filtered if entry.get("severity") in {"warning", "error"}]
        return {
            "generated_utc": datetime.now(timezone.utc).isoformat(),
            "limit": bounded,
            "channel_filter": channel_name,
            "supervisor_event_log": f"journald:{self.service_unit}",
            "supervisor_events": filtered[-bounded:],
            "alerts": alerts[-bounded:],
            "channels": channel_logs,
        }

    def health_payload(self) -> dict[str, Any]:
        dashboard = self.dashboard_payload(limit=1)
        overview = dashboard["overview"]
        return {
            "ok": True,
            "system_healthy": bool(dashboard["supervisor"]["has_status"] and not overview["status_stale"] and overview["attention_count"] == 0),
            "dashboard_version": dashboard["dashboard_version"],
            "generated_utc": dashboard["generated_utc"],
            "has_status": dashboard["supervisor"]["has_status"],
            "channels_running": overview["channels_running"],
            "nodes_online": overview["nodes_online"],
            "nodes_receiving_samples": overview["nodes_receiving_samples"],
            "nodes_without_samples": overview["nodes_without_samples"],
            "attention_count": overview["attention_count"],
        }

    def data_payload(self, raw_relative: str | None) -> dict[str, Any]:
        root = str(self.config_payload()["storage"].get("root_dir", "-"))
        return {"root": root, "relative_path": ".", "parent_relative_path": ".", "items": []}

    def data_search_payload(self, raw_query: str | None) -> dict[str, Any]:
        root = str(self.config_payload()["storage"].get("root_dir", "-"))
        return {"root": root, "query": raw_query or "", "items": [], "truncated": False}
