from __future__ import annotations

import argparse
import json
import mimetypes
import tarfile
import tempfile
import subprocess
import struct
import time
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, quote, unquote, urlparse

from host.common.system_config import HostSystemConfig
from host.dashboard.app import DashboardRepository, clamp_limit, load_json, load_tail_jsonl
from host.host_configurator import (
    CMD_GET_CONFIG,
    CMD_LOAD_CONFIG,
    CMD_RESET_CONFIG_TO_DEFAULTS,
    CMD_SAVE_CONFIG,
    CMD_SET_BAUD_RATE,
    CMD_SET_FIFO_WATERMARK,
    CMD_SET_HIGH_PASS,
    CMD_SET_NODE_ID,
    CMD_SET_ODR,
    CMD_SET_OFFSETS,
    CMD_SET_RANGE,
    ConfigView,
    HostConfig,
    ProtocolClient,
    SUPPORTED_BAUD_RATES,
    SUPPORTED_FIFO_WATERMARKS,
    SUPPORTED_HIGH_PASS_CORNERS,
    SUPPORTED_ODR_HZ,
    effective_output_odr_hz,
    parse_config_view,
    send_and_wait,
    sync_system_config_from_device_config,
)

PROJECT_ROOT = Path(__file__).resolve().parents[2]
PANEL_VERSION = "0.1.0"
MAX_PREVIEW_LIMIT = 4_096
MAX_RUN_ITEMS = 500


class ApiError(RuntimeError):
    def __init__(self, status: HTTPStatus, message: str) -> None:
        super().__init__(message)
        self.status = status
        self.message = message


@dataclass(frozen=True)
class SupervisorControlState:
    available: bool
    controller: str
    service_name: str | None
    active: bool | None
    substate: str | None
    unit_file_state: str | None
    description: str | None
    pid: int | None
    message: str | None = None


@dataclass(frozen=True)
class FileDownload:
    path: Path
    media_type: str
    download_name: str
    cleanup_path: Path | None = None


class SystemConfigStore:
    def __init__(self, path: str | Path) -> None:
        self.path = Path(path)

    def load_raw(self) -> dict[str, Any]:
        return json.loads(self.path.read_text(encoding="utf-8"))

    def save_raw(self, payload: dict[str, Any]) -> dict[str, Any]:
        HostSystemConfig.from_dict(payload)
        self.path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
        return payload


class SystemdSupervisorController:
    def __init__(self, service_name: str, timeout_s: float = 8.0) -> None:
        self.service_name = service_name
        self.timeout_s = timeout_s

    def _run(self, *args: str) -> subprocess.CompletedProcess[str]:
        try:
            return subprocess.run(
                ["systemctl", *args, self.service_name],
                capture_output=True,
                text=True,
                timeout=self.timeout_s,
                check=False,
            )
        except FileNotFoundError as exc:
            raise ApiError(HTTPStatus.SERVICE_UNAVAILABLE, "systemctl is not available on this host") from exc
        except subprocess.TimeoutExpired as exc:
            raise ApiError(HTTPStatus.GATEWAY_TIMEOUT, "systemctl command timed out") from exc

    def status(self) -> SupervisorControlState:
        try:
            result = subprocess.run(
                [
                    "systemctl",
                    "show",
                    "--property=ActiveState,SubState,UnitFileState,Description,ExecMainPID",
                    self.service_name,
                ],
                capture_output=True,
                text=True,
                timeout=self.timeout_s,
                check=False,
            )
        except FileNotFoundError:
            return SupervisorControlState(
                available=False,
                controller="systemd",
                service_name=self.service_name,
                active=None,
                substate=None,
                unit_file_state=None,
                description=None,
                pid=None,
                message="systemctl is not available on this host",
            )
        except subprocess.TimeoutExpired:
            return SupervisorControlState(
                available=False,
                controller="systemd",
                service_name=self.service_name,
                active=None,
                substate=None,
                unit_file_state=None,
                description=None,
                pid=None,
                message="systemctl status timed out",
            )

        if result.returncode != 0:
            message = (result.stderr or result.stdout or "systemctl show failed").strip()
            return SupervisorControlState(
                available=False,
                controller="systemd",
                service_name=self.service_name,
                active=None,
                substate=None,
                unit_file_state=None,
                description=None,
                pid=None,
                message=message,
            )

        fields: dict[str, str] = {}
        for line in result.stdout.splitlines():
            if "=" not in line:
                continue
            key, value = line.split("=", 1)
            fields[key] = value
        active_state = fields.get("ActiveState", "")
        substate = fields.get("SubState", "")
        unit_file_state = fields.get("UnitFileState", "")
        description = fields.get("Description", "")
        pid_raw = fields.get("ExecMainPID", "")
        pid = None
        if pid_raw.strip().isdigit():
            parsed_pid = int(pid_raw.strip())
            pid = parsed_pid if parsed_pid > 0 else None
        return SupervisorControlState(
            available=True,
            controller="systemd",
            service_name=self.service_name,
            active=active_state.strip() == "active",
            substate=substate.strip() or None,
            unit_file_state=unit_file_state.strip() or None,
            description=description.strip() or None,
            pid=pid,
        )

    def perform(self, action: str) -> SupervisorControlState:
        if action not in {"start", "stop", "restart"}:
            raise ApiError(HTTPStatus.BAD_REQUEST, f"unsupported supervisor action '{action}'")
        result = self._run(action)
        if result.returncode != 0:
            message = (result.stderr or result.stdout or f"systemctl {action} failed").strip()
            raise ApiError(HTTPStatus.BAD_GATEWAY, message)
        return self.status()


