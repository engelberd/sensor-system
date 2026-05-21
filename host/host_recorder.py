#!/usr/bin/env python3
from __future__ import annotations

import argparse
import csv
import json
import socket
import signal
import struct
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Iterable, Optional
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

import serial

try:
    from host_lab import (
        BURST_HEADER_FORMAT,
        CMD_GRANT_BURST_READ,
        HOST_NODE_ID,
        STATUS_NO_DATA,
        STATUS_OK,
        ConfigView,
        NodeStats,
        ProtocolClient,
        build_buffer_state_payload,
        build_commit_payload,
        build_get_config_payload,
        build_get_temperature_payload,
        build_grant_burst_payload,
        build_stats_payload,
        effective_output_odr_hz,
        parse_buffer_state,
        parse_commit_response,
        parse_config_view,
        parse_grant_burst_response,
        parse_stats,
        parse_temperature_view,
    )
except ModuleNotFoundError:
    from host.host_lab import (
        BURST_HEADER_FORMAT,
        CMD_GRANT_BURST_READ,
        HOST_NODE_ID,
        STATUS_NO_DATA,
        STATUS_OK,
        ConfigView,
        NodeStats,
        ProtocolClient,
        build_buffer_state_payload,
        build_commit_payload,
        build_get_config_payload,
        build_get_temperature_payload,
        build_grant_burst_payload,
        build_stats_payload,
        effective_output_odr_hz,
        parse_buffer_state,
        parse_commit_response,
        parse_config_view,
        parse_grant_burst_response,
        parse_stats,
        parse_temperature_view,
    )
from host.common.runtime_status import (
    JsonStatusWriter,
    JsonlEventWriter,
    RuntimeStatusNode,
    RuntimeStatusSnapshot,
)
from host.live_web import LiveBuffer, LiveGap, LiveSample, LiveServer


SAMPLE_PAYLOAD_OFFSET = struct.calcsize(BURST_HEADER_FORMAT)
RECORDER_SCHEMA_VERSION = 4
RECORDER_VERSION = "0.3.0"
DEFAULT_WINDOW_SECONDS = 600
STANDARD_GRAVITY_M_S2 = 9.80665


@dataclass
class SampleRecord:
    node_id: int
    sample_seq: int
    x: float
    y: float
    z: float
    packet_seq: int


@dataclass
class TemperatureRecord:
    node_id: int
    sample_seq_anchor: int
    temp_raw: int
    temp_celsius: float


@dataclass
class RecorderNode:
    node_id: int
    config: ConfigView
    committed_sample_seq: int = 0
    expected_sample_seq: int = 0
    last_written_seq: int = 0
    samples_written: int = 0
    bursts_ok: int = 0
    bursts_no_data: int = 0
    bursts_failed: int = 0
    gaps_detected: int = 0
    empty_polls: int = 0
    last_stats: Optional[NodeStats] = None
    next_temperature_at: float = 0.0
    online: bool = False
    last_temperature_c: Optional[float] = None
    last_temperature_unix_ns: Optional[int] = None
    last_temperature_window_start: Optional[datetime] = None
    next_window_temperature_retry_at: float = 0.0
    baseline_sensor_loss: int = 0
    baseline_rx_overflow_count: int = 0
    baseline_packet_overwrite_count: int = 0


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


class BaseWriter:
    def add_node(self, node: RecorderNode) -> None:
        raise NotImplementedError

    def write_samples(self, node_id: int, samples: list[SampleRecord]) -> None:
        raise NotImplementedError

    def write_temperature(self, node_id: int, records: list[TemperatureRecord]) -> None:
        raise NotImplementedError

    def write_gap(
        self,
        node_id: int,
        expected_sample_seq: int,
        received_sample_seq: int,
        packet_seq: int,
    ) -> None:
        raise NotImplementedError

    def flush(self) -> None:
        raise NotImplementedError

    def close(self) -> None:
        raise NotImplementedError


