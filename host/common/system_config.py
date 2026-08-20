from __future__ import annotations

import json
from copy import deepcopy
from dataclasses import dataclass
from pathlib import Path


def _item_identity(section: str, item: object) -> object:
    if section == "channels" and isinstance(item, dict):
        return item.get("name")
    if section == "nodes":
        if isinstance(item, int):
            return item
        if isinstance(item, dict):
            return item.get("id", item.get("node_id"))
    return None


def merge_config_data(base: object, overlay: object, *, section: str = "") -> object:
    """Recursively merge config data, matching channels by name and nodes by id."""
    if isinstance(base, dict) and isinstance(overlay, dict):
        merged = deepcopy(base)
        for key, value in overlay.items():
            if key in merged:
                merged[key] = merge_config_data(merged[key], value, section=key)
            else:
                merged[key] = deepcopy(value)
        return merged

    if isinstance(base, list) and isinstance(overlay, list) and section in {"channels", "nodes"}:
        merged = deepcopy(base)
        positions = {
            _item_identity(section, item): index
            for index, item in enumerate(merged)
            if _item_identity(section, item) is not None
        }
        for item in overlay:
            identity = _item_identity(section, item)
            if identity is None:
                raise ValueError(f"overlay entry in '{section}' must define its identity")
            if identity in positions:
                index = positions[identity]
                merged[index] = merge_config_data(
                    merged[index], item, section=section
                )
            else:
                positions[identity] = len(merged)
                merged.append(deepcopy(item))
        return merged

    return deepcopy(overlay)


def load_config_data(
    path: str | Path,
    *,
    _stack: tuple[Path, ...] = (),
) -> dict:
    """Load a system config, resolving an optional relative ``extends`` chain."""
    config_path = Path(path).expanduser().resolve()
    if config_path in _stack:
        chain = " -> ".join(str(item) for item in (*_stack, config_path))
        raise ValueError(f"circular system config extends chain: {chain}")

    raw = json.loads(config_path.read_text(encoding="utf-8"))
    if not isinstance(raw, dict):
        raise ValueError(f"system config '{config_path}' must contain a JSON object")

    extends = raw.pop("extends", None)
    if extends is None:
        return raw
    if not isinstance(extends, str) or not extends.strip():
        raise ValueError("system config 'extends' must be a non-empty path string")

    base_path = Path(extends).expanduser()
    if not base_path.is_absolute():
        base_path = config_path.parent / base_path
    base = load_config_data(base_path, _stack=(*_stack, config_path))
    merged = merge_config_data(base, raw)
    if not isinstance(merged, dict):
        raise ValueError("resolved system config must contain a JSON object")
    return merged


@dataclass(frozen=True)
class SystemInfo:
    name: str = "sensor-system"
    site: str | None = None
    timezone: str | None = None


@dataclass(frozen=True)
class StorageConfig:
    root_dir: str = "var/recordings"
    archive_dir: str = "var/archive"
    format: str = "hdf5"
    compression: str = "gzip"
    window_seconds: int = 600
    capture_schema: int = 5
    min_free_bytes: int = 1024 * 1024 * 1024


@dataclass(frozen=True)
class SupervisorConfig:
    status_file: str = "/run/sensor-system/supervisor.status.json"
    event_log: str = "var/log/supervisor.events.jsonl"
    channel_runtime_dir: str = "/run/sensor-system"
    log_dir: str = "var/log"
    status_interval_s: float = 1.0
    restart_delay_s: float = 2.0
    restart_delay_max_s: float = 60.0
    process_log_max_bytes: int = 10 * 1024 * 1024
    process_log_backup_count: int = 3
    console_status_interval_s: float = 30.0


@dataclass(frozen=True)
class NodeConfig:
    node_id: int
    name: str | None = None
    enabled: bool = True
    expected_odr_hz: float | None = None
    sensor_odr_hz: int | None = None
    range_g: int | None = None
    high_pass_corner: int | None = None
    fifo_watermark: int | None = None
    offset_x: int | None = None
    offset_y: int | None = None
    offset_z: int | None = None
    sensor_label: str | None = None
    sensor_id: str | None = None
    hardware_id: str | None = None
    board_revision: int | None = None
    filter_profile: int | None = None
    decimation_factor: int | None = None