class RunsRepository:
    def __init__(self, config_store: SystemConfigStore) -> None:
        self.config_store = config_store

    def root_path(self) -> Path:
        raw = self.config_store.load_raw()
        config = HostSystemConfig.from_dict(raw)
        root = Path(config.storage.root_dir)
        if not root.is_absolute():
            root = PROJECT_ROOT / root
        return root.resolve()

    def _resolve(self, raw_relative: str | None) -> Path:
        root = self.root_path()
        target = root
        if raw_relative:
            target = (root / unquote(raw_relative)).resolve()
        if root != target and root not in target.parents:
            raise ApiError(HTTPStatus.BAD_REQUEST, "path escapes runs root")
        return target

    def list(self, raw_relative: str | None) -> dict[str, Any]:
        root = self.root_path()
        target = self._resolve(raw_relative)
        if not root.exists():
            return {
                "root": str(root),
                "relative_path": ".",
                "exists": False,
                "items": [],
            }
        if not target.exists():
            raise ApiError(HTTPStatus.NOT_FOUND, "requested runs path does not exist")
        if not target.is_dir():
            raise ApiError(HTTPStatus.BAD_REQUEST, "requested runs path is not a directory")

        items: list[dict[str, Any]] = []
        for child in sorted(target.iterdir(), key=lambda entry: (not entry.is_dir(), entry.name.lower()))[:MAX_RUN_ITEMS]:
            stat = child.stat()
            relative = child.relative_to(root).as_posix()
            items.append(
                {
                    "name": child.name,
                    "relative_path": relative,
                    "type": "directory" if child.is_dir() else "file",
                    "size_bytes": stat.st_size,
                    "modified_utc": datetime.fromtimestamp(stat.st_mtime, tz=timezone.utc).isoformat(),
                    "download_url": f"/api/runs/download?path={quote(relative)}",
                }
            )

        relative_path = "." if target == root else target.relative_to(root).as_posix()
        parent = None
        if target != root:
            parent_path = target.parent.relative_to(root)
            parent = "." if str(parent_path) == "." else parent_path.as_posix()
        return {
            "root": str(root),
            "relative_path": relative_path,
            "parent_relative_path": parent,
            "exists": True,
            "items": items,
        }

    def download(self, raw_relative: str | None) -> FileDownload:
        if not raw_relative:
            raise ApiError(HTTPStatus.BAD_REQUEST, "missing runs file path")
        target = self._resolve(raw_relative)
        if not target.exists():
            raise ApiError(HTTPStatus.NOT_FOUND, "requested runs file does not exist")
        if target.is_dir():
            return self._archive_directory(target)
        media_type = mimetypes.guess_type(target.name)[0] or "application/octet-stream"
        return FileDownload(path=target, media_type=media_type, download_name=target.name)

    def _archive_directory(self, target: Path) -> FileDownload:
        temp_handle = tempfile.NamedTemporaryFile(
            prefix=f"{target.name}_",
            suffix=".tar.gz",
            delete=False,
        )
        temp_handle.close()
        archive_path = Path(temp_handle.name)
        try:
            with tarfile.open(archive_path, "w:gz") as archive:
                archive.add(target, arcname=target.name)
        except Exception:
            archive_path.unlink(missing_ok=True)
            raise
        return FileDownload(
            path=archive_path,
            media_type="application/gzip",
            download_name=f"{target.name}.tar.gz",
            cleanup_path=archive_path,
        )


def tail_text(path: Path, limit: int) -> list[str]:
    if not path.exists() or limit <= 0:
        return []
    try:
        with path.open("rb") as handle:
            handle.seek(0, 2)
            pos = handle.tell()
            buffer = b""
            lines: list[bytes] = []
            while pos > 0 and len(lines) <= limit:
                read_size = min(4096, pos)
                pos -= read_size
                handle.seek(pos)
                buffer = handle.read(read_size) + buffer
                lines = buffer.splitlines()
    except OSError:
        return []
    return [line.decode("utf-8", errors="replace") for line in lines[-limit:] if line.strip()]


class PreviewReader:
    def __init__(self, config_store: SystemConfigStore) -> None:
        self.config_store = config_store

    def _resolve_channel_status(self, channel_name: str) -> tuple[HostSystemConfig, dict[str, Any] | None]:
        raw = self.config_store.load_raw()
        config = HostSystemConfig.from_dict(raw)
        for channel in config.channels:
            if channel.name == channel_name:
                status = load_json(Path(config.supervisor.status_file))
                return config, status
        raise ApiError(HTTPStatus.NOT_FOUND, f"unknown channel '{channel_name}'")

    def _resolve_data_file(self, channel_name: str) -> Path:
        config, status = self._resolve_channel_status(channel_name)
        if status:
            for channel in status.get("channels", []):
                if channel.get("name") != channel_name:
                    continue
                active_file = channel.get("active_file")
                if active_file:
                    path = Path(str(active_file))
                    if not path.is_absolute():
                        path = PROJECT_ROOT / path
                    if path.exists():
                        return path

        root = Path(config.storage.root_dir)
        if not root.is_absolute():
            root = PROJECT_ROOT / root
        candidates = sorted(root.rglob("*.h5"), key=lambda item: item.stat().st_mtime, reverse=True)
        if not candidates:
            raise ApiError(HTTPStatus.NOT_FOUND, "no HDF5 files found under runs root")
        return candidates[0]

    def read(self, channel_name: str, node_id: int, limit: int) -> dict[str, Any]:
        limit_value = max(16, min(limit, MAX_PREVIEW_LIMIT))
        file_path = self._resolve_data_file(channel_name)
        try:
            import h5py  # type: ignore
        except ImportError as exc:
            raise ApiError(HTTPStatus.SERVICE_UNAVAILABLE, "h5py is required for preview support") from exc

        try:
            with h5py.File(file_path, "r") as handle:
                dataset = handle[f"nodes/{node_id}/samples"]
                total = int(dataset.shape[0])
                start = max(0, total - limit_value)
                records = dataset[start:total]
                attrs = dict(handle[f"nodes/{node_id}"].attrs.items())
        except KeyError as exc:
            raise ApiError(HTTPStatus.NOT_FOUND, f"node {node_id} does not exist in {file_path.name}") from exc
        except OSError as exc:
            raise ApiError(HTTPStatus.BAD_GATEWAY, f"cannot open preview source '{file_path}': {exc}") from exc

        samples = [
            {
                "sample_seq": int(record["sample_seq"]),
                "x": float(record["x"]),
                "y": float(record["y"]),
                "z": float(record["z"]),
                "packet_seq": int(record["packet_seq"]),
            }
            for record in records
        ]
        return {
            "channel_name": channel_name,
            "node_id": node_id,
            "source_file": str(file_path),
            "sample_count": len(samples),
            "samples_total": total,
            "sensor_odr_hz": int(attrs.get("sensor_odr_hz", 0)),
            "output_odr_hz": float(attrs.get("output_odr_hz", 0.0)),
            "range_g": int(attrs.get("range_g", 0)),
            "samples": samples,
        }