class CsvWriter(BaseWriter):
    def __init__(self, path: Path, metadata: dict[str, object], append: bool = False) -> None:
        self.path = path
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.metadata = metadata
        self.node_metadata: list[dict[str, object]] = []
        self.meta_path = self.path.with_suffix(self.path.suffix + ".meta.json")
        self.temperature_path = self.path.with_suffix(self.path.suffix + ".temperature.csv")
        self.gaps_path = self.path.with_suffix(self.path.suffix + ".gaps.csv")
        self.handle = self.path.open("a" if append else "w", newline="", encoding="utf-8")
        self.csv = csv.writer(self.handle)
        self.temperature_handle = self.temperature_path.open("a" if append else "w", newline="", encoding="utf-8")
        self.temperature_csv = csv.writer(self.temperature_handle)
        self.gaps_handle = self.gaps_path.open("a" if append else "w", newline="", encoding="utf-8")
        self.gaps_csv = csv.writer(self.gaps_handle)
        if not append or self.path.stat().st_size == 0:
            self.csv.writerow([
                "node_id",
                "sample_seq",
                "x",
                "y",
                "z",
                "packet_seq",
            ])
        if not append or self.temperature_path.stat().st_size == 0:
            self.temperature_csv.writerow([
                "node_id",
                "sample_seq_anchor",
                "temp_raw",
                "temp_celsius",
            ])
        if not append or self.gaps_path.stat().st_size == 0:
            self.gaps_csv.writerow([
                "node_id",
                "expected_sample_seq",
                "received_sample_seq",
                "packet_seq",
            ])

    def add_node(self, node: RecorderNode) -> None:
        self.node_metadata.append({
            "node_id": node.node_id,
            "sensor_odr_hz": node.config.odr_hz,
            "output_odr_hz": effective_output_odr_hz(node.config.odr_hz),
            "range_g": node.config.range_g,
            "accel_unit": "m/s^2",
            "high_pass_corner": node.config.high_pass_corner,
            "fifo_watermark": node.config.fifo_watermark,
            "offset_x": node.config.offset_x,
            "offset_y": node.config.offset_y,
            "offset_z": node.config.offset_z,
            "baseline_sensor_loss": node.baseline_sensor_loss,
            "baseline_rx_overflow_count": node.baseline_rx_overflow_count,
            "baseline_packet_overwrite_count": node.baseline_packet_overwrite_count,
        })
        self.write_metadata()

    def write_metadata(self) -> None:
        payload = dict(self.metadata)
        payload["nodes"] = self.node_metadata
        self.meta_path.write_text(
            json.dumps(payload, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )

    def write_samples(self, node_id: int, samples: list[SampleRecord]) -> None:
        del node_id
        for sample in samples:
            self.csv.writerow([
                sample.node_id,
                sample.sample_seq,
                sample.x,
                sample.y,
                sample.z,
                sample.packet_seq,
            ])

    def write_temperature(self, node_id: int, records: list[TemperatureRecord]) -> None:
        del node_id
        for record in records:
            self.temperature_csv.writerow([
                record.node_id,
                record.sample_seq_anchor,
                record.temp_raw,
                record.temp_celsius,
            ])

    def write_gap(
        self,
        node_id: int,
        expected_sample_seq: int,
        received_sample_seq: int,
        packet_seq: int,
    ) -> None:
        self.gaps_csv.writerow([
            node_id,
            expected_sample_seq,
            received_sample_seq,
            packet_seq,
        ])

    def flush(self) -> None:
        self.handle.flush()
        self.temperature_handle.flush()
        self.gaps_handle.flush()

    def close(self) -> None:
        self.flush()
        self.handle.close()
        self.temperature_handle.close()
        self.gaps_handle.close()


class Hdf5Writer(BaseWriter):
    def __init__(self, path: Path, metadata: dict[str, object], compression: str, append: bool = False) -> None:
        try:
            import h5py  # type: ignore
            import numpy as np  # type: ignore
        except ImportError as exc:
            raise RuntimeError(
                "HDF5 output requires numpy+h5py. Install them with: "
                "host/.venv/bin/python -m pip install -r host/requirements-recorder.txt"
            ) from exc

        self.h5py = h5py
        self.np = np
        self.path = path
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            self.file = h5py.File(self.path, "a" if append else "w")
        except PermissionError as exc:
            raise RuntimeError(
                f"cannot write HDF5 output to '{self.path}'. "
                "Choose a writable output directory."
            ) from exc
        except OSError as exc:
            raise RuntimeError(f"cannot prepare HDF5 output '{self.path}': {exc}") from exc
        self.nodes_group = self.file.require_group("nodes")
        self.datasets: dict[int, object] = {}
        self.temperature_datasets: dict[int, object] = {}
        self.gap_datasets: dict[int, object] = {}
        self.compression = None if compression == "none" else compression

        self.file.attrs["schema_version"] = RECORDER_SCHEMA_VERSION
        for key, value in metadata.items():
            self.file.attrs[key] = value

    def add_node(self, node: RecorderNode) -> None:
        group = self.nodes_group.require_group(str(node.node_id))
        group.attrs["node_id"] = node.node_id
        group.attrs["sensor_odr_hz"] = node.config.odr_hz
        group.attrs["output_odr_hz"] = effective_output_odr_hz(node.config.odr_hz)
        group.attrs["range_g"] = node.config.range_g
        group.attrs["accel_unit"] = "m/s^2"
        group.attrs["high_pass_corner"] = node.config.high_pass_corner
        group.attrs["fifo_watermark"] = node.config.fifo_watermark
        group.attrs["offset_x"] = node.config.offset_x
        group.attrs["offset_y"] = node.config.offset_y
        group.attrs["offset_z"] = node.config.offset_z
        group.attrs["baseline_sensor_loss"] = node.baseline_sensor_loss
        group.attrs["baseline_rx_overflow_count"] = node.baseline_rx_overflow_count
        group.attrs["baseline_packet_overwrite_count"] = node.baseline_packet_overwrite_count

        sample_dtype = self.np.dtype([
            ("sample_seq", "<u8"),
            ("x", "<f4"),
            ("y", "<f4"),
            ("z", "<f4"),
            ("packet_seq", "<u4"),
        ])
        temperature_dtype = self.np.dtype([
            ("sample_seq_anchor", "<u8"),
            ("temp_raw", "<u2"),
            ("temp_celsius", "<f4"),
        ])
        gaps_dtype = self.np.dtype([
            ("expected_sample_seq", "<u8"),
            ("received_sample_seq", "<u8"),
            ("packet_seq", "<u4"),
        ])
        dataset = group.require_dataset(
            "samples",
            shape=(0,),
            maxshape=(None,),
            chunks=(4096,),
            dtype=sample_dtype,
            compression=self.compression,
        )
        temperature_dataset = group.require_dataset(
            "temperature",
            shape=(0,),
            maxshape=(None,),
            chunks=(256,),
            dtype=temperature_dtype,
            compression=self.compression,
        )
        gaps_dataset = group.require_dataset(
            "gaps",
            shape=(0,),
            maxshape=(None,),
            chunks=(256,),
            dtype=gaps_dtype,
            compression=self.compression,
        )
        self.datasets[node.node_id] = dataset
        self.temperature_datasets[node.node_id] = temperature_dataset
        self.gap_datasets[node.node_id] = gaps_dataset

    def write_samples(self, node_id: int, samples: list[SampleRecord]) -> None:
        if not samples:
            return

        dataset = self.datasets[node_id]
        offset = dataset.shape[0]
        dataset.resize((offset + len(samples),))

        arr = self.np.empty(len(samples), dtype=dataset.dtype)
        for i, sample in enumerate(samples):
            arr[i] = (
                sample.sample_seq,
                sample.x,
                sample.y,
                sample.z,
                sample.packet_seq,
            )

        dataset[offset:offset + len(samples)] = arr

    def write_temperature(self, node_id: int, records: list[TemperatureRecord]) -> None:
        if not records:
            return

        dataset = self.temperature_datasets[node_id]
        offset = dataset.shape[0]
        dataset.resize((offset + len(records),))

        arr = self.np.empty(len(records), dtype=dataset.dtype)
        for i, record in enumerate(records):
            arr[i] = (
                record.sample_seq_anchor,
                record.temp_raw,
                record.temp_celsius,
            )

        dataset[offset:offset + len(records)] = arr

    def write_gap(
        self,
        node_id: int,
        expected_sample_seq: int,
        received_sample_seq: int,
        packet_seq: int,
    ) -> None:
        dataset = self.gap_datasets[node_id]
        offset = dataset.shape[0]
        dataset.resize((offset + 1,))
        dataset[offset:offset + 1] = self.np.array(
            [(expected_sample_seq, received_sample_seq, packet_seq)],
            dtype=dataset.dtype,
        )

    def flush(self) -> None:
        self.file.flush()

    def close(self) -> None:
        self.flush()
        self.file.close()


class WindowedWriter(BaseWriter):
    def __init__(
        self,
        args: argparse.Namespace,
        metadata: dict[str, object],
        nodes: list[RecorderNode],
    ) -> None:
        self.args = args
        self.metadata = metadata
        self.nodes = nodes
        self.window_timezone = args.window_timezone
        self.current_window_start: Optional[datetime] = None
        self.current_path: Optional[Path] = None
        self.writer: Optional[BaseWriter] = None

    def current_window(self, now_utc: Optional[datetime] = None) -> datetime:
        resolved_now = now_utc or datetime.now(timezone.utc)
        if resolved_now.tzinfo is None:
            resolved_now = resolved_now.replace(tzinfo=timezone.utc)
        local_now = resolved_now.astimezone(self.window_timezone)
        window_seconds = max(1, int(self.args.window_seconds))
        local_day_start = local_now.replace(hour=0, minute=0, second=0, microsecond=0)
        seconds_since_day_start = int((local_now - local_day_start).total_seconds())
        local_window_start = local_day_start + timedelta(
            seconds=(seconds_since_day_start // window_seconds) * window_seconds
        )
        return local_window_start.astimezone(timezone.utc)

    def window_path_for(self, window_start: datetime) -> Path:
        suffix = ".h5" if self.args.format == "hdf5" else ".csv"
        window_start_local = window_start.astimezone(self.window_timezone)
        if self.args.window_seconds >= 86400 and self.args.channel_name == "default":
            return Path(self.args.output_dir) / f"{window_start_local.date().isoformat()}{suffix}"

        window_label = window_start_local.strftime("%Y-%m-%d_%H-%M")
        day_dir = Path(self.args.output_dir) / window_start_local.strftime("%Y-%m-%d")
        return day_dir / f"{window_label}{suffix}"

    def ensure_writer(self, now_utc: Optional[datetime] = None) -> BaseWriter:
        window_start = self.current_window(now_utc)
        if self.writer is not None and self.current_window_start == window_start:
            return self.writer

        if self.writer is not None:
            self.writer.close()

        path = self.window_path_for(window_start)
        append = path.exists()
        window_metadata = dict(self.metadata)
        window_end = window_start + timedelta(seconds=max(1, int(self.args.window_seconds)))
        window_start_local = window_start.astimezone(self.window_timezone)
        window_end_local = window_end.astimezone(self.window_timezone)
        window_metadata["window_start_utc"] = window_start.isoformat()
        window_metadata["window_end_utc"] = window_end.isoformat()
        window_metadata["window_timezone"] = self.args.window_timezone_name
        window_metadata["window_start_local"] = window_start_local.isoformat()
        window_metadata["window_end_local"] = window_end_local.isoformat()
        if append:
            window_metadata["file_opened_utc"] = datetime.now(timezone.utc).isoformat()
        else:
            window_metadata["file_created_utc"] = datetime.now(timezone.utc).isoformat()
        self.writer = make_single_writer(self.args, window_metadata, path, append=append)
        self.current_window_start = window_start
        self.current_path = path
        for node in self.nodes:
            self.writer.add_node(node)
        print(f"[FILE] {path} ({'append' if append else 'new'})")
        return self.writer

    def add_node(self, node: RecorderNode) -> None:
        del node
        self.ensure_writer()

    def write_samples(self, node_id: int, samples: list[SampleRecord]) -> None:
        writer = self.ensure_writer()
        writer.write_samples(node_id, samples)

    def write_temperature(self, node_id: int, records: list[TemperatureRecord]) -> None:
        writer = self.ensure_writer()
        writer.write_temperature(node_id, records)

    def write_gap(
        self,
        node_id: int,
        expected_sample_seq: int,
        received_sample_seq: int,
        packet_seq: int,
    ) -> None:
        writer = self.ensure_writer()
        writer.write_gap(
            node_id,
            expected_sample_seq,
            received_sample_seq,
            packet_seq,
        )

    def flush(self) -> None:
        if self.writer is not None:
            self.writer.flush()

    def close(self) -> None:
        if self.writer is not None:
            self.writer.close()
            self.writer = None
            self.current_window_start = None


def parse_node_list(value: str) -> list[int]:
    nodes = [int(part.strip()) for part in value.split(",") if part.strip()]
    if not nodes:
        raise argparse.ArgumentTypeError("node list must not be empty")
    return nodes


def resolve_window_timezone(value: str):
    normalized = value.strip()
    if not normalized:
        raise ValueError("window timezone must not be empty")
    if normalized.lower() == "local":
        return datetime.now().astimezone().tzinfo or timezone.utc
    if normalized.upper() == "UTC":
        return timezone.utc
    try:
        return ZoneInfo(normalized)
    except ZoneInfoNotFoundError as exc:
        raise ValueError(
            f"unknown window timezone '{value}'; use 'local', 'UTC', or an IANA name like Europe/Warsaw"
        ) from exc


def decode_i24_be(data: bytes) -> int:
    value = (data[0] << 16) | (data[1] << 8) | data[2]
    if value & 0x800000:
        value -= 0x1000000
    return value


def raw_lsb_to_m_s2(raw_value: int, range_g: int) -> float:
    scale_g_per_lsb = 3.9e-6
    if range_g == 4:
        scale_g_per_lsb = 7.8e-6
    elif range_g == 8:
        scale_g_per_lsb = 15.6e-6
    return raw_value * scale_g_per_lsb * STANDARD_GRAVITY_M_S2


def decode_packet_samples(
    node_id: int,
    packet_payload: bytes,
    first_sample_seq: int,
    sample_count: int,
    packet_seq: int,
    range_g: int,
) -> list[SampleRecord]:
    expected_size = SAMPLE_PAYLOAD_OFFSET + sample_count * 9
    if len(packet_payload) < expected_size:
        raise ValueError("burst packet payload is shorter than declared sample_count")

    records: list[SampleRecord] = []
    base = SAMPLE_PAYLOAD_OFFSET
    for i in range(sample_count):
        offset = base + i * 9
        records.append(
            SampleRecord(
                node_id=node_id,
                sample_seq=first_sample_seq + i,
                x=raw_lsb_to_m_s2(decode_i24_be(packet_payload[offset:offset + 3]), range_g),
                y=raw_lsb_to_m_s2(decode_i24_be(packet_payload[offset + 3:offset + 6]), range_g),
                z=raw_lsb_to_m_s2(decode_i24_be(packet_payload[offset + 6:offset + 9]), range_g),
                packet_seq=packet_seq,
            )
        )
    return records


def send_and_parse_config(client: ProtocolClient, node_id: int, timeout_s: float) -> ConfigView:
    sequence = client.send_command(node_id, build_get_config_payload())
    response = client.wait_for_response(node_id, sequence, timeout_s)
    if response is None:
        raise RuntimeError(f"node {node_id}: no GetConfig response")
    return parse_config_view(response.payload)


def send_and_parse_buffer_state(client: ProtocolClient, node_id: int, timeout_s: float):
    sequence = client.send_command(node_id, build_buffer_state_payload())
    response = client.wait_for_response(node_id, sequence, timeout_s)
    if response is None:
        raise RuntimeError(f"node {node_id}: no GetBufferState response")
    return parse_buffer_state(response.payload)


def send_and_parse_temperature(client: ProtocolClient, node_id: int, timeout_s: float):
    sequence = client.send_command(node_id, build_get_temperature_payload())
    response = client.wait_for_response(node_id, sequence, timeout_s)
    if response is None:
        raise RuntimeError(f"node {node_id}: no GetTemperature response")
    return parse_temperature_view(response.payload)


def send_commit(client: ProtocolClient, node_id: int, last_sample_seq: int, timeout_s: float) -> int:
    sequence = client.send_command(node_id, build_commit_payload(last_sample_seq))
    response = client.wait_for_response(node_id, sequence, timeout_s)
    if response is None:
        raise RuntimeError(f"node {node_id}: no CommitReadUpTo response")

    _, status, committed_sample_seq = parse_commit_response(response.payload)
    if status != STATUS_OK:
        raise RuntimeError(f"node {node_id}: CommitReadUpTo failed with status={status}")
    return committed_sample_seq


def initialize_node(
    client: ProtocolClient,
    node_id: int,
    start_from: str,
    timeout_s: float,
) -> RecorderNode:
    config = send_and_parse_config(client, node_id, timeout_s)
    state = send_and_parse_buffer_state(client, node_id, timeout_s)
    node = RecorderNode(node_id=node_id, config=config)

    if start_from == "newest":
        newest = state.newest_packet_last_seq or state.newest_seq
        if newest > 0:
            node.committed_sample_seq = send_commit(client, node_id, newest, timeout_s)
        else:
            node.committed_sample_seq = state.committed_sample_seq
        node.expected_sample_seq = node.committed_sample_seq + 1
        return node

    node.committed_sample_seq = state.committed_sample_seq
    if state.oldest_packet_first_seq > 0:
        node.expected_sample_seq = max(state.committed_sample_seq + 1, state.oldest_packet_first_seq)
    else:
        node.expected_sample_seq = state.committed_sample_seq + 1
    return node


def make_single_writer(
    args: argparse.Namespace,
    metadata: dict[str, object],
    output: Path,
    append: bool = False,
) -> BaseWriter:
    if output.exists() and not append and not args.overwrite:
        raise RuntimeError(f"output file already exists: {output}; pass --overwrite to replace it")
    if args.format == "hdf5":
        return Hdf5Writer(output, metadata, args.compression, append=append)
    if args.format == "csv":
        return CsvWriter(output, metadata, append=append)
    raise ValueError(f"unsupported format: {args.format}")


def make_writer(args: argparse.Namespace, metadata: dict[str, object], nodes: list[RecorderNode]) -> BaseWriter:
    if args.output_dir:
        return WindowedWriter(args, metadata, nodes)
    if not args.output:
        raise RuntimeError("either --output or --output-dir must be provided")
    return make_single_writer(args, metadata, Path(args.output))


def refresh_stats(client: ProtocolClient, node: RecorderNode, timeout_s: float) -> None:
    sequence = client.send_command(node.node_id, build_stats_payload())
    response = client.wait_for_response(node.node_id, sequence, timeout_s)
    if response is not None:
        node.last_stats = parse_stats(response.payload)


def node_total_sensor_loss(node: RecorderNode) -> int:
    return node.last_stats.dropped_samples if node.last_stats is not None else 0


def node_total_rx_overflow(node: RecorderNode) -> int:
    return node.last_stats.rx_overflow_count if node.last_stats is not None else 0


def node_total_packet_overwrite(node: RecorderNode) -> int:
    return node.last_stats.packet_overwrite_count if node.last_stats is not None else 0


def node_session_sensor_loss(node: RecorderNode) -> int:
    return max(0, node_total_sensor_loss(node) - node.baseline_sensor_loss)


def node_session_rx_overflow(node: RecorderNode) -> int:
    return max(0, node_total_rx_overflow(node) - node.baseline_rx_overflow_count)


def node_session_packet_overwrite(node: RecorderNode) -> int:
    return max(0, node_total_packet_overwrite(node) - node.baseline_packet_overwrite_count)


def capture_stats_baseline(node: RecorderNode) -> None:
    node.baseline_sensor_loss = node_total_sensor_loss(node)
    node.baseline_rx_overflow_count = node_total_rx_overflow(node)
    node.baseline_packet_overwrite_count = node_total_packet_overwrite(node)


def refresh_temperature(
    client: ProtocolClient,
    writer: BaseWriter,
    node: RecorderNode,
    timeout_s: float,
    sample_seq_anchor: Optional[int] = None,
) -> int:
    temperature = send_and_parse_temperature(client, node.node_id, timeout_s)
    rx_unix_ns = time.time_ns()
    resolved_sample_seq_anchor = (
        sample_seq_anchor if sample_seq_anchor is not None else max(node.last_written_seq, node.committed_sample_seq)
    )
    node.last_temperature_c = temperature.celsius
    node.last_temperature_unix_ns = rx_unix_ns
    writer.write_temperature(
        node.node_id,
        [
            TemperatureRecord(
                node_id=node.node_id,
                sample_seq_anchor=resolved_sample_seq_anchor,
                temp_raw=temperature.raw,
                temp_celsius=temperature.celsius,
            )
        ],
    )
    return resolved_sample_seq_anchor


def emit_temperature_sample_event(
    event_writer: JsonlEventWriter,
    node: RecorderNode,
    sample_seq_anchor: int,
    *,
    reason: str,
    window_start: Optional[datetime] = None,
) -> None:
    fields: dict[str, object] = {
        "sample_seq_anchor": sample_seq_anchor,
        "temp_celsius": node.last_temperature_c,
        "reason": reason,
    }
    if window_start is not None:
        fields["window_start_utc"] = window_start.isoformat()
    event_writer.emit(
        "temperature_sampled",
        node_id=node.node_id,
        fields=fields,
    )


def maybe_refresh_window_start_temperature(
    client: ProtocolClient,
    writer: BaseWriter,
    node: RecorderNode,
    args: argparse.Namespace,
    event_writer: JsonlEventWriter,
    *,
    now_monotonic: float,
    now_utc: Optional[datetime] = None,
) -> None:
    if not isinstance(writer, WindowedWriter):
        return

    window_start = writer.current_window(now_utc)
    if node.last_temperature_window_start == window_start:
        return
    if now_monotonic < node.next_window_temperature_retry_at:
        return

    try:
        sample_seq_anchor = refresh_temperature(
            client,
            writer,
            node,
            args.timeout,
            sample_seq_anchor=node.expected_sample_seq,
        )
        node.last_temperature_window_start = window_start
        node.next_window_temperature_retry_at = 0.0
        if args.temperature_interval > 0:
            node.next_temperature_at = now_monotonic + args.temperature_interval
        emit_temperature_sample_event(
            event_writer,
            node,
            sample_seq_anchor,
            reason="window_start",
            window_start=window_start,
        )
    except RuntimeError as exc:
        print(f"[WARN] {exc}", file=sys.stderr)
        event_writer.emit(
            "temperature_read_failed",
            severity="warning",
            node_id=node.node_id,
            fields={
                "error": str(exc),
                "reason": "window_start",
                "window_start_utc": window_start.isoformat(),
            },
        )
        node.next_window_temperature_retry_at = now_monotonic + max(args.error_sleep, 1.0)


def active_output_path(writer: BaseWriter, args: argparse.Namespace) -> Optional[str]:
    if isinstance(writer, WindowedWriter):
        return str(writer.current_path) if writer.current_path is not None else None
    if args.output:
        return str(Path(args.output))
    return None


def write_runtime_status(
    writer: BaseWriter,
    args: argparse.Namespace,
    nodes: list[RecorderNode],
    started_utc: str,
    status_writer: JsonStatusWriter,
) -> None:
    snapshot = RuntimeStatusSnapshot(
        schema_version=1,
        updated_utc=datetime.now(timezone.utc).isoformat(),
        started_utc=started_utc,
        recorder_version=RECORDER_VERSION,
        destination=args.output_dir or args.output,
        active_file=active_output_path(writer, args),
        port=args.port,
        baud=args.baud,
        channel_name=args.channel_name,
        nodes=[
            RuntimeStatusNode(
                node_id=node.node_id,
                name=None,
                online=node.online,
                sensor_odr_hz=node.config.odr_hz,
                output_odr_hz=effective_output_odr_hz(node.config.odr_hz),
                samples_written=node.samples_written,
                expected_sample_seq=node.expected_sample_seq,
                last_written_seq=node.last_written_seq,
                bursts_ok=node.bursts_ok,
                bursts_no_data=node.bursts_no_data,
                bursts_failed=node.bursts_failed,
                gaps_detected=node.gaps_detected,
                empty_polls=node.empty_polls,
                sensor_loss_total=node_total_sensor_loss(node),
                sensor_loss_session=node_session_sensor_loss(node),
                rx_overflow_total=node_total_rx_overflow(node),
                rx_overflow_session=node_session_rx_overflow(node),
                packet_overwrite_total=node_total_packet_overwrite(node),
                packet_overwrite_session=node_session_packet_overwrite(node),
                baseline_sensor_loss=node.baseline_sensor_loss,
                baseline_rx_overflow_count=node.baseline_rx_overflow_count,
                baseline_packet_overwrite_count=node.baseline_packet_overwrite_count,
                last_temperature_c=node.last_temperature_c,
                last_temperature_unix_ns=node.last_temperature_unix_ns,
            )
            for node in nodes
        ],
    )
    status_writer.write(snapshot)


def record_one_burst(
    client: ProtocolClient,
    writer: BaseWriter,
    node: RecorderNode,
    args: argparse.Namespace,
    event_writer: JsonlEventWriter,
    live: LiveBuffer | None,
) -> None:
    sequence = client.send_command(
        node.node_id,
        build_grant_burst_payload(node.expected_sample_seq, args.grant_packets),
    )
    response = client.wait_for_response(node.node_id, sequence, args.timeout)
    if response is None:
        node.online = False
        node.bursts_failed += 1
        time.sleep(args.idle_sleep)
        return

    _, status, _granted_start_seq, granted_max_frames = parse_grant_burst_response(response.payload)
    if status == STATUS_NO_DATA:
        node.online = True
        node.bursts_no_data += 1
        node.empty_polls += 1
        time.sleep(args.idle_sleep)
        return

    if status != STATUS_OK:
        node.online = False
        node.bursts_failed += 1
        time.sleep(args.error_sleep)
        return

    packets = client.collect_burst_packets(
        node.node_id,
        max_packets=granted_max_frames,
        idle_timeout_s=args.burst_idle_timeout,
        session_timeout_s=args.burst_session_timeout,
    )
    if not packets:
        node.online = False
        node.bursts_failed += 1
        time.sleep(args.error_sleep)
        return

    batch: list[SampleRecord] = []
    expected_seq = node.expected_sample_seq
    last_contiguous_seq = node.committed_sample_seq

    for packet in packets:
        if packet.first_sample_seq != expected_seq:
            node.gaps_detected += 1
            writer.write_gap(
                node.node_id,
                expected_sample_seq=expected_seq,
                received_sample_seq=packet.first_sample_seq,
                packet_seq=packet.packet_seq,
            )
            if live is not None:
                live.publish_gap(
                    LiveGap(
                        node_id=node.node_id,
                        expected_sample_seq=expected_seq,
                        received_sample_seq=packet.first_sample_seq,
                        packet_seq=packet.packet_seq,
                    )
                )
            event_writer.emit(
                "gap_detected",
                severity="warning",
                node_id=node.node_id,
                fields={
                    "expected_sample_seq": expected_seq,
                    "received_sample_seq": packet.first_sample_seq,
                    "packet_seq": packet.packet_seq,
                },
            )
            expected_seq = packet.first_sample_seq

        samples = decode_packet_samples(
            node.node_id,
            packet.payload,
            packet.first_sample_seq,
            packet.sample_count,
            packet.packet_seq,
            node.config.range_g,
        )
        if live is not None and samples:
            live.publish_samples(
                node.node_id,
                [
                    LiveSample(
                        node_id=s.node_id,
                        sample_seq=s.sample_seq,
                        x=s.x,
                        y=s.y,
                        z=s.z,
                        packet_seq=s.packet_seq,
                    )
                    for s in samples
                ],
            )
        batch.extend(samples)
        last_contiguous_seq = packet.first_sample_seq + packet.sample_count - 1
        expected_seq = last_contiguous_seq + 1

    writer.write_samples(node.node_id, batch)
    node.committed_sample_seq = send_commit(
        client,
        node.node_id,
        last_contiguous_seq,
        args.timeout,
    )
    node.expected_sample_seq = node.committed_sample_seq + 1
    node.last_written_seq = last_contiguous_seq
    node.samples_written += len(batch)
    node.bursts_ok += 1
    node.online = True


def print_status(nodes: Iterable[RecorderNode], started_at: float) -> None:
    elapsed_s = max(0.001, time.monotonic() - started_at)
    total_samples = sum(node.samples_written for node in nodes)
    print(
        f"[REC] t={elapsed_s:8.1f}s samples={total_samples} "
        f"rate={total_samples / elapsed_s:7.1f} samples/s"
    )
    for node in nodes:
        stats_suffix = ""
        if node.last_stats is not None:
            stats_suffix = (
                f" sensor_loss={node_session_sensor_loss(node)}"
                f" rx_ovf={node_session_rx_overflow(node)}"
                f" pkt_ovf={node_session_packet_overwrite(node)}"
                f" totals=({node_total_sensor_loss(node)}/{node_total_rx_overflow(node)}/{node_total_packet_overwrite(node)})"
            )
        print(
            f"  node={node.node_id} written={node.samples_written}"
            f" next={node.expected_sample_seq}"
            f" bursts_ok={node.bursts_ok}"
            f" no_data={node.bursts_no_data}"
            f" failed={node.bursts_failed}"
            f" gaps={node.gaps_detected}"
            f"{stats_suffix}"
        )
    sys.stdout.flush()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Sensor System RS485 recorder")
    parser.add_argument("--port", default="/dev/sensor-system-rs485")
    parser.add_argument("--baud", type=int, default=115200)
    parser.add_argument("--channel-name", default="default")
    parser.add_argument("--nodes", type=parse_node_list, default=[1], help="Comma separated node ids, e.g. 1,2")
    parser.add_argument("--output", help="Single output file path, e.g. run.h5 or run.csv")
    parser.add_argument("--output-dir", help="Root directory for rotated output files")
    parser.add_argument("--overwrite", action="store_true", help="Overwrite output file if it already exists")
    parser.add_argument("--format", choices=["hdf5", "csv"], default="hdf5")
    parser.add_argument("--compression", choices=["gzip", "lzf", "none"], default="gzip")
    parser.add_argument(
        "--window-seconds",
        type=int,
        default=DEFAULT_WINDOW_SECONDS,
        help="Rotation window for --output-dir; default creates 10-minute files",
    )
    parser.add_argument(
        "--window-timezone",
        default="local",
        help="Timezone used to align --output-dir windows; use 'local', 'UTC', or an IANA name like Europe/Warsaw",
    )
    parser.add_argument("--duration", type=float, default=0.0, help="Seconds to record; 0 means until Ctrl+C")
    parser.add_argument("--start-from", choices=["newest", "oldest"], default="newest")
    parser.add_argument("--grant-packets", type=int, default=4)
    parser.add_argument("--timeout", type=float, default=0.5)
    parser.add_argument("--burst-idle-timeout", type=float, default=0.15)
    parser.add_argument("--burst-session-timeout", type=float, default=0.75)
    parser.add_argument("--status-interval", type=float, default=1.0)
    parser.add_argument("--flush-interval", type=float, default=2.0)
    parser.add_argument("--stats-interval", type=float, default=5.0)
    parser.add_argument(
        "--temperature-interval",
        type=float,
        default=3600.0,
        help="Seconds between periodic temperature reads; 0 disables periodic reads but rotated files still get a window-start sample",
    )
    parser.add_argument("--status-file", default="/tmp/sensor-system_recorder_status.json", help="JSON runtime status for the operator console")
    parser.add_argument("--event-log", default="/tmp/sensor-system_recorder_events.jsonl", help="JSONL event log for recorder lifecycle and warnings")
    parser.add_argument("--idle-sleep", type=float, default=0.01)
    parser.add_argument("--error-sleep", type=float, default=0.10)
    parser.add_argument("--live", action="store_true", help="Serve a minimal live web UI (time plot + FFT) over HTTP/SSE")
    parser.add_argument("--live-host", default="0.0.0.0")
    parser.add_argument("--live-port", type=int, default=8000)
    args = parser.parse_args()
    if not args.output and not args.output_dir:
        parser.error("one of --output or --output-dir is required")
    if args.output and args.output_dir:
        parser.error("use either --output or --output-dir, not both")
    if args.window_seconds <= 0:
        parser.error("--window-seconds must be greater than zero")
    args.window_timezone_name = args.window_timezone
    try:
        args.window_timezone = resolve_window_timezone(args.window_timezone_name)
    except ValueError as exc:
        parser.error(str(exc))
    return args


def main() -> int:
    args = parse_args()
    started_at = time.monotonic()
    created_utc = datetime.now(timezone.utc).isoformat()
    stop_reason = "running"
    stop_flag = StopFlag()
    status_writer = JsonStatusWriter(args.status_file)
    event_writer = JsonlEventWriter(args.event_log)
    signal.signal(signal.SIGINT, stop_flag.request_stop)
    signal.signal(signal.SIGTERM, stop_flag.request_stop)

    metadata: dict[str, object] = {
        "created_utc": created_utc,
        "host_node_id": HOST_NODE_ID,
        "channel_name": args.channel_name,
        "port": args.port,
        "baud": args.baud,
        "start_from": args.start_from,
        "grant_packets": args.grant_packets,
        "window_seconds": args.window_seconds,
        "window_timezone": args.window_timezone_name,
        "recorder_version": RECORDER_VERSION,
        "host_name": socket.gethostname(),
    }

    try:
        live_buffer: LiveBuffer | None = None
        live_server: LiveServer | None = None
        with serial.Serial(
            port=args.port,
            baudrate=args.baud,
            timeout=0.03,
            write_timeout=0.5,
        ) as ser:
            client = ProtocolClient(ser)
            nodes = [
                initialize_node(client, node_id, args.start_from, args.timeout)
                for node_id in args.nodes
            ]
            for node in nodes:
                refresh_stats(client, node, args.timeout)
                capture_stats_baseline(node)

            if args.live:
                live_buffer = LiveBuffer()
                live_buffer.set_meta(
                    {
                        "created_utc": created_utc,
                        "port": args.port,
                        "baud": args.baud,
                        "output_odr_hz": effective_output_odr_hz(nodes[0].config.odr_hz) if nodes else 0.0,
                        "nodes": [
                            {
                                "node_id": n.node_id,
                                "sensor_odr_hz": n.config.odr_hz,
                                "output_odr_hz": effective_output_odr_hz(n.config.odr_hz),
                                "range_g": n.config.range_g,
                                "high_pass_corner": n.config.high_pass_corner,
                                "fifo_watermark": n.config.fifo_watermark,
                            }
                            for n in nodes
                        ],
                    }
                )
                live_server = LiveServer(args.live_host, args.live_port, live_buffer)
                live_server.start()
                print(f"[LIVE] http://{args.live_host}:{args.live_port}/")

            writer = make_writer(args, metadata, nodes)
            try:
                event_writer.emit(
                    "recorder_started",
                    fields={
                        "channel_name": args.channel_name,
                        "port": args.port,
                        "baud": args.baud,
                        "destination": args.output_dir or args.output,
                        "node_ids": list(args.nodes),
                    },
                )
                for node in nodes:
                    writer.add_node(node)
                    node.next_temperature_at = started_at
                    node.online = True
                    event_writer.emit(
                        "node_initialized",
                        node_id=node.node_id,
                        fields={
                            "sensor_odr_hz": node.config.odr_hz,
                            "output_odr_hz": effective_output_odr_hz(node.config.odr_hz),
                            "range_g": node.config.range_g,
                            "high_pass_corner": node.config.high_pass_corner,
                            "baseline_sensor_loss": node.baseline_sensor_loss,
                            "baseline_rx_overflow_count": node.baseline_rx_overflow_count,
                            "baseline_packet_overwrite_count": node.baseline_packet_overwrite_count,
                        },
                    )
                    print(
                        f"[INIT] channel={args.channel_name} node={node.node_id} "
                        f"sensor_odr={node.config.odr_hz}Hz "
                        f"output_odr={effective_output_odr_hz(node.config.odr_hz):g}Hz "
                        f"range={node.config.range_g}g "
                        f"high_pass={node.config.high_pass_corner} "
                        f"fifo_watermark={node.config.fifo_watermark} "
                        f"start_seq={node.expected_sample_seq}"
                    )

                next_status_at = started_at
                next_flush_at = started_at + args.flush_interval
                next_stats_at = started_at + args.stats_interval

                while not stop_flag.stop_requested:
                    now = time.monotonic()
                    if args.duration > 0 and (now - started_at) >= args.duration:
                        stop_reason = "duration_elapsed"
                        break

                    loop_now_utc = datetime.now(timezone.utc)
                    for node in nodes:
                        maybe_refresh_window_start_temperature(
                            client,
                            writer,
                            node,
                            args,
                            event_writer,
                            now_monotonic=now,
                            now_utc=loop_now_utc,
                        )
                        record_one_burst(client, writer, node, args, event_writer, live_buffer)

                    now = time.monotonic()
                    if args.temperature_interval > 0:
                        for node in nodes:
                            if now >= node.next_temperature_at:
                                try:
                                    sample_seq_anchor = refresh_temperature(client, writer, node, args.timeout)
                                    if isinstance(writer, WindowedWriter):
                                        node.last_temperature_window_start = writer.current_window()
                                        node.next_window_temperature_retry_at = 0.0
                                    emit_temperature_sample_event(
                                        event_writer,
                                        node,
                                        sample_seq_anchor,
                                        reason="periodic",
                                    )
                                except RuntimeError as exc:
                                    print(f"[WARN] {exc}", file=sys.stderr)
                                    event_writer.emit(
                                        "temperature_read_failed",
                                        severity="warning",
                                        node_id=node.node_id,
                                        fields={"error": str(exc), "reason": "periodic"},
                                    )
                                node.next_temperature_at = now + args.temperature_interval
                    if now >= next_stats_at:
                        for node in nodes:
                            refresh_stats(client, node, args.timeout)
                        next_stats_at = now + args.stats_interval

                    if now >= next_flush_at:
                        writer.flush()
                        next_flush_at = now + args.flush_interval

                    if now >= next_status_at:
                        print_status(nodes, started_at)
                        write_runtime_status(writer, args, nodes, created_utc, status_writer)
                        next_status_at = now + args.status_interval
            finally:
                if stop_flag.stop_requested:
                    stop_reason = f"signal:{stop_flag.signal_name or stop_flag.signal_number}"
                write_runtime_status(writer, args, nodes, created_utc, status_writer)
                writer.close()
                if live_server is not None:
                    live_server.stop()
                event_writer.emit(
                    "recorder_stopped",
                    fields={
                        "channel_name": args.channel_name,
                        "destination": args.output_dir or args.output,
                        "samples_written": sum(node.samples_written for node in nodes),
                        "stop_reason": stop_reason,
                        "signal_number": stop_flag.signal_number,
                        "signal_name": stop_flag.signal_name,
                    },
                )

        print_status(nodes, started_at)
        destination = args.output_dir or args.output
        print(f"[DONE] wrote {destination}")
        return 0 if all(node.gaps_detected == 0 for node in nodes) else 2
    except serial.SerialException as exc:
        event_writer.emit(
            "serial_error",
            severity="error",
            fields={"channel_name": args.channel_name, "port": args.port, "error": str(exc)},
        )
        print(f"[ERROR] serial error on {args.port}: {exc}", file=sys.stderr)
        return 1
    except RuntimeError as exc:
        event_writer.emit(
            "runtime_error",
            severity="error",
            fields={"channel_name": args.channel_name, "error": str(exc)},
        )
        print(f"[ERROR] {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
