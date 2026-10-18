from __future__ import annotations

import json
import re
import subprocess
import threading
import time
from collections import deque
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from host.common.host_metrics import HostMetricsSampler
from host.dashboard.v2_operations import (
    DEFAULT_OPERATIONS_SOCKET,
    OperationsIpcClient,
    OperationsIpcClientError,
    fresh_operation_id,
)
from host.dashboard.v2_commissioning import CommissioningProfileStore
from host.dashboard.v2_service_control import (
    DEFAULT_SERVICE_CONTROL_SOCKET,
    ServiceControlClient,
    ServiceControlError,
)


DEFAULT_STATUS_PATH = Path("/run/sensor-system/host-v2-status.json")
DEFAULT_SERVICE_UNIT = "sensor-host-v2.service"
MAX_STATUS_BYTES = 4 * 1024 * 1024
STATUS_STALE_AFTER_S = 30.0
JOURNAL_CACHE_SECONDS = 5.0
SAMPLE_RATE_WINDOW_US = 5_000_000
OUTPUT_RATE_HZ_BY_CODE = {
    0: 4000.0,
    1: 2000.0,
    2: 1000.0,
    3: 500.0,
    4: 250.0,
    5: 125.0,
    6: 62.5,
    7: 31.25,
    8: 15.625,
    9: 7.8125,
    10: 3.90625,
}