class NodeDeviceService:
    def __init__(self, config_store: SystemConfigStore) -> None:
        self.config_store = config_store

    def _channel(self, channel_name: str):
        config = HostSystemConfig.load(self.config_store.path)
        for channel in config.channels:
            if channel.name == channel_name:
                return channel
        raise ApiError(HTTPStatus.NOT_FOUND, f"unknown channel '{channel_name}'")

    def _open(self, channel_name: str):
        try:
            import serial
        except ImportError as exc:
            raise ApiError(HTTPStatus.SERVICE_UNAVAILABLE, "pyserial is required for node control") from exc

        channel = self._channel(channel_name)
        host_config = HostConfig(
            port=channel.port,
            baud=channel.baud,
            node=channel.nodes[0].node_id,
            timeout=2.0,
        )
        try:
            serial_port = serial.Serial(
                port=host_config.port,
                baudrate=host_config.baud,
                timeout=0.05,
                write_timeout=0.5,
            )
        except serial.SerialException as exc:
            raise ApiError(HTTPStatus.BAD_GATEWAY, f"cannot open serial port {host_config.port}: {exc}") from exc
        return channel, host_config, serial_port, ProtocolClient(serial_port)

    def _config_payload(self, channel_name: str, config: ConfigView) -> dict[str, Any]:
        return {
            "channel_name": channel_name,
            "node_id": config.node_id,
            "baudrate": config.baudrate,
            "sensor_odr_hz": config.odr_hz,
            "output_odr_hz": effective_output_odr_hz(config.odr_hz),
            "range_g": config.range_g,
            "high_pass_corner": config.high_pass_corner,
            "offset_x": config.offset_x,
            "offset_y": config.offset_y,
            "offset_z": config.offset_z,
            "fifo_watermark": config.fifo_watermark,
            "act_threshold": config.act_threshold,
            "act_count": config.act_count,
        }

    def read_config(self, channel_name: str, node_id: int) -> dict[str, Any]:
        channel, host_config, serial_port, client = self._open(channel_name)
        del channel
        try:
            response = send_and_wait(client, node_id, bytes([CMD_GET_CONFIG]), host_config.timeout)
            return self._config_payload(channel_name, parse_config_view(response.payload))
        except RuntimeError as exc:
            raise ApiError(HTTPStatus.BAD_GATEWAY, str(exc)) from exc
        finally:
            serial_port.close()

    def _sync_system_config(self, port: str, previous_node_id: int, updated: ConfigView) -> None:
        try:
            sync_system_config_from_device_config(
                self.config_store.path,
                port=port,
                previous_node_id=previous_node_id,
                updated=updated,
            )
        except RuntimeError as exc:
            raise ApiError(HTTPStatus.BAD_GATEWAY, str(exc)) from exc

    def _validated_int(self, payload: dict[str, Any], key: str) -> int | None:
        if key not in payload or payload[key] is None:
            return None
        value = payload[key]
        if not isinstance(value, int):
            raise ApiError(HTTPStatus.BAD_REQUEST, f"field '{key}' must be an integer")
        return value

    def apply_config(self, channel_name: str, node_id: int, payload: dict[str, Any]) -> dict[str, Any]:
        channel, host_config, serial_port, client = self._open(channel_name)
        current_node_id = node_id
        original_node_id = node_id
        persist = bool(payload.get("persist", True))

        try:
            initial_response = send_and_wait(client, current_node_id, bytes([CMD_GET_CONFIG]), host_config.timeout)
            current = parse_config_view(initial_response.payload)

            next_node_id = self._validated_int(payload, "node_id")
            sensor_odr_hz = self._validated_int(payload, "sensor_odr_hz")
            range_g = self._validated_int(payload, "range_g")
            high_pass_corner = self._validated_int(payload, "high_pass_corner")
            fifo_watermark = self._validated_int(payload, "fifo_watermark")
            baudrate = self._validated_int(payload, "baudrate")
            offset_x = self._validated_int(payload, "offset_x")
            offset_y = self._validated_int(payload, "offset_y")
            offset_z = self._validated_int(payload, "offset_z")

            if next_node_id is not None and next_node_id != current.node_id:
                send_and_wait(client, current_node_id, struct.pack("<BB", CMD_SET_NODE_ID, next_node_id), host_config.timeout)
                current_node_id = next_node_id

            if sensor_odr_hz is not None:
                if sensor_odr_hz not in SUPPORTED_ODR_HZ:
                    raise ApiError(HTTPStatus.BAD_REQUEST, "unsupported sensor ODR")
                send_and_wait(client, current_node_id, struct.pack("<BH", CMD_SET_ODR, sensor_odr_hz), host_config.timeout)

            if range_g is not None:
                send_and_wait(client, current_node_id, struct.pack("<BB", CMD_SET_RANGE, range_g), host_config.timeout)

            if high_pass_corner is not None:
                if high_pass_corner not in SUPPORTED_HIGH_PASS_CORNERS:
                    raise ApiError(HTTPStatus.BAD_REQUEST, "unsupported high-pass corner")
                send_and_wait(client, current_node_id, struct.pack("<BB", CMD_SET_HIGH_PASS, high_pass_corner), host_config.timeout)

            if any(value is not None for value in (offset_x, offset_y, offset_z)):
                send_and_wait(
                    client,
                    current_node_id,
                    struct.pack(
                        "<Biii",
                        CMD_SET_OFFSETS,
                        offset_x if offset_x is not None else current.offset_x,
                        offset_y if offset_y is not None else current.offset_y,
                        offset_z if offset_z is not None else current.offset_z,
                    ),
                    host_config.timeout,
                )

            if fifo_watermark is not None:
                if fifo_watermark not in SUPPORTED_FIFO_WATERMARKS:
                    raise ApiError(HTTPStatus.BAD_REQUEST, "unsupported FIFO watermark")
                send_and_wait(
                    client,
                    current_node_id,
                    struct.pack("<BB", CMD_SET_FIFO_WATERMARK, fifo_watermark),
                    host_config.timeout,
                )

            if baudrate is not None:
                if baudrate not in SUPPORTED_BAUD_RATES:
                    raise ApiError(HTTPStatus.BAD_REQUEST, "unsupported baudrate")
                send_and_wait(client, current_node_id, struct.pack("<BI", CMD_SET_BAUD_RATE, baudrate), host_config.timeout)
                client.rx_buffer.clear()
                time.sleep(0.1)
                serial_port.baudrate = baudrate

            if persist:
                send_and_wait(client, current_node_id, bytes([CMD_SAVE_CONFIG]), host_config.timeout)

            final_response = send_and_wait(client, current_node_id, bytes([CMD_GET_CONFIG]), host_config.timeout)
            final = parse_config_view(final_response.payload)
            self._sync_system_config(channel.port, original_node_id, final)
            return {
                "persisted": persist,
                "config": self._config_payload(channel_name, final),
            }
        except RuntimeError as exc:
            raise ApiError(HTTPStatus.BAD_GATEWAY, str(exc)) from exc
        finally:
            serial_port.close()

    def load_config(self, channel_name: str, node_id: int) -> dict[str, Any]:
        channel, host_config, serial_port, client = self._open(channel_name)
        try:
            send_and_wait(client, node_id, bytes([CMD_LOAD_CONFIG]), host_config.timeout)
            response = send_and_wait(client, node_id, bytes([CMD_GET_CONFIG]), host_config.timeout)
            updated = parse_config_view(response.payload)
            self._sync_system_config(channel.port, node_id, updated)
            return self._config_payload(channel_name, updated)
        except RuntimeError as exc:
            raise ApiError(HTTPStatus.BAD_GATEWAY, str(exc)) from exc
        finally:
            serial_port.close()

    def reset_defaults(self, channel_name: str, node_id: int) -> dict[str, Any]:
        channel, host_config, serial_port, client = self._open(channel_name)
        try:
            send_and_wait(client, node_id, bytes([CMD_RESET_CONFIG_TO_DEFAULTS]), host_config.timeout)
            response = send_and_wait(client, node_id, bytes([CMD_GET_CONFIG]), host_config.timeout)
            updated = parse_config_view(response.payload)
            self._sync_system_config(channel.port, node_id, updated)
            return self._config_payload(channel_name, updated)
        except RuntimeError as exc:
            raise ApiError(HTTPStatus.BAD_GATEWAY, str(exc)) from exc
        finally:
            serial_port.close()

    def save_config(self, channel_name: str, node_id: int) -> dict[str, Any]:
        channel, host_config, serial_port, client = self._open(channel_name)
        del channel
        try:
            send_and_wait(client, node_id, bytes([CMD_SAVE_CONFIG]), host_config.timeout)
            response = send_and_wait(client, node_id, bytes([CMD_GET_CONFIG]), host_config.timeout)
            return self._config_payload(channel_name, parse_config_view(response.payload))
        except RuntimeError as exc:
            raise ApiError(HTTPStatus.BAD_GATEWAY, str(exc)) from exc
        finally:
            serial_port.close()


