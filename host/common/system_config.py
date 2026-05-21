from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class SystemInfo:
    name: str = "sensor-system"
    site: str | None = None
    timezone: str | None = None


@dataclass(frozen=True)
class StorageConfig:
    root_dir: str = "runs/sensor-system"
    format: str = "hdf5"
    compression: str = "gzip"
    window_seconds: int = 600


@dataclass(frozen=True)
class SupervisorConfig:
    status_file: str = "/tmp/sensor-system_supervisor_status.json"
    event_log: str = "/tmp/sensor-system_supervisor_events.jsonl"
    channel_runtime_dir: str = "/tmp/sensor-system_channels"
    status_interval_s: float = 1.0
    restart_delay_s: float = 2.0


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


@dataclass(frozen=True)
class ChannelConfig:
    name: str
    label: str | None
    port: str
    nodes: tuple[NodeConfig, ...]
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
        config_path = Path(path)
        data = json.loads(config_path.read_text(encoding="utf-8"))
        return cls.from_dict(data)

    @classmethod
    def from_dict(cls, data: dict) -> "HostSystemConfig":
        system_data = data.get("system", {})
        storage_data = data.get("storage", {})
        supervisor_data = data.get("supervisor", data.get("runtime", {}))
        channels_data = data.get("channels")

        if channels_data is None:
            return cls._from_legacy_dict(data)

        channels: list[ChannelConfig] = []
        for index, raw in enumerate(channels_data):
            raw_nodes = raw.get("nodes", [])
            nodes = cls._parse_nodes(raw_nodes)
            if not nodes:
                raise ValueError(f"channel at index {index} must define at least one node")

            name = str(raw.get("name", f"channel-{index + 1}"))
            channels.append(
                ChannelConfig(
                    name=name,
                    label=str(raw.get("label")) if raw.get("label") is not None else None,
                    port=str(raw["port"]),
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
                format=str(storage_data.get("format", StorageConfig.format)),
                compression=str(storage_data.get("compression", StorageConfig.compression)),
                window_seconds=int(storage_data.get("window_seconds", StorageConfig.window_seconds)),
            ),
            supervisor=SupervisorConfig(
                status_file=str(supervisor_data.get("status_file", SupervisorConfig.status_file)),
                event_log=str(supervisor_data.get("event_log", SupervisorConfig.event_log)),
                channel_runtime_dir=str(
                    supervisor_data.get("channel_runtime_dir", SupervisorConfig.channel_runtime_dir)
                ),
                status_interval_s=float(
                    supervisor_data.get("status_interval_s", SupervisorConfig.status_interval_s)
                ),
                restart_delay_s=float(
                    supervisor_data.get("restart_delay_s", SupervisorConfig.restart_delay_s)
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
                format=str(storage_data.get("format", StorageConfig.format)),
                compression=str(storage_data.get("compression", StorageConfig.compression)),
                window_seconds=legacy_window_seconds,
            ),
            supervisor=SupervisorConfig(),
            channels=(
                ChannelConfig(
                    name="channel-1",
                    label="Channel 1",
                    port=str(serial_data.get("port", "/dev/sensor-system-rs485")),
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
            )
            if node.enabled:
                nodes.append(node)

        return tuple(nodes)