def _deep_merge(base: Any, overlay: Any, section: str | None = None) -> Any:
    if isinstance(base, dict) and isinstance(overlay, dict):
        merged = dict(base)
        for key, value in overlay.items():
            if key == "extends":
                continue
            merged[key] = (
                _deep_merge(merged[key], value, key) if key in merged else value
            )
        return merged
    if isinstance(base, list) and isinstance(overlay, list) and section in {"channels", "nodes"}:
        merged = list(base)
        identity_key = "name" if section == "channels" else "id"
        positions = {
            item.get(identity_key): index
            for index, item in enumerate(merged)
            if isinstance(item, dict) and identity_key in item
        }
        for item in overlay:
            identity = item.get(identity_key) if isinstance(item, dict) else None
            if identity in positions:
                index = positions[identity]
                merged[index] = _deep_merge(merged[index], item, section)
            else:
                positions[identity] = len(merged)
                merged.append(item)
        return merged
    return overlay


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
        service_control_socket: str | Path = DEFAULT_SERVICE_CONTROL_SOCKET,
        service_control_client: ServiceControlClient | None = None,
        host_metrics_sampler: HostMetricsSampler | None = None,
    ) -> None:
        self.config_path = Path(config_path)
        self.default_event_limit = default_event_limit
        self.status_path = Path(status_path)
        self.service_unit = service_unit
        self.operations = operations_client or OperationsIpcClient(operations_socket)
        self.service_control = service_control_client or ServiceControlClient(service_control_socket)
        self.commissioning_profiles = CommissioningProfileStore(self.config_path)
        self.host_metrics_sampler = host_metrics_sampler or HostMetricsSampler()
        self._journal_lock = threading.Lock()
        self._journal_cache: tuple[float, int, list[dict[str, Any]]] | None = None
        self._rate_lock = threading.Lock()
        self._rate_observations: dict[
            tuple[int, int], tuple[int, deque[tuple[int, int]]]
        ] = {}

    def _sample_rate(
        self,
        *,
        channel_id: int,
        node_address: int,
        boot_epoch: int,
        status_monotonic_us: int,
        durable_cursor: int,
    ) -> float | None:
        if status_monotonic_us <= 0:
            return None
        key = (channel_id, node_address)
        with self._rate_lock:
            state = self._rate_observations.get(key)
            if state is None or state[0] != boot_epoch:
                observations: deque[tuple[int, int]] = deque()
                self._rate_observations[key] = (boot_epoch, observations)
            else:
                observations = state[1]

            if observations:
                last_time, last_cursor = observations[-1]
                if status_monotonic_us < last_time or durable_cursor < last_cursor:
                    observations.clear()
                elif status_monotonic_us > last_time:
                    observations.append((status_monotonic_us, durable_cursor))
            else:
                observations.append((status_monotonic_us, durable_cursor))

            cutoff = status_monotonic_us - SAMPLE_RATE_WINDOW_US
            while len(observations) > 2 and observations[1][0] <= cutoff:
                observations.popleft()
            if len(observations) < 2:
                return None
            first_time, first_cursor = observations[0]
            last_time, last_cursor = observations[-1]
            elapsed_us = last_time - first_time
            if elapsed_us <= 0 or last_cursor < first_cursor:
                return None
            return (last_cursor - first_cursor) * 1_000_000.0 / elapsed_us

    def _operations_status(self, channel_id: int) -> dict[str, Any] | None:
        try:
            return self.operations.status(channel_id)
        except OperationsIpcClientError:
            return None

    def operator_capabilities_payload(self) -> dict[str, Any]:
        return {
            "schema_version": 1,
            "operations_transport": "host-v2-unix-datagram",
            "submission": "asynchronous",
            "service_control": self.service_control_status_payload().get("available", False),
            "configuration_workflow": {
                "automatic_mode_transition": True,
                "restore_recording_on_success": True,
                "failure_mode": "maintenance_or_faulted",
            },
            "commissioning_workflow": {
                "single_unassigned_node": True,
                "automatic_maintenance": True,
                "scan_slot_count": 16,
                "scan_progress": "operation_phase_and_elapsed_time",
                "persistent_system_profile": True,
                "restart_after_commit": True,
            },
            "measurement_configuration": {
                "output_rates": [
                    {"code": 0, "hz": "4000"},
                    {"code": 1, "hz": "2000"},
                    {"code": 2, "hz": "1000"},
                    {"code": 3, "hz": "500"},
                    {"code": 4, "hz": "250"},
                    {"code": 5, "hz": "125"},
                    {"code": 6, "hz": "62.5"},
                    {"code": 7, "hz": "31.25"},
                    {"code": 8, "hz": "15.625"},
                    {"code": 9, "hz": "7.8125"},
                    {"code": 10, "hz": "3.90625"},
                ],
                "ranges_g": [2, 4, 8],
                "high_pass_corner": {"minimum": 0, "maximum": 7},
                "filter_profiles": [
                    {"code": 0, "name": "light"},
                    {"code": 1, "name": "balanced"},
                    {"code": 2, "name": "aggressive"},
                ],
                "fifo_watermark": {"minimum": 3, "maximum": 96, "step": 3},
                "offset": {"minimum": -32768, "maximum": 32767},
            },
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

    def service_control_status_payload(self) -> dict[str, Any]:
        try:
            response = self.service_control.status()
        except ServiceControlError as exc:
            return {"available": False, "active": None, "error": str(exc)}
        state = response.get("state")
        if not isinstance(state, dict):
            return {"available": False, "active": None, "error": "invalid service control state"}
        return dict(state)

    def perform_service_action(self, action: str) -> dict[str, Any]:
        if action not in {"start", "stop", "restart"}:
            raise ValueError("unsupported service action")
        return self.service_control.perform(action)

    def register_commissioned_node_payload(
        self,
        channel_id: int,
        payload: dict[str, Any],
    ) -> dict[str, Any]:
        self._validate_channel_id(channel_id)
        required = ("name", "node_address", "hardware_id", "board_revision", "round_nonce")
        for field in required:
            if field not in payload:
                raise ValueError(f"{field} is required for commissioned node")
        node_address = payload["node_address"]
        board_revision = payload["board_revision"]
        round_nonce = payload["round_nonce"]
        if isinstance(node_address, bool) or not isinstance(node_address, int):
            raise ValueError("node_address must be an integer")
        if isinstance(board_revision, bool) or not isinstance(board_revision, int):
            raise ValueError("board_revision must be an integer")
        if isinstance(round_nonce, bool) or not isinstance(round_nonce, int) or round_nonce <= 0:
            raise ValueError("round_nonce must be a positive integer")
        self.commissioning_profiles.validate_node(
            channel_id=channel_id,
            node_name=str(payload["name"]),
            node_address=node_address,
            hardware_id=str(payload["hardware_id"]),
            board_revision=board_revision,
        )
        self.commissioning_profiles.verify_writable()
        service_state = self.service_control_status_payload()
        if service_state.get("available") is not True:
            raise ServiceControlError("host service control is unavailable")
        assignment = self.submit_operation_payload(
            channel_id,
            {
                "operation": "assign_address",
                "round_nonce": round_nonce,
                "hardware_id": str(payload["hardware_id"]),
                "new_node_address": node_address,
            },
        )
        assignment = self._wait_for_operation(channel_id, assignment, timeout_seconds=120.0)
        result = assignment.get("result")
        if not isinstance(result, dict) or result.get("kind") != "address_assigned":
            raise OperationsIpcClientError("host did not confirm the commissioned node address")
        if (
            str(result.get("hardware_id", "")).lower() != str(payload["hardware_id"]).lower()
            or result.get("node_address") != node_address
        ):
            raise OperationsIpcClientError("commissioning confirmation does not match the requested node")
        saved = self.commissioning_profiles.add_node(
            channel_id=channel_id,
            node_name=str(payload["name"]),
            node_address=node_address,
            hardware_id=str(payload["hardware_id"]),
            board_revision=board_revision,
        )
        restart = self.service_control.perform("restart")
        return {
            "ok": True,
            "channel_id": channel_id,
            "assignment": assignment,
            "profile": saved,
            "service": restart.get("state", restart),
        }

    def _wait_for_operation(
        self,
        channel_id: int,
        submitted: dict[str, Any],
        *,
        timeout_seconds: float,
    ) -> dict[str, Any]:
        phase = submitted.get("phase")
        if phase == "succeeded":
            return submitted
        operation_id = submitted.get("operation_id")
        if isinstance(operation_id, bool) or not isinstance(operation_id, int):
            raise OperationsIpcClientError("Operations API did not return an operation_id")
        deadline = time.monotonic() + timeout_seconds
        while time.monotonic() < deadline:
            response = self.operations.get(channel_id, operation_id)
            phase = response.get("phase")
            if phase == "succeeded":
                return response
            if phase in {"failed", "rejected"}:
                raise OperationsIpcClientError(
                    str(response.get("error") or f"commissioning operation {phase}")
                )
            time.sleep(0.25)
        raise OperationsIpcClientError("commissioning operation did not finish before its deadline")

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
        if isinstance(operation_id, str):
            if not operation_id.isascii() or not operation_id.isdecimal():
                raise ValueError("operation_id must be an integer or decimal string")
            operation_id = int(operation_id)
        if isinstance(operation_id, bool) or not isinstance(operation_id, int):
            raise ValueError("operation_id must be an integer or decimal string")
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
        if operation in {"apply_configuration", "reset_configuration"}:
            # The node owns the full persisted configuration, while output ODR
            # is also host scheduling policy.  Preflight the profile before
            # touching the node, then commit its confirmed effective ODR.
            self.commissioning_profiles.verify_writable()
            submitted = self.operations.submit(request)
            completed = self._wait_for_operation(
                channel_id, submitted, timeout_seconds=120.0
            )
            result = completed.get("result")
            observed = result.get("observed") if isinstance(result, dict) else None
            configuration = (
                observed.get("configuration") if isinstance(observed, dict) else None
            )
            if not isinstance(result, dict) or result.get("kind") != "configuration_applied":
                raise OperationsIpcClientError(
                    "host did not confirm the effective node configuration"
                )
            if not isinstance(configuration, dict):
                raise OperationsIpcClientError(
                    "host did not return the effective node configuration"
                )
            output_rate_code = _safe_int(configuration.get("output_rate_code"), -1)
            output_rate_hz = OUTPUT_RATE_HZ_BY_CODE.get(output_rate_code)
            if output_rate_hz is None:
                raise OperationsIpcClientError("host returned an unsupported output rate")
            completed["profile"] = self.commissioning_profiles.update_node_output_rate(
                channel_id=channel_id,
                node_address=_safe_int(payload.get("node_address")),
                output_rate_hz=output_rate_hz,
            )
            return completed
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
        process = status.get("process")
        process = process if isinstance(process, dict) else {}
        updated = _unix_ms_to_iso(process.get("updated_unix_ms"))
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
        health = (status or {}).get("health")
        health = health if isinstance(health, dict) else {}
        health_reasons = health.get("reasons", [])
        process = (status or {}).get("process")
        process = process if isinstance(process, dict) else {}
        status_monotonic_us = _safe_int(process.get("updated_monotonic_us"))
        result: list[dict[str, Any]] = []
        for raw_channel in profile.get("channels", []):
            if not isinstance(raw_channel, dict):
                continue
            channel_id = _safe_int(raw_channel.get("channel_id"))
            runtime = runtime_channels.get(channel_id, {})
            enabled = bool(raw_channel.get("enabled", True))
            phase = str(runtime.get("phase", "offline"))
            metrics = runtime.get("metrics", {}) if isinstance(runtime.get("metrics"), dict) else {}
            running = phase == "running" and not status_stale
            operations_mode = None
            active_operation_id = None
            operations_available = False
            operations_status = None
            if enabled:
                operations_status = self._operations_status(channel_id)
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
            if enabled and phase == "faulted":
                channel_alerts.append("kanał faulted")
            if enabled and status_stale:
                channel_alerts.append("status runtime nieaktualny")
            for reason in health_reasons if isinstance(health_reasons, list) else []:
                if enabled and isinstance(reason, dict) and _safe_int(reason.get("channel_id")) == channel_id:
                    channel_alerts.append(str(reason.get("code", "health reason")))
            nodes: list[dict[str, Any]] = []
            for raw_node in raw_channel.get("nodes", []):
                if not isinstance(raw_node, dict):
                    continue
                address = _safe_int(raw_node.get("id", raw_node.get("node_id")))
                node = runtime_nodes.get((channel_id, address))
                alerts: list[str] = []
                cursors = node.get("cursors") if isinstance(node, dict) else None
                measurement_age_ms = node.get("last_measurement_age_ms") if isinstance(node, dict) else None
                time_sync_age_ms = node.get("last_time_sync_age_ms") if isinstance(node, dict) else None
                online = bool(node is not None and not status_stale)
                measurement_fresh = bool(
                    running
                    and measurement_age_ms is not None
                    and _safe_int(measurement_age_ms, int(STATUS_STALE_AFTER_S * 1000) + 1)
                    <= int(STATUS_STALE_AFTER_S * 1000)
                )
                if node is None:
                    if enabled:
                        alerts.append("brak node w statusie runtime")
                elif cursors is None:
                    alerts.append("cursory niedostępne")
                elif running and not measurement_fresh:
                    alerts.append("brak świeżych próbek")
                received = _safe_int(cursors.get("received")) if isinstance(cursors, dict) else 0
                durable = _safe_int(cursors.get("durable")) if isinstance(cursors, dict) else 0
                released = _safe_int(cursors.get("released")) if isinstance(cursors, dict) else 0
                produced = _safe_int(cursors.get("produced")) if isinstance(cursors, dict) else 0
                measured_rate = (
                    self._sample_rate(
                        channel_id=channel_id,
                        node_address=address,
                        boot_epoch=_safe_int(node.get("boot_epoch")),
                        status_monotonic_us=status_monotonic_us,
                        durable_cursor=durable,
                    )
                    if isinstance(node, dict) and isinstance(cursors, dict) and not status_stale
                    else None
                )
                expected_odr_hz = float(raw_node.get("output_rate_hz", 0))
                output_odr_hz = (
                    float(node.get("output_rate_hz", expected_odr_hz))
                    if isinstance(node, dict)
                    else expected_odr_hz
                )
                if node is not None and abs(output_odr_hz - expected_odr_hz) > 1e-9:
                    alerts.append("ODR node różni się od profilu hosta")
                nodes.append(
                    {
                        "node_id": address,
                        "name": raw_node.get("name"),
                        "enabled": bool(raw_node.get("enabled", True)),
                        "runtime_v2": True,
                        "has_runtime": node is not None,
                        "online": online,
                        "receiving_samples": measurement_fresh,
                        "expected_to_receive": running,
                        "sample_flow_state": (
                            "flowing" if measurement_fresh else "idle" if online and not running else "stalled" if online else "offline"
                        ),
                        "firmware_version": node.get("firmware") if isinstance(node, dict) else None,
                        "hardware_id": node.get("hardware_id") if isinstance(node, dict) else raw_node.get("hardware_id"),
                        "board_revision": node.get("board_revision") if isinstance(node, dict) else None,
                        "boot_epoch": node.get("boot_epoch") if isinstance(node, dict) else None,
                        "application_protocol": node.get("application_protocol") if isinstance(node, dict) else None,
                        "framing_protocol": node.get("framing_protocol") if isinstance(node, dict) else None,
                        "output_odr_hz": output_odr_hz,
                        "expected_odr_hz": expected_odr_hz,
                        "output_rate_mask": _safe_int(node.get("output_rate_mask")) if isinstance(node, dict) else 0,
                        "configuration_revision": _safe_int(node.get("configuration_revision")) if isinstance(node, dict) else 0,
                        "service_flags": _safe_int(node.get("service_flags")) if isinstance(node, dict) else 0,
                        "feature_flags": _safe_int(node.get("feature_flags")) if isinstance(node, dict) else 0,
                        "samples_written": durable,
                        "expected_sample_seq": produced,
                        "instant_samples_per_second_5s": measured_rate,
                        "rate_stability_percent_5s": None,
                        "bursts_no_data": _safe_int(metrics.get("measurement_no_data")),
                        "received_cursor": received,
                        "durable_cursor": durable,
                        "released_cursor": released,
                        "produced_cursor": produced,
                        "node_backlog": max(0, produced - received),
                        "persistence_backlog": max(0, received - durable),
                        "release_backlog": max(0, durable - released),
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
                    "runtime_v2": True,
                    "label": raw_channel.get("label"),
                    "channel_id": channel_id,
                    "enabled": enabled,
                    "desired_running": enabled,
                    "controls_enabled": operations_available,
                    "control_state": operations_mode or "unavailable",
                    "operation_mode": operations_mode,
                    "active_operation_id": active_operation_id,
                    "running": running,
                    "health": health,
                    "phase": phase,
                    "operating_mode": operations_mode,
                    "port": transport.get("endpoint", "-"),
                    "baud": _safe_int(transport.get("baud_rate")),
                    "updated_utc": updated_utc,
                    "attention_count": len(channel_alerts) + sum(len(node["alerts"]) for node in nodes),
                    "completed_steps": _safe_int(runtime.get("completed_steps")),
                    "alerts": channel_alerts,
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
        events: list[dict[str, Any]] = []
        enabled_channels = [channel for channel in channels if channel["enabled"]]
        enabled_nodes = [node for channel in enabled_channels for node in channel["nodes"] if node["enabled"]]
        running_channels = sum(1 for channel in enabled_channels if channel["running"])
        online_nodes = sum(1 for node in enabled_nodes if node["online"])
        receiving_nodes = sum(1 for node in enabled_nodes if node["receiving_samples"])
        attention_count = sum(channel["attention_count"] for channel in enabled_channels)
        storage = (status or {}).get("storage")
        storage = storage if isinstance(storage, dict) else {}
        process = (status or {}).get("process")
        process = process if isinstance(process, dict) else {}
        health = (status or {}).get("health")
        health = health if isinstance(health, dict) else {}
        software = (status or {}).get("software")
        software = software if isinstance(software, dict) else {}
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
            "nodes_receiving_samples": receiving_nodes,
            "nodes_without_samples": sum(1 for node in enabled_nodes if node["expected_to_receive"] and not node["receiving_samples"]),
            "samples_written_total": sum(_safe_int(node["durable_cursor"]) for node in enabled_nodes),
            "attention_count": attention_count + (1 if stale else 0),
            "events_by_severity": severity_counts,
            "status_age_s": age_s,
            "status_stale": stale,
            "storage_free_percent": None,
            "storage_low": str(storage.get("state", "unknown")) != "healthy",
            "process_phase": phase,
            "health_level": str(health.get("level", "unknown")),
            "status_summary": summary,
        }
        config = self.config_payload()
        host_metrics = self.host_metrics_sampler.sample().to_dict()
        supervisor = {
            "has_status": status is not None,
            "status_file": str(self.status_path),
            "event_log": f"journald:{self.service_unit}",
            "updated_utc": updated_utc,
            "started_utc": _unix_ms_to_iso(process.get("started_unix_ms")),
            "supervisor_version": software.get("host_version"),
            "storage_root": storage.get("root", config["storage"].get("root_dir")),
            "status_age_s": age_s,
            "status_stale": stale,
            "storage_total_bytes": None,
            "storage_free_bytes": storage.get("available_bytes"),
            "storage_used_percent": None,
            "storage_low": overview["storage_low"],
            "storage_minimum_available_bytes": storage.get("minimum_available_bytes"),
            "process_phase": phase,
            "health_level": overview["health_level"],
            "coordinator_state": health.get("coordinator"),
            "time_persistence_state": health.get("time_persistence"),
            "process_id": process.get("pid"),
            "process_instance_id": process.get("instance_id"),
            "process_uptime_ms": process.get("uptime_ms"),
            "status_sequence": (status or {}).get("sequence"),
            "application_protocol": software.get("application_protocol"),
            "framing_protocol": software.get("framing_protocol"),
            **host_metrics,
        }
        return {
            "dashboard_version": "v2-observer-1",
            "dashboard_mode": "host-v2-operations",
            "service_control": self.service_control_status_payload(),
            "read_only": True,
            "operator_api_available": any(
                channel.get("controls_enabled", False) for channel in channels
            ),
            "data_available": False,
            "live_preview_available": False,
            "logs_available": False,
            "events_available": False,
            "config_available": False,
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
            process = status.get("process")
            process = process if isinstance(process, dict) else {}
            health = status.get("health")
            health = health if isinstance(health, dict) else {}
            utc = _unix_ms_to_iso(process.get("updated_unix_ms"))
            for reason in health.get("reasons", []):
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