class OperatorApplication:
    def __init__(
        self,
        config_path: str | Path,
        supervisor_controller: SystemdSupervisorController,
        *,
        default_event_limit: int = 40,
        config_store: SystemConfigStore | None = None,
        runs_repository: RunsRepository | None = None,
        preview_reader: PreviewReader | None = None,
        node_service: NodeDeviceService | None = None,
    ) -> None:
        self.config_path = Path(config_path)
        self.dashboard = DashboardRepository(self.config_path, default_event_limit=default_event_limit)
        self.config_store = config_store or SystemConfigStore(self.config_path)
        self.supervisor_controller = supervisor_controller
        self.runs_repository = runs_repository or RunsRepository(self.config_store)
        self.preview_reader = preview_reader or PreviewReader(self.config_store)
        self.node_service = node_service or NodeDeviceService(self.config_store)
        self.default_event_limit = default_event_limit

    def _channel_is_running(self, channel_name: str) -> bool:
        dashboard = self.dashboard.dashboard_payload(limit=1)
        for channel in dashboard.get("channels", []):
            if str(channel.get("name")) == channel_name:
                return bool(channel.get("running", False))
        return False

    def _ensure_channel_is_idle(self, channel_name: str) -> None:
        if self._channel_is_running(channel_name):
            raise ApiError(
                HTTPStatus.CONFLICT,
                f"channel '{channel_name}' is recording; stop the supervisor before opening device config",
            )

    def meta_payload(self) -> dict[str, Any]:
        supervisor = asdict(self.supervisor_controller.status())
        return {
            "version": PANEL_VERSION,
            "config_path": str(self.config_path),
            "pages": [
                {"path": "/", "label": "Przeglad"},
                {"path": "/logs", "label": "Logi"},
                {"path": "/runs", "label": "Runs"},
            ],
            "supervisor": supervisor,
        }

    def dashboard_payload(self, limit: int) -> dict[str, Any]:
        return self.dashboard.dashboard_payload(limit=limit)

    def system_config_payload(self) -> dict[str, Any]:
        return {
            "path": str(self.config_store.path),
            "config": self.config_store.load_raw(),
        }

    def update_system_config(self, payload: dict[str, Any]) -> dict[str, Any]:
        if "config" not in payload or not isinstance(payload["config"], dict):
            raise ApiError(HTTPStatus.BAD_REQUEST, "request body must contain a 'config' object")
        saved = self.config_store.save_raw(payload["config"])
        return {"path": str(self.config_store.path), "config": saved}

    def supervisor_payload(self) -> dict[str, Any]:
        service = asdict(self.supervisor_controller.status())
        dashboard = self.dashboard.dashboard_payload(limit=5)
        runtime_channels_running = int(dashboard.get("overview", {}).get("channels_running", 0))
        runtime_active = runtime_channels_running > 0
        service["runtime_channels_running"] = runtime_channels_running
        service["runtime_active"] = runtime_active
        service["effective_active"] = bool(service.get("active")) or runtime_active
        return service

    def supervisor_action(self, action: str) -> dict[str, Any]:
        self.supervisor_controller.perform(action)
        return self.supervisor_payload()

    def runs_payload(self, raw_relative: str | None) -> dict[str, Any]:
        return self.runs_repository.list(raw_relative)

    def logs_payload(self, limit: int, channel_name: str | None = None) -> dict[str, Any]:
        limit_value = max(10, min(limit, 500))
        dashboard = self.dashboard.dashboard_payload(limit=limit_value)
        config = HostSystemConfig.load(self.config_store.path)
        supervisor_events = load_tail_jsonl(Path(config.supervisor.event_log), limit_value)
        runtime_dir = Path(config.supervisor.channel_runtime_dir)

        channel_logs: list[dict[str, Any]] = []
        for channel in dashboard.get("channels", []):
            name = str(channel.get("name"))
            if channel_name and channel_name != name:
                continue
            process_log_path = channel.get("process_log")
            if not process_log_path:
                process_log_path = str(runtime_dir / f"{name}.process.log")
            lines = tail_text(Path(process_log_path), limit_value) if process_log_path else []
            channel_logs.append(
                {
                    "name": name,
                    "label": channel.get("label"),
                    "process_log": process_log_path,
                    "running": bool(channel.get("running", False)),
                    "lines": lines,
                }
            )

        return {
            "limit": limit_value,
            "supervisor_event_log": config.supervisor.event_log,
            "supervisor_events": supervisor_events,
            "channels": channel_logs,
        }

    def preview_payload(self, channel_name: str, node_id: int, limit: int) -> dict[str, Any]:
        return self.preview_reader.read(channel_name, node_id, limit)

    def device_config_payload(self, channel_name: str, node_id: int) -> dict[str, Any]:
        self._ensure_channel_is_idle(channel_name)
        return self.node_service.read_config(channel_name, node_id)

    def apply_device_config(self, channel_name: str, node_id: int, payload: dict[str, Any]) -> dict[str, Any]:
        self._ensure_channel_is_idle(channel_name)
        return self.node_service.apply_config(channel_name, node_id, payload)

    def load_device_config(self, channel_name: str, node_id: int) -> dict[str, Any]:
        self._ensure_channel_is_idle(channel_name)
        return self.node_service.load_config(channel_name, node_id)

    def reset_device_config(self, channel_name: str, node_id: int) -> dict[str, Any]:
        self._ensure_channel_is_idle(channel_name)
        return self.node_service.reset_defaults(channel_name, node_id)

    def save_device_config(self, channel_name: str, node_id: int) -> dict[str, Any]:
        self._ensure_channel_is_idle(channel_name)
        return self.node_service.save_config(channel_name, node_id)