@dataclass(frozen=True)
class ChannelConfig:
    name: str
    label: str | None
    port: str
    nodes: tuple[NodeConfig, ...]
    output_dir: str | None = None
    baud: int = 115200
    enabled: bool = True
    start_from: str = "newest"
    grant_packets: int = 4
    timeout: float = 0.5
    burst_idle_timeout: float = 0.15
    burst_session_timeout: float = 0.75
    status_interval_s: float = 1.0
    flush_interval_s: float = 2.0
    stats_interval_s: float = 5.0
    temperature_interval_s: float = 3600.0
    idle_sleep_s: float = 0.01
    error_sleep_s: float = 0.10
    timing_mode: str = "legacy"
    channel_id: int | None = None

    @property
    def node_ids(self) -> tuple[int, ...]:
        return tuple(node.node_id for node in self.nodes if node.enabled)

    def node_name_map(self) -> dict[int, str]:
        return {
            node.node_id: node.name
            for node in self.nodes
            if node.enabled and node.name
        }


@dataclass(frozen=True)
class HostSystemConfig:
    system: SystemInfo
    storage: StorageConfig
    supervisor: SupervisorConfig
    channels: tuple[ChannelConfig, ...]

    @classmethod
    def load(cls, path: str | Path) -> "HostSystemConfig":
        return cls.from_dict(load_config_data(path))

    @classmethod
    def from_dict(cls, data: dict) -> "HostSystemConfig":
        system_data = data.get("system", {})
        storage_data = data.get("storage", {})
        supervisor_data = data.get("supervisor", data.get("runtime", {}))
        channels_data = data.get("channels")

        if channels_data is None:
            return cls._from_legacy_dict(data)

        capture_schema = int(
            storage_data.get("capture_schema", StorageConfig.capture_schema)
        )
        if capture_schema not in {1, 5}:
            raise ValueError("storage.capture_schema must be 1 or 5")
        window_seconds = int(
            storage_data.get("window_seconds", StorageConfig.window_seconds)
        )
        min_free_bytes = int(
            storage_data.get("min_free_bytes", StorageConfig.min_free_bytes)
        )
        if min_free_bytes < 0:
            raise ValueError("storage.min_free_bytes must be non-negative")
        if window_seconds <= 0:
            raise ValueError("storage.window_seconds must be greater than zero")
        if capture_schema == 1 and (
            not 60 <= window_seconds <= 3600
            or 86400 % window_seconds != 0
        ):
            raise ValueError(
                "Capture v1 storage.window_seconds must divide one UTC day "
                "and be in range 60..3600"
            )

        channels: list[ChannelConfig] = []
        for index, raw in enumerate(channels_data):
            raw_nodes = raw.get("nodes", [])
            nodes = cls._parse_nodes(raw_nodes)
            if not nodes:
                raise ValueError(f"channel at index {index} must define at least one node")

            name = str(raw.get("name", f"channel-{index + 1}"))
            timing_mode = str(raw.get("timing_mode", "legacy"))
            if timing_mode not in {"legacy", "observe", "required"}:
                raise ValueError(
                    f"channel '{name}' has unsupported timing_mode "
                    f"'{timing_mode}'"
                )
            if timing_mode == "required" and str(
                storage_data.get("format", StorageConfig.format)
            ) != "hdf5":
                raise ValueError(
                    f"channel '{name}' timing_mode 'required' requires "
                    "HDF5 storage"
                )
            channel_id = int(raw.get("channel_id", index + 1))
            if not 1 <= channel_id <= 8:
                raise ValueError(
                    f"channel '{name}' channel_id must be in range 1..8"
                )
            if capture_schema == 1 and len(nodes) != 1:
                raise ValueError(
                    f"channel '{name}' Capture v1 requires exactly one node"
                )
            channels.append(
                ChannelConfig(
                    name=name,
                    label=str(raw.get("label")) if raw.get("label") is not None else None,
                    port=str(raw["port"]),
                    output_dir=str(raw["output_dir"]) if raw.get("output_dir") is not None else None,
                    enabled=bool(raw.get("enabled", True)),
                    baud=int(raw.get("baud", 115200)),
                    nodes=nodes,
                    start_from=str(raw.get("start_from", "newest")),
                    grant_packets=int(raw.get("grant_packets", 4)),
                    timeout=float(raw.get("timeout", 0.5)),
                    burst_idle_timeout=float(raw.get("burst_idle_timeout", 0.15)),
                    burst_session_timeout=float(raw.get("burst_session_timeout", 0.75)),
                    status_interval_s=float(raw.get("status_interval_s", 1.0)),
                    flush_interval_s=float(raw.get("flush_interval_s", 2.0)),
                    stats_interval_s=float(raw.get("stats_interval_s", 5.0)),
                    temperature_interval_s=float(raw.get("temperature_interval_s", 3600.0)),
                    idle_sleep_s=float(raw.get("idle_sleep_s", 0.01)),
                    error_sleep_s=float(raw.get("error_sleep_s", 0.10)),
                    timing_mode=timing_mode,
                    channel_id=channel_id,
                )
            )

        return cls(
            system=SystemInfo(
                name=str(system_data.get("name", SystemInfo.name)),
                site=str(system_data["site"]) if system_data.get("site") is not None else None,
                timezone=str(system_data["timezone"]) if system_data.get("timezone") is not None else None,
            ),
            storage=StorageConfig(
                root_dir=str(storage_data.get("root_dir", StorageConfig.root_dir)),
                archive_dir=str(storage_data.get("archive_dir", StorageConfig.archive_dir)),
                format=str(storage_data.get("format", StorageConfig.format)),
                compression=str(storage_data.get("compression", StorageConfig.compression)),
                window_seconds=window_seconds,
                capture_schema=capture_schema,
                min_free_bytes=min_free_bytes,
            ),
            supervisor=SupervisorConfig(
                status_file=str(supervisor_data.get("status_file", SupervisorConfig.status_file)),
                event_log=str(supervisor_data.get("event_log", SupervisorConfig.event_log)),
                channel_runtime_dir=str(
                    supervisor_data.get("channel_runtime_dir", SupervisorConfig.channel_runtime_dir)
                ),
                log_dir=str(supervisor_data.get("log_dir", SupervisorConfig.log_dir)),
                status_interval_s=float(
                    supervisor_data.get("status_interval_s", SupervisorConfig.status_interval_s)
                ),
                restart_delay_s=float(
                    supervisor_data.get("restart_delay_s", SupervisorConfig.restart_delay_s)
                ),
                restart_delay_max_s=float(
                    supervisor_data.get(
                        "restart_delay_max_s",
                        SupervisorConfig.restart_delay_max_s,
                    )
                ),
                process_log_max_bytes=int(
                    supervisor_data.get(
                        "process_log_max_bytes",
                        SupervisorConfig.process_log_max_bytes,
                    )
                ),
                process_log_backup_count=int(
                    supervisor_data.get(
                        "process_log_backup_count",
                        SupervisorConfig.process_log_backup_count,
                    )
                ),
                console_status_interval_s=float(
                    supervisor_data.get(
                        "console_status_interval_s",
                        SupervisorConfig.console_status_interval_s,
                    )
                ),
            ),
            channels=tuple(channels),
        )

    @classmethod
    def _from_legacy_dict(cls, data: dict) -> "HostSystemConfig":
        serial_data = data.get("serial", {})
        recorder_data = data.get("recorder", {})
        storage_data = data.get("storage", {})
        if "window_seconds" in storage_data:
            legacy_window_seconds = int(storage_data["window_seconds"])
        elif "rotate_daily" in storage_data:
            legacy_window_seconds = 86400 if bool(storage_data.get("rotate_daily", True)) else StorageConfig.window_seconds
        else:
            legacy_window_seconds = StorageConfig.window_seconds
        enabled_nodes = tuple(
            NodeConfig(
                node_id=int(node["node_id"]),
                name=str(node["name"]) if node.get("name") is not None else None,
                enabled=bool(node.get("enabled", True)),
                expected_odr_hz=float(node["expected_odr_hz"]) if node.get("expected_odr_hz") is not None else None,
                sensor_odr_hz=int(node["sensor_odr_hz"]) if node.get("sensor_odr_hz") is not None else None,
                range_g=int(node["range_g"]) if node.get("range_g") is not None else None,
                high_pass_corner=int(node["high_pass_corner"]) if node.get("high_pass_corner") is not None else None,
                fifo_watermark=int(node["fifo_watermark"]) if node.get("fifo_watermark") is not None else None,
                offset_x=int(node["offset_x"]) if node.get("offset_x") is not None else None,
                offset_y=int(node["offset_y"]) if node.get("offset_y") is not None else None,
                offset_z=int(node["offset_z"]) if node.get("offset_z") is not None else None,
            )
            for node in data.get("nodes", [])
        )
        filtered_nodes = tuple(node for node in enabled_nodes if node.enabled)
        if not filtered_nodes:
            raise ValueError("legacy config must define at least one enabled node")

        return cls(
            system=SystemInfo(),
            storage=StorageConfig(
                root_dir=str(storage_data.get("root_dir", StorageConfig.root_dir)),
                archive_dir=str(storage_data.get("archive_dir", StorageConfig.archive_dir)),
                format=str(storage_data.get("format", StorageConfig.format)),
                compression=str(storage_data.get("compression", StorageConfig.compression)),
                window_seconds=legacy_window_seconds,
                min_free_bytes=int(
                    storage_data.get(
                        "min_free_bytes",
                        StorageConfig.min_free_bytes,
                    )
                ),
            ),
            supervisor=SupervisorConfig(),
            channels=(
                ChannelConfig(
                    name="channel-1",
                    label="Channel 1",
                    port=str(serial_data.get("port", "/dev/sensor-system-rs485")),
                    output_dir=None,
                    baud=int(serial_data.get("baud", 115200)),
                    nodes=filtered_nodes,
                    start_from=str(recorder_data.get("start_from", "newest")),
                    grant_packets=int(recorder_data.get("grant_packets", 4)),
                    flush_interval_s=float(recorder_data.get("flush_interval_s", 2.0)),
                    stats_interval_s=float(recorder_data.get("stats_interval_s", 5.0)),
                    temperature_interval_s=float(recorder_data.get("temperature_interval_s", 3600.0)),
                ),
            ),
        )

    @staticmethod
    def _parse_nodes(raw_nodes: list) -> tuple[NodeConfig, ...]:
        nodes: list[NodeConfig] = []
        for raw in raw_nodes:
            if isinstance(raw, int):
                nodes.append(NodeConfig(node_id=raw))
                continue

            if not isinstance(raw, dict):
                raise ValueError(f"unsupported node config entry: {raw!r}")

            node = NodeConfig(
                node_id=int(raw.get("id", raw.get("node_id"))),
                name=str(raw["name"]) if raw.get("name") is not None else None,
                enabled=bool(raw.get("enabled", True)),
                expected_odr_hz=float(raw["expected_odr_hz"]) if raw.get("expected_odr_hz") is not None else None,
                sensor_odr_hz=int(raw["sensor_odr_hz"]) if raw.get("sensor_odr_hz") is not None else None,
                range_g=int(raw["range_g"]) if raw.get("range_g") is not None else None,
                high_pass_corner=int(raw["high_pass_corner"]) if raw.get("high_pass_corner") is not None else None,
                fifo_watermark=int(raw["fifo_watermark"]) if raw.get("fifo_watermark") is not None else None,
                offset_x=int(raw["offset_x"]) if raw.get("offset_x") is not None else None,
                offset_y=int(raw["offset_y"]) if raw.get("offset_y") is not None else None,
                offset_z=int(raw["offset_z"]) if raw.get("offset_z") is not None else None,
                sensor_label=str(raw["sensor_label"]) if raw.get("sensor_label") is not None else None,
                sensor_id=str(raw["sensor_id"]) if raw.get("sensor_id") is not None else None,
                hardware_id=str(raw["hardware_id"]) if raw.get("hardware_id") is not None else None,
                board_revision=int(raw["board_revision"]) if raw.get("board_revision") is not None else None,
                filter_profile=int(raw["filter_profile"]) if raw.get("filter_profile") is not None else None,
                decimation_factor=int(raw["decimation_factor"]) if raw.get("decimation_factor") is not None else None,
            )
            if node.board_revision not in {None, 1, 2}:
                raise ValueError(
                    f"node {node.node_id} board_revision must be 1 or 2"
                )
            if node.enabled:
                nodes.append(node)

        return tuple(nodes)