def page_template(*, title: str, active: str, body: str, script: str = "") -> str:
    nav = [
        ("/", "Przeglad"),
        ("/logs", "Logi"),
        ("/runs", "Runs"),
    ]
    nav_html = "".join(
        f'<a class="nav-link{" active" if path == active else ""}" href="{path}">{label}</a>'
        for path, label in nav
    )
    return f"""<!doctype html>
<html lang="pl">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>{title}</title>
    <style>
      :root {{
        --bg: #f4efe7;
        --ink: #20170f;
        --muted: #6a5848;
        --panel: rgba(255, 252, 247, 0.92);
        --line: rgba(67, 47, 22, 0.12);
        --good: #1d7a58;
        --warn: #a26716;
        --bad: #b03f31;
        --accent: #1b5f92;
        --radius: 18px;
        --shadow: 0 18px 40px rgba(70, 46, 21, 0.12);
      }}
      * {{ box-sizing: border-box; }}
      body {{
        margin: 0;
        min-height: 100vh;
        color: var(--ink);
        font-family: "Avenir Next", "Segoe UI", sans-serif;
        background:
          radial-gradient(circle at top left, rgba(235, 191, 118, 0.28), transparent 26rem),
          radial-gradient(circle at top right, rgba(64, 121, 166, 0.18), transparent 22rem),
          linear-gradient(180deg, #f5efe7 0%, #fbf8f3 54%, #f1e9dc 100%);
      }}
      .shell {{ width: min(1220px, calc(100vw - 28px)); margin: 24px auto 40px; }}
      .hero, .panel {{ background: var(--panel); border: 1px solid var(--line); border-radius: 24px; box-shadow: var(--shadow); }}
      .hero {{ padding: 24px; display: grid; gap: 14px; }}
      .hero h1 {{ margin: 0; font-family: "Iowan Old Style", Georgia, serif; font-size: clamp(32px, 5vw, 54px); line-height: 1; }}
      .hero p {{ margin: 0; color: var(--muted); max-width: 70ch; line-height: 1.55; }}
      .nav {{ display: flex; flex-wrap: wrap; gap: 8px; }}
      .nav-link {{
        padding: 10px 14px;
        border-radius: 999px;
        text-decoration: none;
        color: var(--ink);
        border: 1px solid rgba(57, 41, 23, 0.14);
        background: rgba(255, 255, 255, 0.62);
      }}
      .nav-link.active {{ background: rgba(233, 239, 245, 0.95); border-color: rgba(27, 95, 146, 0.22); }}
      .grid {{ display: grid; gap: 14px; margin-top: 16px; }}
      .grid.two {{ grid-template-columns: repeat(2, minmax(0, 1fr)); }}
      .panel {{ padding: 18px; }}
      .panel h2 {{ margin: 0 0 12px; font-size: 14px; text-transform: uppercase; letter-spacing: 0.11em; color: var(--muted); }}
      .btn {{
        border: 1px solid rgba(53, 38, 20, 0.14);
        border-radius: 999px;
        background: rgba(255, 255, 255, 0.78);
        padding: 10px 14px;
        font: inherit;
        cursor: pointer;
      }}
      .btn.primary {{ background: rgba(28, 95, 146, 0.12); border-color: rgba(28, 95, 146, 0.22); }}
      .btn.warn {{ background: rgba(176, 63, 49, 0.10); border-color: rgba(176, 63, 49, 0.18); }}
      .btn-row {{ display: flex; flex-wrap: wrap; gap: 10px; }}
      .card-list {{ display: grid; gap: 10px; }}
      .card {{
        border: 1px solid var(--line);
        border-radius: 16px;
        padding: 14px;
        background: rgba(255, 255, 255, 0.68);
      }}
      .chip {{ display: inline-flex; align-items: center; gap: 6px; padding: 6px 10px; border-radius: 999px; font-size: 12px; font-weight: 600; }}
      .chip.good {{ color: var(--good); background: rgba(29, 122, 88, 0.10); }}
      .chip.warn {{ color: var(--warn); background: rgba(162, 103, 22, 0.10); }}
      .chip.bad {{ color: var(--bad); background: rgba(176, 63, 49, 0.10); }}
      .chip.info {{ color: var(--accent); background: rgba(27, 95, 146, 0.10); }}
      .metric {{ font-size: 28px; font-weight: 700; }}
      .muted {{ color: var(--muted); }}
      .mono, pre, textarea, code {{ font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }}
      pre {{
        margin: 0;
        padding: 14px;
        border-radius: 14px;
        background: #17120d;
        color: #f8f3ea;
        overflow: auto;
      }}
      textarea {{
        width: 100%;
        min-height: 420px;
        border-radius: 14px;
        border: 1px solid var(--line);
        background: #fffdfa;
        padding: 14px;
        font-size: 13px;
      }}
      input, select {{
        width: 100%;
        border-radius: 12px;
        border: 1px solid var(--line);
        background: rgba(255,255,255,0.9);
        padding: 10px 12px;
        font: inherit;
      }}
      label {{ display: grid; gap: 6px; font-size: 13px; color: var(--muted); }}
      .form-grid {{ display: grid; gap: 10px; grid-template-columns: repeat(2, minmax(0, 1fr)); }}
      .wide {{ grid-column: 1 / -1; }}
      table {{ width: 100%; border-collapse: collapse; }}
      th, td {{ text-align: left; padding: 10px 8px; border-bottom: 1px solid var(--line); font-size: 14px; }}
      canvas {{ width: 100%; height: 300px; display: block; background: #120f0b; border-radius: 14px; }}
      @media (max-width: 900px) {{
        .grid.two, .form-grid {{ grid-template-columns: 1fr; }}
      }}
    </style>
  </head>
  <body>
    <div class="shell">
      <section class="hero">
        <div class="nav">{nav_html}</div>
        <h1>{title}</h1>
      </section>
      {body}
    </div>
    <script>
      async function fetchJson(url, options) {{
        const response = await fetch(url, options);
        const text = await response.text();
        let payload = {{}};
        try {{
          payload = text ? JSON.parse(text) : {{}};
        }} catch (error) {{
          throw new Error(text || "Niepoprawna odpowiedz serwera");
        }}
        if (!response.ok) {{
          throw new Error(payload.error || ('HTTP ' + response.status));
        }}
        return payload;
      }}
      function statusChip(value) {{
        if (value === true) return '<span class="chip good">aktywny</span>';
        if (value === false) return '<span class="chip warn">zatrzymany</span>';
        return '<span class="chip info">nieznany</span>';
      }}
      {script}
    </script>
  </body>
</html>"""


def overview_page() -> str:
    body = """
      <section class="grid two">
        <div class="panel">
          <h2>Stan systemu</h2>
          <div id="overview-metrics" class="card-list"></div>
        </div>
        <div class="panel">
          <h2>Supervisor</h2>
          <div id="supervisor-state" class="card"></div>
        </div>
      </section>
      <section class="grid">
        <div class="panel">
          <h2>Kanaly i nody</h2>
          <div id="channels" class="card-list"></div>
        </div>
      </section>
    """
    script = """
      async function boot() {
        const dashboard = await fetchJson('/api/dashboard?limit=20');
        const service = await fetchJson('/api/supervisor');
        const overview = dashboard.overview || {};
        const serviceState = service.effective_active ? 'good' : 'warn';
        const serviceLabel = service.effective_active ? 'aktywny' : 'zatrzymany';
        document.getElementById('overview-metrics').innerHTML = [
          `<div class="card"><div class="muted">Kanaly aktywne</div><div class="metric">${overview.channels_running || 0}/${overview.channels_total || 0}</div></div>`,
          `<div class="card"><div class="muted">Nody online</div><div class="metric">${overview.nodes_online || 0}/${overview.nodes_total || 0}</div></div>`,
          `<div class="card"><div class="muted">Probek zapisanych</div><div class="metric">${overview.samples_written_total || 0}</div></div>`,
          `<div class="card"><div class="muted">Restarty</div><div class="metric">${overview.restart_count_total || 0}</div></div>`
        ].join('');
        document.getElementById('supervisor-state').innerHTML = `
          <div class="btn-row">
            <span class="chip ${serviceState}">${serviceLabel}</span>
            <span class="chip info">${service.service_name || 'brak nazwy'}</span>
          </div>
          <p class="muted">systemd: ${service.active === null ? 'brak danych' : (service.active ? 'aktywny' : 'zatrzymany')} · runtime: ${service.runtime_active ? 'aktywny' : 'zatrzymany'}</p>
          <p class="muted">Substate: ${service.substate || '-'}</p>
          <p class="muted">PID: ${service.pid || '-'}</p>
          <p class="muted">${service.message || ''}</p>
        `;
        document.getElementById('channels').innerHTML = (dashboard.channels || []).map(channel => `
          <div class="card">
            <div class="btn-row">
              <strong>${channel.label || channel.name}</strong>
              <span class="chip ${channel.running ? 'good' : 'warn'}">${channel.running ? 'recording' : 'offline'}</span>
            </div>
            <p class="muted">Port ${channel.port} · baud ${channel.baud} · restarty ${channel.restart_count || 0}</p>
            <p class="muted">Active file: ${channel.active_file || '-'}</p>
            <table>
              <thead><tr><th>Node</th><th>Online</th><th>Output ODR</th><th>Temperatura</th></tr></thead>
              <tbody>
                ${(channel.nodes || []).map(node => `
                  <tr>
                    <td>${node.name || `Node ${node.node_id}`}</td>
                    <td>${node.online ? 'tak' : 'nie'}</td>
                    <td>${node.output_odr_hz || 0}</td>
                    <td>${node.last_temperature_c ?? '-'}</td>
                  </tr>`).join('')}
              </tbody>
            </table>
          </div>`).join('');
      }
      boot().catch(error => alert(error.message));
    """
    return page_template(title="Panel", active="/", body=body, script=script)


def control_page() -> str:
    body = """
      <section class="grid two">
        <div class="panel">
          <h2>Supervisor</h2>
          <div class="btn-row">
            <button class="btn primary" onclick="serviceAction('start')">Start</button>
            <button class="btn" onclick="serviceAction('restart')">Restart</button>
            <button class="btn warn" onclick="serviceAction('stop')">Stop</button>
          </div>
          <div id="service-box" class="card" style="margin-top: 12px;"></div>
        </div>
        <div class="panel">
          <h2>Config node na urzadzeniu</h2>
          <label>Kanal<select id="channel-select"></select></label>
          <label>Node<select id="node-select"></select></label>
          <div class="btn-row">
            <button class="btn primary" onclick="readDeviceConfig()">Odczytaj z node</button>
            <button class="btn" onclick="saveDeviceConfig()">Save</button>
            <button class="btn" onclick="loadDeviceConfig()">Load</button>
            <button class="btn warn" onclick="resetDeviceConfig()">Reset defaults</button>
          </div>
          <div id="device-config-box" class="card" style="margin-top: 12px;"></div>
        </div>
      </section>
      <section class="grid">
        <div class="panel">
          <h2>Zastosuj zmiany na node</h2>
          <div class="form-grid">
            <label>sensor_odr_hz<input id="cfg-odr" type="number" /></label>
            <label>range_g<input id="cfg-range" type="number" /></label>
            <label>high_pass_corner<input id="cfg-highpass" type="number" /></label>
            <label>fifo_watermark<input id="cfg-watermark" type="number" /></label>
            <label>offset_x<input id="cfg-ox" type="number" /></label>
            <label>offset_y<input id="cfg-oy" type="number" /></label>
            <label>offset_z<input id="cfg-oz" type="number" /></label>
            <label>baudrate<input id="cfg-baud" type="number" /></label>
            <label>node_id<input id="cfg-node-id" type="number" /></label>
          </div>
          <div class="btn-row" style="margin-top: 12px;">
            <button class="btn primary" onclick="applyDeviceConfig()">Apply + Save</button>
          </div>
        </div>
      </section>
    """
    script = """
      let channels = [];
      async function loadChannels() {
        const dashboard = await fetchJson('/api/dashboard?limit=5');
        channels = dashboard.channels || [];
        const channelSelect = document.getElementById('channel-select');
        channelSelect.innerHTML = channels.map(channel => `<option value="${channel.name}">${channel.label || channel.name}</option>`).join('');
        renderNodes();
      }
      function renderNodes() {
        const channel = channels.find(item => item.name === document.getElementById('channel-select').value) || channels[0];
        const nodeSelect = document.getElementById('node-select');
        nodeSelect.innerHTML = (channel?.nodes || []).map(node => `<option value="${node.node_id}">${node.name || `Node ${node.node_id}`}</option>`).join('');
      }
      document.addEventListener('change', event => {
        if (event.target.id === 'channel-select') renderNodes();
      });
      async function refreshService() {
        const service = await fetchJson('/api/supervisor');
        document.getElementById('service-box').innerHTML = `
          <div class="btn-row">${statusChip(service.active)} <span class="chip info">${service.service_name || '-'}</span></div>
          <p class="muted">Substate: ${service.substate || '-'}</p>
          <p class="muted">PID: ${service.pid || '-'}</p>
          <p class="muted">${service.message || ''}</p>`;
      }
      async function serviceAction(action) {
        const service = await fetchJson(`/api/supervisor/${action}`, { method: 'POST' });
        await refreshService();
        alert(`Supervisor: ${action}`);
      }
      function selectedTarget() {
        return {
          channel: document.getElementById('channel-select').value,
          node: document.getElementById('node-select').value
        };
      }
      function fillConfig(config) {
        document.getElementById('cfg-odr').value = config.sensor_odr_hz ?? '';
        document.getElementById('cfg-range').value = config.range_g ?? '';
        document.getElementById('cfg-highpass').value = config.high_pass_corner ?? '';
        document.getElementById('cfg-watermark').value = config.fifo_watermark ?? '';
        document.getElementById('cfg-ox').value = config.offset_x ?? '';
        document.getElementById('cfg-oy').value = config.offset_y ?? '';
        document.getElementById('cfg-oz').value = config.offset_z ?? '';
        document.getElementById('cfg-baud').value = config.baudrate ?? '';
        document.getElementById('cfg-node-id').value = config.node_id ?? '';
      }
      async function readDeviceConfig() {
        const target = selectedTarget();
        const payload = await fetchJson(`/api/channels/${encodeURIComponent(target.channel)}/nodes/${target.node}/device-config`);
        document.getElementById('device-config-box').innerHTML = `<pre>${JSON.stringify(payload, null, 2)}</pre>`;
        fillConfig(payload);
      }
      async function applyDeviceConfig() {
        const target = selectedTarget();
        const payload = {
          sensor_odr_hz: Number(document.getElementById('cfg-odr').value),
          range_g: Number(document.getElementById('cfg-range').value),
          high_pass_corner: Number(document.getElementById('cfg-highpass').value),
          fifo_watermark: Number(document.getElementById('cfg-watermark').value),
          offset_x: Number(document.getElementById('cfg-ox').value),
          offset_y: Number(document.getElementById('cfg-oy').value),
          offset_z: Number(document.getElementById('cfg-oz').value),
          baudrate: Number(document.getElementById('cfg-baud').value),
          node_id: Number(document.getElementById('cfg-node-id').value),
          persist: true
        };
        Object.keys(payload).forEach(key => { if (!Number.isFinite(payload[key]) && key !== 'persist') delete payload[key]; });
        const response = await fetchJson(`/api/channels/${encodeURIComponent(target.channel)}/nodes/${target.node}/device-config/apply`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(payload)
        });
        document.getElementById('device-config-box').innerHTML = `<pre>${JSON.stringify(response, null, 2)}</pre>`;
        fillConfig(response.config);
      }
      async function saveDeviceConfig() {
        const target = selectedTarget();
        const response = await fetchJson(`/api/channels/${encodeURIComponent(target.channel)}/nodes/${target.node}/device-config/save`, { method: 'POST' });
        document.getElementById('device-config-box').innerHTML = `<pre>${JSON.stringify(response, null, 2)}</pre>`;
      }
      async function loadDeviceConfig() {
        const target = selectedTarget();
        const response = await fetchJson(`/api/channels/${encodeURIComponent(target.channel)}/nodes/${target.node}/device-config/load`, { method: 'POST' });
        document.getElementById('device-config-box').innerHTML = `<pre>${JSON.stringify(response, null, 2)}</pre>`;
        fillConfig(response);
      }
      async function resetDeviceConfig() {
        const target = selectedTarget();
        const response = await fetchJson(`/api/channels/${encodeURIComponent(target.channel)}/nodes/${target.node}/device-config/reset`, { method: 'POST' });
        document.getElementById('device-config-box').innerHTML = `<pre>${JSON.stringify(response, null, 2)}</pre>`;
        fillConfig(response);
      }
      loadChannels().then(refreshService).catch(error => alert(error.message));
    """
    return page_template(title="Sterowanie hostem i node", active="/control", body=body, script=script)


def config_page() -> str:
    body = """
      <section class="grid">
        <div class="panel">
          <h2>host/system_config.json</h2>
          <p class="muted">Ta strona zapisuje caly obiekt configu po walidacji modelem hosta. Dobre miejsce na swiadome zmiany operatorskie.</p>
          <div class="btn-row">
            <button class="btn" onclick="loadConfig()">Odswiez</button>
            <button class="btn primary" onclick="saveConfig()">Zapisz</button>
          </div>
          <textarea id="config-editor"></textarea>
        </div>
      </section>
    """
    script = """
      async function loadConfig() {
        const payload = await fetchJson('/api/system-config');
        document.getElementById('config-editor').value = JSON.stringify(payload.config, null, 2);
      }
      async function saveConfig() {
        const config = JSON.parse(document.getElementById('config-editor').value);
        await fetchJson('/api/system-config', {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ config })
        });
        alert('Config zapisany');
      }
      loadConfig().catch(error => alert(error.message));
    """
    return page_template(title="Edycja configu hosta", active="/config", body=body, script=script)


def runs_page() -> str:
    body = """
      <section class="grid two">
        <div class="panel">
          <h2>Przegladarka runs</h2>
          <div class="btn-row">
            <button class="btn" onclick="browse(currentPath)">Odswiez</button>
            <button class="btn" onclick="browse(parentPath)">Do gory</button>
          </div>
          <p class="muted mono" id="runs-path"></p>
          <div id="runs-list" class="card-list"></div>
        </div>
        <div class="panel">
          <h2>Podsumowanie</h2>
          <div id="runs-summary" class="card"></div>
        </div>
      </section>
    """
    script = """
      let currentPath = '.';
      let parentPath = '.';
      async function browse(path) {
        const suffix = path && path !== '.' ? `?path=${encodeURIComponent(path)}` : '';
        const payload = await fetchJson(`/api/runs${suffix}`);
        currentPath = payload.relative_path || '.';
        parentPath = payload.parent_relative_path || '.';
        document.getElementById('runs-path').textContent = `root: ${payload.root} / ${currentPath}`;
        const files = (payload.items || []).filter(item => item.type === 'file');
        const dirs = (payload.items || []).filter(item => item.type === 'directory');
        document.getElementById('runs-summary').innerHTML = `
          <div class="metric">${payload.items.length}</div>
          <p class="muted">pozycji w katalogu</p>
          <p class="muted">Katalogi: ${dirs.length} · pliki: ${files.length}</p>`;
        document.getElementById('runs-list').innerHTML = (payload.items || []).map(item => `
          <div class="card">
            <div class="btn-row">
              <strong>${item.name}</strong>
              <span class="chip ${item.type === 'directory' ? 'info' : 'good'}">${item.type}</span>
            </div>
            <p class="muted mono">${item.relative_path}</p>
            <p class="muted">size=${item.size_bytes} · modified=${item.modified_utc}</p>
            <div class="btn-row">
              ${item.type === 'directory'
                ? `<button class="btn" onclick="browse('${item.relative_path}')">Otworz</button><a class="btn" href="${item.download_url}">Pobierz dzien</a>`
                : `<a class="btn" href="${item.download_url}">Pobierz</a>`}
            </div>
          </div>`).join('');
      }
      browse('.').catch(error => alert(error.message));
    """
    return page_template(title="Runs i pobieranie plikow", active="/runs", body=body, script=script)


def logs_page() -> str:
    body = """
      <section class="grid two">
        <div class="panel">
          <h2>Supervisor events</h2>
          <label>Kanal<select id="logs-channel"></select></label>
          <label>Liczba linii<input id="logs-limit" type="number" value="120" min="20" max="500" /></label>
          <div class="btn-row">
            <button class="btn primary" onclick="loadLogs()">Odswiez logi</button>
          </div>
          <pre id="supervisor-events" style="margin-top: 12px;"></pre>
        </div>
        <div class="panel">
          <h2>Process log kanalu</h2>
          <pre id="channel-process-log"></pre>
        </div>
      </section>
    """
    script = """
      let logChannels = [];
      async function loadTargets() {
        const dashboard = await fetchJson('/api/dashboard?limit=5');
        logChannels = dashboard.channels || [];
        const channelSelect = document.getElementById('logs-channel');
        channelSelect.innerHTML = ['<option value="">Wszystkie kanaly</option>'].concat(
          logChannels.map(channel => `<option value="${channel.name}">${channel.label || channel.name}</option>`)
        ).join('');
      }
      async function loadLogs() {
        const channel = document.getElementById('logs-channel').value;
        const limit = document.getElementById('logs-limit').value;
        const suffix = new URLSearchParams({ limit, ...(channel ? { channel } : {}) }).toString();
        const payload = await fetchJson(`/api/logs?${suffix}`);
        document.getElementById('supervisor-events').textContent =
          (payload.supervisor_events || []).map(event => JSON.stringify(event)).join('\\n') || 'Brak eventow';
        const channelLog = (payload.channels || [])[0];
        document.getElementById('channel-process-log').textContent =
          channelLog ? ((channelLog.lines || []).join('\\n') || 'Brak linii w process logu') : 'Brak danych dla kanalu';
      }
      loadTargets().then(loadLogs).catch(error => alert(error.message));
    """
    return page_template(title="Logi hosta i kanalu", active="/logs", body=body, script=script)


class OperatorRequestHandler(BaseHTTPRequestHandler):
    server: "OperatorServer"

    def log_message(self, format: str, *args: object) -> None:
        return None

    @property
    def app(self) -> OperatorApplication:
        return self.server.app

    def do_GET(self) -> None:
        try:
            parsed = urlparse(self.path)
            route = parsed.path
            query = parse_qs(parsed.query)

            if route == "/":
                self._write_response(overview_page().encode("utf-8"), "text/html; charset=utf-8")
                return
            if route == "/logs":
                self._write_response(logs_page().encode("utf-8"), "text/html; charset=utf-8")
                return
            if route == "/control":
                self._write_response(control_page().encode("utf-8"), "text/html; charset=utf-8")
                return
            if route == "/config":
                self._write_response(config_page().encode("utf-8"), "text/html; charset=utf-8")
                return
            if route == "/runs":
                self._write_response(runs_page().encode("utf-8"), "text/html; charset=utf-8")
                return
            if route == "/preview":
                self._write_response(preview_page().encode("utf-8"), "text/html; charset=utf-8")
                return

            if route == "/api/meta":
                self._write_json(self.app.meta_payload())
                return
            if route == "/api/dashboard":
                limit = clamp_limit(query.get("limit", [None])[0], self.app.default_event_limit)
                self._write_json(self.app.dashboard_payload(limit))
                return
            if route == "/api/overview":
                limit = clamp_limit(query.get("limit", [None])[0], self.app.default_event_limit)
                self._write_json(self.app.dashboard_payload(limit)["overview"])
                return
            if route == "/api/channels":
                limit = clamp_limit(query.get("limit", [None])[0], self.app.default_event_limit)
                self._write_json(self.app.dashboard_payload(limit)["channels"])
                return
            if route == "/api/events":
                limit = clamp_limit(query.get("limit", [None])[0], self.app.default_event_limit)
                self._write_json(self.app.dashboard.events_payload(limit=limit))
                return
            if route == "/api/health":
                self._write_json(self.app.dashboard.health_payload())
                return
            if route == "/api/system-config":
                self._write_json(self.app.system_config_payload())
                return
            if route == "/api/supervisor":
                self._write_json(self.app.supervisor_payload())
                return
            if route == "/api/logs":
                limit = clamp_limit(query.get("limit", [None])[0], 120)
                self._write_json(self.app.logs_payload(limit, query.get("channel", [None])[0]))
                return
            if route == "/api/runs":
                self._write_json(self.app.runs_payload(query.get("path", [None])[0]))
                return
            if route == "/api/runs/download":
                download = self.app.runs_repository.download(query.get("path", [None])[0])
                self._write_file_response(
                    download,
                    headers={
                        "Content-Disposition": f'attachment; filename="{download.download_name}"',
                    },
                )
                return

            segments = [segment for segment in route.split("/") if segment]
            if len(segments) == 6 and segments[:2] == ["api", "channels"] and segments[3] == "nodes":
                channel_name = unquote(segments[2])
                node_id = int(segments[4])
                action = segments[5]
                if action == "device-config":
                    self._write_json(self.app.device_config_payload(channel_name, node_id))
                    return
                if action == "preview":
                    limit = max(16, min(int(query.get("limit", ["512"])[0]), MAX_PREVIEW_LIMIT))
                    self._write_json(self.app.preview_payload(channel_name, node_id, limit))
                    return

            self._write_json({"error": "not found", "path": route}, status=HTTPStatus.NOT_FOUND)
        except ApiError as exc:
            self._write_json({"error": exc.message}, status=exc.status)
        except ValueError as exc:
            self._write_json({"error": str(exc)}, status=HTTPStatus.BAD_REQUEST)

    def do_PUT(self) -> None:
        try:
            parsed = urlparse(self.path)
            if parsed.path != "/api/system-config":
                self._write_json({"error": "not found", "path": parsed.path}, status=HTTPStatus.NOT_FOUND)
                return
            payload = self._read_json_body()
            self._write_json(self.app.update_system_config(payload))
        except ApiError as exc:
            self._write_json({"error": exc.message}, status=exc.status)
        except ValueError as exc:
            self._write_json({"error": str(exc)}, status=HTTPStatus.BAD_REQUEST)

    def do_POST(self) -> None:
        try:
            parsed = urlparse(self.path)
            route = parsed.path

            if route.startswith("/api/supervisor/"):
                action = route.rsplit("/", 1)[-1]
                self._write_json(self.app.supervisor_action(action))
                return

            segments = [segment for segment in route.split("/") if segment]
            if len(segments) == 7 and segments[:2] == ["api", "channels"] and segments[3] == "nodes" and segments[5] == "device-config":
                channel_name = unquote(segments[2])
                node_id = int(segments[4])
                action = segments[6]
                if action == "apply":
                    payload = self._read_json_body()
                    self._write_json(self.app.apply_device_config(channel_name, node_id, payload))
                    return
                if action == "load":
                    self._write_json(self.app.load_device_config(channel_name, node_id))
                    return
                if action == "reset":
                    self._write_json(self.app.reset_device_config(channel_name, node_id))
                    return
                if action == "save":
                    self._write_json(self.app.save_device_config(channel_name, node_id))
                    return

            self._write_json({"error": "not found", "path": route}, status=HTTPStatus.NOT_FOUND)
        except ApiError as exc:
            self._write_json({"error": exc.message}, status=exc.status)
        except ValueError as exc:
            self._write_json({"error": str(exc)}, status=HTTPStatus.BAD_REQUEST)

    def _read_json_body(self) -> dict[str, Any]:
        content_length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(content_length) if content_length > 0 else b"{}"
        try:
            payload = json.loads(raw.decode("utf-8"))
        except json.JSONDecodeError as exc:
            raise ApiError(HTTPStatus.BAD_REQUEST, f"invalid JSON body: {exc}") from exc
        if not isinstance(payload, dict):
            raise ApiError(HTTPStatus.BAD_REQUEST, "request body must be a JSON object")
        return payload

    def _write_json(self, payload: Any, status: HTTPStatus = HTTPStatus.OK) -> None:
        encoded = json.dumps(payload, indent=2, sort_keys=True).encode("utf-8")
        self._write_response(encoded, "application/json; charset=utf-8", status=status)

    def _write_response(
        self,
        payload: bytes,
        content_type: str,
        *,
        status: HTTPStatus = HTTPStatus.OK,
        headers: dict[str, str] | None = None,
    ) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Cache-Control", "no-store")
        if headers:
            for key, value in headers.items():
                self.send_header(key, value)
        self.end_headers()
        self.wfile.write(payload)

    def _write_file_response(
        self,
        download: FileDownload,
        *,
        status: HTTPStatus = HTTPStatus.OK,
        headers: dict[str, str] | None = None,
    ) -> None:
        try:
            self.send_response(status)
            self.send_header("Content-Type", download.media_type)
            self.send_header("Content-Length", str(download.path.stat().st_size))
            self.send_header("Cache-Control", "no-store")
            if headers:
                for key, value in headers.items():
                    self.send_header(key, value)
            self.end_headers()
            with download.path.open("rb") as handle:
                while True:
                    chunk = handle.read(64 * 1024)
                    if not chunk:
                        break
                    self.wfile.write(chunk)
        finally:
            if download.cleanup_path is not None:
                download.cleanup_path.unlink(missing_ok=True)


class OperatorServer(ThreadingHTTPServer):
    def __init__(self, server_address: tuple[str, int], app: OperatorApplication) -> None:
        super().__init__(server_address, OperatorRequestHandler)
        self.app = app


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Serve the Sensor System operator panel")
    parser.add_argument("--config", default="host/system_config.json")
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=8090)
    parser.add_argument("--event-limit", type=int, default=40)
    parser.add_argument("--supervisor-service", default="sensor-system-supervisor.service")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    app = OperatorApplication(
        args.config,
        SystemdSupervisorController(args.supervisor_service),
        default_event_limit=args.event_limit,
    )
    server = OperatorServer((args.host, args.port), app)
    print(f"[operator-panel] serving on http://{args.host}:{args.port}/ using {args.config}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("[operator-panel] stopping")
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
