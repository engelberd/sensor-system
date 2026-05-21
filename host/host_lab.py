#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import math
import os
import struct
import sys
import time
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Optional

import serial


FRAME_MAGIC = 0xAA55
FRAME_PROTOCOL_VERSION = 2
FRAME_HEADER_FORMAT = "<HBBBBBHI"
FRAME_HEADER_SIZE = struct.calcsize(FRAME_HEADER_FORMAT)
FRAME_CRC_SIZE = 2
FRAME_MAX_PAYLOAD_SIZE = 1024

FRAME_TYPE_DATA = 0x01
FRAME_TYPE_COMMAND = 0x02
FRAME_TYPE_RESPONSE = 0x03

HOST_NODE_ID = 0xFE

CMD_PING = 0x01
CMD_GET_CONFIG = 0x20
CMD_SET_ODR = 0x22
CMD_SET_HIGH_PASS = 0x2C
CMD_SET_FIFO_WATERMARK = 0x25
CMD_SAVE_CONFIG = 0x26
CMD_SET_BAUD_RATE = 0x29
CMD_GET_TEMPERATURE = 0x41
CMD_GET_BUFFER_STATE = 0x42
CMD_GET_STATS = 0x43
CMD_GRANT_BURST_READ = 0x52
CMD_COMMIT_READ_UP_TO = 0x53

STATUS_OK = 0x00
STATUS_NO_DATA = 0x06

SAMPLE_ENCODING_RAW_XYZ24 = 0x01
SAMPLES_PER_PACKET = 32

BUFFER_STATE_FORMAT = "<BBQQIIIQQQIII"
STATS_FORMAT = "<BBQ" + ("I" * 12)
GRANT_BURST_RESPONSE_FORMAT = "<BBQH"
COMMIT_READ_RESPONSE_FORMAT = "<BBQ"
BURST_HEADER_FORMAT = "<BBIQHB"
PACKET_FRAME_BYTES = FRAME_HEADER_SIZE + struct.calcsize(BURST_HEADER_FORMAT) + (SAMPLES_PER_PACKET * 9) + FRAME_CRC_SIZE
CONTROL_RESPONSE_BYTES = FRAME_HEADER_SIZE + 16 + FRAME_CRC_SIZE
GET_CONFIG_FORMAT = "<BBBIHBiiiBHBB"
GET_TEMPERATURE_FORMAT = "<BBHf"

SUPPORTED_ODR_HZ = (
    4000,
    2000,
    1000,
    500,
    250,
    125,
    # Lower ADXL355 ODR values are intentionally disabled in host commands
    # until firmware support is wired end-to-end and verified on hardware.
    # 62.5,
    # 31.25,
    # 15.625,
    # 7.813,
    # 3.906,
    # 1.953,
    # 0.977,
)
SUPPORTED_HIGH_PASS_CORNERS = tuple(range(0, 8))
SUPPORTED_FIFO_WATERMARKS = tuple(range(3, 97, 3))
SUPPORTED_BAUD_RATES = (
    9600,
    19200,
    38400,
    57600,
    115200,
    # Higher baudrates are intentionally disabled in host commands until
    # firmware support is wired end-to-end and verified on hardware.
    # 230400,
    # 460800,
    # 921600,
    # 1000000,
    # 1500000,
    # 2000000,
)
OUTPUT_DECIMATION_FACTOR = 2
SUPPORTED_OUTPUT_ODR_HZ = tuple(
    value / float(OUTPUT_DECIMATION_FACTOR) for value in SUPPORTED_ODR_HZ
)


def crc16_ccitt(data: bytes) -> int:
    crc = 0xFFFF
    for byte in data:
        crc ^= (byte << 8) & 0xFFFF
        for _ in range(8):
            if crc & 0x8000:
                crc = ((crc << 1) ^ 0x1021) & 0xFFFF
            else:
                crc = (crc << 1) & 0xFFFF
    return crc


def load_json_config(path: Path) -> dict:
    if not path.exists():
        return {}

    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


@dataclass
class Frame:
    frame_type: int
    flags: int
    destination: int
    source: int
    sequence: int
    payload: bytes


@dataclass
class BufferState:
    command: int
    status: int
    oldest_seq: int
    newest_seq: int
    stored_samples: int
    capacity_samples: int
    overwrite_count: int
    oldest_packet_first_seq: int
    newest_packet_last_seq: int
    committed_sample_seq: int
    queued_packets: int
    packet_capacity: int
    packet_overwrite_count: int


@dataclass
class NodeStats:
    command: int
    status: int
    next_sample_seq: int
    pushed_samples: int
    dropped_samples: int
    sample_buffer_overwrite_count: int
    update_calls: int
    fifo_reads: int
    fifo_no_data: int
    sensor_errors: int
    fifo_irq_events: int
    fifo_batches: int
    fifo_samples_read: int
    rx_overflow_count: int
    packet_overwrite_count: int


@dataclass
class BurstPacket:
    packet_seq: int
    first_sample_seq: int
    sample_count: int
    payload: bytes


@dataclass
class SweepResult:
    baudrate: int
    odr_hz: int
    fifo_watermark: int
    total_nodes: int
    virtual_nodes: int
    grant_packets: int
    start_from: str
    verdict: str
    reason: str
    packet_capacity: int
    buffer_capacity_samples: int
    packet_retention_s: float
    buffer_retention_s: float
    real_bursts_ok: int
    real_avg_lag: float
    real_max_lag: int
    real_loss_delta: int
    real_packet_overwrite_delta: int
    real_sample_buffer_overwrite_delta: int
    real_rx_overflow_delta: int


@dataclass
class LabConfig:
    port: str = "/dev/sensor-system-rs485"
    baud: int = 115200
    node: int = 1
    nodes: tuple[int, ...] = (1, 2)
    timeout: float = 0.20
    burst_idle_timeout: float = 0.15
    burst_session_timeout: float = 0.50
    grant_packets: int = 4
    duration_s: float = 0.0
    refresh_s: float = 0.25
    buffer_poll_s: float = 0.50
    stats_poll_s: float = 2.0
    start_from: str = "newest"
    dead_node_fail_threshold: int = 3
    dead_node_retry_s: float = 5.0
    virtual_nodes: tuple[int, ...] = ()
    virtual_odr_hz: float = 125.0
    virtual_packet_capacity: int = 128
    live: bool = False
    report_format: str = "plain"
    report_file: str = ""
    abort_on_error: bool = False
    config_node_ports: dict[int, str] | None = None

    @classmethod
    def from_dict(cls, data: dict) -> "LabConfig":
        raw_nodes = data.get("nodes", list(cls.nodes))
        nodes = tuple(int(node) for node in raw_nodes) if raw_nodes else cls.nodes
        raw_config_node_ports = data.get("config_node_ports", {})
        config_node_ports = {
            int(node_id): str(port)
            for node_id, port in raw_config_node_ports.items()
        } if raw_config_node_ports else None
        return cls(
            port=str(data.get("port", cls.port)),
            baud=int(data.get("baud", cls.baud)),
            node=int(data.get("node", cls.node)),
            nodes=nodes,
            timeout=float(data.get("timeout", data.get("cmd_timeout_s", cls.timeout))),
            burst_idle_timeout=float(data.get("burst_idle_timeout", data.get("burst_timeout_s", cls.burst_idle_timeout))),
            burst_session_timeout=float(data.get("burst_session_timeout", cls.burst_session_timeout)),
            grant_packets=int(data.get("grant_packets", data.get("max_frames", cls.grant_packets))),
            duration_s=float(data.get("duration_s", cls.duration_s)),
            refresh_s=float(data.get("refresh_s", cls.refresh_s)),
            buffer_poll_s=float(data.get("buffer_poll_s", cls.buffer_poll_s)),
            stats_poll_s=float(data.get("stats_poll_s", cls.stats_poll_s)),
            start_from=str(data.get("start_from", cls.start_from)),
            dead_node_fail_threshold=int(data.get("dead_node_fail_threshold", cls.dead_node_fail_threshold)),
            dead_node_retry_s=float(data.get("dead_node_retry_s", cls.dead_node_retry_s)),
            virtual_nodes=tuple(int(node) for node in data.get("virtual_nodes", list(cls.virtual_nodes))),
            virtual_odr_hz=float(data.get("virtual_odr_hz", cls.virtual_odr_hz)),
            virtual_packet_capacity=int(data.get("virtual_packet_capacity", cls.virtual_packet_capacity)),
            live=bool(data.get("live", cls.live)),
            report_format=str(data.get("report_format", cls.report_format)),
            report_file=str(data.get("report_file", cls.report_file)),
            abort_on_error=bool(data.get("abort_on_error", cls.abort_on_error)),
            config_node_ports=config_node_ports,
        )


@dataclass
class DrainNodeState:
    node_id: int
    is_virtual: bool = False
    virtual_odr_hz: float = 0.0
    initialized_window: bool = False
    committed_sample_seq: int = 0
    expected_next_sample_seq: int = 0
    newest_sample_seq: int = 0
    queued_packets: int = 0
    packet_capacity: int = 0
    buffer_capacity_samples: int = 0
    packet_overwrite_count: int = 0
    dropped_samples: int = 0
    sample_buffer_overwrite_count: int = 0
    rx_overflow_count: int = 0
    lag_samples: int = 0
    window_valid: bool = True
    online: bool = False
    failures: int = 0
    bursts_ok: int = 0
    bursts_failed: int = 0
    gaps_detected: int = 0
    last_cmd_rtt_ms: float = 0.0
    last_update_monotonic: float = 0.0
    next_buffer_poll_at: float = 0.0
    next_stats_poll_at: float = 0.0
    next_service_at: float = 0.0
    skip_until: float = 0.0
    virtual_started_monotonic: float = 0.0
    max_lag_samples: int = 0
    lag_sum_samples: int = 0
    lag_samples_count: int = 0
    initial_packet_overwrite_count: int = 0
    initial_sample_buffer_overwrite_count: int = 0
    initial_dropped_samples: int = 0
    initial_rx_overflow_count: int = 0


@dataclass
class ConfigView:
    command: int
    status: int
    node_id: int
    baudrate: int
    odr_hz: int
    range_g: int
    offset_x: int
    offset_y: int
    offset_z: int
    fifo_watermark: int
    act_threshold: int
    act_count: int
    high_pass_corner: int


@dataclass
class TemperatureView:
    command: int
    status: int
    raw: int
    celsius: float


class FrameCodec:
    @staticmethod
    def encode(
        frame_type: int,
        destination: int,
        source: int,
        sequence: int,
        payload: bytes,
    ) -> bytes:
        header = struct.pack(
            FRAME_HEADER_FORMAT,
            FRAME_MAGIC,
            FRAME_PROTOCOL_VERSION,
            frame_type,
            0,
            destination,
            source,
            len(payload),
            sequence,
        )
        crc = crc16_ccitt(header + payload)
        return header + payload + struct.pack("<H", crc)

    @staticmethod
    def try_decode(rx_buffer: bytearray) -> Optional[Frame]:
        while len(rx_buffer) >= 2:
            if rx_buffer[0] == (FRAME_MAGIC & 0xFF) and rx_buffer[1] == ((FRAME_MAGIC >> 8) & 0xFF):
                break
            del rx_buffer[0]

        if len(rx_buffer) < FRAME_HEADER_SIZE:
            return None

        header = bytes(rx_buffer[:FRAME_HEADER_SIZE])
        try:
            magic, version, frame_type, flags, destination, source, payload_length, sequence = struct.unpack(
                FRAME_HEADER_FORMAT,
                header,
            )
        except struct.error:
            return None

        if magic != FRAME_MAGIC or version != FRAME_PROTOCOL_VERSION or payload_length > FRAME_MAX_PAYLOAD_SIZE:
            del rx_buffer[0]
            return None

        total_length = FRAME_HEADER_SIZE + payload_length + FRAME_CRC_SIZE
        if len(rx_buffer) < total_length:
            return None

        raw_frame = bytes(rx_buffer[:total_length])
        del rx_buffer[:total_length]

        expected_crc = struct.unpack("<H", raw_frame[-2:])[0]
        calculated_crc = crc16_ccitt(raw_frame[:-2])
        if expected_crc != calculated_crc:
            return None

        return Frame(
            frame_type=frame_type,
            flags=flags,
            destination=destination,
            source=source,
            sequence=sequence,
            payload=raw_frame[FRAME_HEADER_SIZE:-2],
        )


class ProtocolClient:
    def __init__(self, ser: serial.Serial, host_node_id: int = HOST_NODE_ID) -> None:
        self.ser = ser
        self.host_node_id = host_node_id
        self.rx_buffer = bytearray()
        self.sequence = 1
        self.pending_frames: list[Frame] = []

    def next_sequence(self) -> int:
        current = self.sequence
        self.sequence = 1 if self.sequence == 0xFFFFFFFF else self.sequence + 1
        return current

    def send_command(self, node_id: int, payload: bytes) -> int:
        sequence = self.next_sequence()
        frame = FrameCodec.encode(
            frame_type=FRAME_TYPE_COMMAND,
            destination=node_id,
            source=self.host_node_id,
            sequence=sequence,
            payload=payload,
        )
        self.ser.write(frame)
        self.ser.flush()
        return sequence

    def poll_frames(self, timeout_s: float) -> list[Frame]:
        frames: list[Frame] = []
        deadline = time.monotonic() + timeout_s

        while time.monotonic() < deadline:
            chunk = self.ser.read(256)
            if chunk:
                self.rx_buffer.extend(chunk)

            while True:
                before = len(self.rx_buffer)
                frame = FrameCodec.try_decode(self.rx_buffer)
                if frame is None:
                    if len(self.rx_buffer) == before:
                        break
                    continue
                frames.append(frame)

            if frames:
                break

        if frames:
            self.pending_frames.extend(frames)

        ready = self.pending_frames
        self.pending_frames = []
        return ready

    def wait_for_response(self, node_id: int, sequence: int, timeout_s: float) -> Optional[Frame]:
        deadline = time.monotonic() + timeout_s
        stash: list[Frame] = []

        while time.monotonic() < deadline:
            for frame in self.poll_frames(0.03):
                if (
                    frame.frame_type == FRAME_TYPE_RESPONSE
                    and frame.source == node_id
                    and frame.destination == self.host_node_id
                    and frame.sequence == sequence
                ):
                    self.pending_frames.extend(stash)
                    return frame
                stash.append(frame)

        self.pending_frames.extend(stash)
        return None

    def collect_burst_packets(
        self,
        node_id: int,
        max_packets: int,
        idle_timeout_s: float,
        session_timeout_s: float,
    ) -> list[BurstPacket]:
        deadline = time.monotonic() + session_timeout_s
        idle_deadline = time.monotonic() + idle_timeout_s
        packets: list[BurstPacket] = []
        stash: list[Frame] = []

        while time.monotonic() < deadline and len(packets) < max_packets:
            new_frames = self.poll_frames(0.03)
            if new_frames:
                idle_deadline = time.monotonic() + idle_timeout_s

            for frame in new_frames:
                if (
                    frame.frame_type == FRAME_TYPE_DATA
                    and frame.source == node_id
                    and frame.destination == self.host_node_id
                ):
                    packet = parse_burst_packet(frame.payload)
                    if packet is not None:
                        packets.append(packet)
                        if len(packets) >= max_packets:
                            break
                else:
                    stash.append(frame)

            if time.monotonic() >= idle_deadline:
                break

        self.pending_frames.extend(stash)
        return packets


def parse_buffer_state(payload: bytes) -> BufferState:
    values = struct.unpack(BUFFER_STATE_FORMAT, payload[: struct.calcsize(BUFFER_STATE_FORMAT)])
    return BufferState(*values)


def parse_stats(payload: bytes) -> NodeStats:
    values = struct.unpack(STATS_FORMAT, payload[: struct.calcsize(STATS_FORMAT)])
    return NodeStats(*values)


def parse_config_view(payload: bytes) -> ConfigView:
    values = struct.unpack(GET_CONFIG_FORMAT, payload[: struct.calcsize(GET_CONFIG_FORMAT)])
    return ConfigView(*values)


def parse_temperature_view(payload: bytes) -> TemperatureView:
    values = struct.unpack(GET_TEMPERATURE_FORMAT, payload[: struct.calcsize(GET_TEMPERATURE_FORMAT)])
    return TemperatureView(*values)


def parse_grant_burst_response(payload: bytes) -> tuple[int, int, int, int]:
    return struct.unpack(GRANT_BURST_RESPONSE_FORMAT, payload[: struct.calcsize(GRANT_BURST_RESPONSE_FORMAT)])


def parse_commit_response(payload: bytes) -> tuple[int, int, int]:
    return struct.unpack(COMMIT_READ_RESPONSE_FORMAT, payload[: struct.calcsize(COMMIT_READ_RESPONSE_FORMAT)])


def parse_burst_packet(payload: bytes) -> Optional[BurstPacket]:
    if len(payload) < struct.calcsize(BURST_HEADER_FORMAT):
        return None

    command, status, packet_seq, first_sample_seq, sample_count, sample_encoding = struct.unpack(
        BURST_HEADER_FORMAT,
        payload[: struct.calcsize(BURST_HEADER_FORMAT)],
    )
    if command != CMD_GRANT_BURST_READ or status != STATUS_OK or sample_encoding != SAMPLE_ENCODING_RAW_XYZ24:
        return None

    return BurstPacket(
        packet_seq=packet_seq,
        first_sample_seq=first_sample_seq,
        sample_count=sample_count,
        payload=payload,
    )


def build_ping_payload(user_payload: bytes) -> bytes:
    return bytes([CMD_PING]) + user_payload


def build_buffer_state_payload() -> bytes:
    return bytes([CMD_GET_BUFFER_STATE])


def build_stats_payload() -> bytes:
    return bytes([CMD_GET_STATS])


def build_get_config_payload() -> bytes:
    return bytes([CMD_GET_CONFIG])


def build_get_temperature_payload() -> bytes:
    return bytes([CMD_GET_TEMPERATURE])


def build_set_odr_payload(odr_hz: int) -> bytes:
    validate_odr_hz(odr_hz)
    return struct.pack("<BH", CMD_SET_ODR, odr_hz)


def build_set_high_pass_payload(high_pass_corner: int) -> bytes:
    validate_high_pass_corner(high_pass_corner)
    return struct.pack("<BB", CMD_SET_HIGH_PASS, high_pass_corner)


def build_set_watermark_payload(watermark: int) -> bytes:
    validate_fifo_watermark(watermark)
    return struct.pack("<BB", CMD_SET_FIFO_WATERMARK, watermark)


def build_set_baudrate_payload(baudrate: int) -> bytes:
    if baudrate not in SUPPORTED_BAUD_RATES:
        choices = ", ".join(str(value) for value in SUPPORTED_BAUD_RATES)
        raise ValueError(f"unsupported baudrate {baudrate}; expected one of: {choices}")
    return struct.pack("<BI", CMD_SET_BAUD_RATE, baudrate)


def build_grant_burst_payload(start_seq: int, max_packets: int) -> bytes:
    return struct.pack("<BQH", CMD_GRANT_BURST_READ, start_seq, max_packets)


def build_commit_payload(last_sample_seq: int) -> bytes:
    return struct.pack("<BQ", CMD_COMMIT_READ_UP_TO, last_sample_seq)


def parse_int_list(value: str) -> tuple[int, ...]:
    return tuple(int(part.strip()) for part in value.split(",") if part.strip())


def validate_odr_hz(odr_hz: int) -> None:
    if odr_hz not in SUPPORTED_ODR_HZ:
        choices = ", ".join(str(value) for value in SUPPORTED_ODR_HZ)
        raise ValueError(f"unsupported ODR {odr_hz} Hz; expected one of: {choices}")


def validate_high_pass_corner(high_pass_corner: int) -> None:
    if high_pass_corner not in SUPPORTED_HIGH_PASS_CORNERS:
        choices = ", ".join(str(value) for value in SUPPORTED_HIGH_PASS_CORNERS)
        raise ValueError(
            f"unsupported high-pass corner {high_pass_corner}; expected one of: {choices}"
        )


def parse_odr_list(value: str) -> tuple[int, ...]:
    odrs = parse_int_list(value)
    for odr_hz in odrs:
        validate_odr_hz(odr_hz)
    return odrs


def parse_sweep_odr_list(value: str) -> tuple[int, ...]:
    sensor_odrs: list[int] = []
    output_to_sensor = {
        effective_output_odr_hz(sensor_odr_hz): sensor_odr_hz
        for sensor_odr_hz in SUPPORTED_ODR_HZ
    }

    for raw_part in value.split(","):
        token = raw_part.strip()
        if not token:
            continue

        try:
            numeric = float(token)
        except ValueError as exc:
            raise ValueError(f"unsupported ODR '{token}'") from exc

        if numeric.is_integer() and int(numeric) in SUPPORTED_ODR_HZ:
            sensor_odrs.append(int(numeric))
            continue

        sensor_odr = output_to_sensor.get(numeric)
        if sensor_odr is not None:
            sensor_odrs.append(sensor_odr)
            continue

        sensor_int = int(round(numeric * OUTPUT_DECIMATION_FACTOR))
        if math.isclose(effective_output_odr_hz(sensor_int), numeric, rel_tol=0.0, abs_tol=1e-6) and sensor_int in SUPPORTED_ODR_HZ:
            sensor_odrs.append(sensor_int)
            continue

        sensor_choices = ", ".join(str(item) for item in SUPPORTED_ODR_HZ)
        output_choices = ", ".join(f"{item:g}" for item in SUPPORTED_OUTPUT_ODR_HZ)
        raise ValueError(
            f"unsupported ODR '{token}'; expected sensor ODR one of [{sensor_choices}] "
            f"or output ODR one of [{output_choices}]"
        )

    return tuple(sensor_odrs)


def validate_fifo_watermark(watermark: int) -> None:
    if watermark not in SUPPORTED_FIFO_WATERMARKS:
        raise ValueError("unsupported FIFO watermark; expected a multiple of 3 from 3 to 96")


def effective_output_odr_hz(sensor_odr_hz: int) -> float:
    return sensor_odr_hz / float(OUTPUT_DECIMATION_FACTOR)


def format_effective_output_odr(sensor_odr_hz: int) -> str:
    output_odr = effective_output_odr_hz(sensor_odr_hz)
    if output_odr.is_integer():
        return str(int(output_odr))
    return f"{output_odr:g}"


def normalize_fifo_watermark(watermark: int) -> int:
    watermark = max(3, min(96, watermark))
    watermark -= watermark % 3
    return max(3, watermark)


def parse_fifo_watermark_list(value: str) -> tuple[int, ...]:
    watermarks = parse_int_list(value)
    for watermark in watermarks:
        validate_fifo_watermark(watermark)
    return watermarks


def parse_str_list(value: str) -> tuple[str, ...]:
    return tuple(part.strip() for part in value.split(",") if part.strip())


def parse_baud_list(value: str) -> tuple[int, ...]:
    baudrates = parse_int_list(value)
    for baudrate in baudrates:
        if baudrate not in SUPPORTED_BAUD_RATES:
            choices = ", ".join(str(item) for item in SUPPORTED_BAUD_RATES)
            raise ValueError(f"unsupported baudrate {baudrate}; expected one of: {choices}")
    return baudrates


def parse_node_port_map(value: str) -> dict[int, str]:
    mapping: dict[int, str] = {}
    for entry in parse_str_list(value):
        if "=" not in entry:
            raise ValueError(f"invalid node-port mapping '{entry}'; expected node=/dev/ttyXYZ")
        node_text, port = entry.split("=", 1)
        mapping[int(node_text.strip())] = port.strip()
    return mapping


def resolve_config(args: argparse.Namespace) -> LabConfig:
    config_path = Path(args.config)
    if not config_path.is_absolute() and not config_path.exists():
        config_path = Path(__file__).resolve().parent / args.config

    config = LabConfig.from_dict(load_json_config(config_path))

    nodes = config.nodes
    if args.nodes:
        nodes = tuple(int(value) for value in args.nodes.split(",") if value.strip())

    virtual_nodes = config.virtual_nodes
    if args.virtual_nodes:
        virtual_nodes = tuple(int(value) for value in args.virtual_nodes.split(",") if value.strip())

    return LabConfig(
        port=args.port if args.port is not None else config.port,
        baud=args.baud if args.baud is not None else config.baud,
        node=args.node if args.node is not None else config.node,
        nodes=nodes,
        timeout=args.timeout if args.timeout is not None else config.timeout,
        burst_idle_timeout=args.burst_idle_timeout if args.burst_idle_timeout is not None else config.burst_idle_timeout,
        burst_session_timeout=args.burst_session_timeout if args.burst_session_timeout is not None else config.burst_session_timeout,
        grant_packets=args.grant_packets if args.grant_packets is not None else config.grant_packets,
        duration_s=args.duration if args.duration is not None else config.duration_s,
        refresh_s=args.refresh if args.refresh is not None else config.refresh_s,
        buffer_poll_s=args.buffer_poll if args.buffer_poll is not None else config.buffer_poll_s,
        stats_poll_s=args.stats_poll if args.stats_poll is not None else config.stats_poll_s,
        start_from=args.start_from if args.start_from is not None else config.start_from,
        dead_node_fail_threshold=config.dead_node_fail_threshold,
        dead_node_retry_s=config.dead_node_retry_s,
        virtual_nodes=virtual_nodes,
        virtual_odr_hz=args.virtual_odr if args.virtual_odr is not None else config.virtual_odr_hz,
        virtual_packet_capacity=args.virtual_packet_capacity if args.virtual_packet_capacity is not None else config.virtual_packet_capacity,
        live=args.live or config.live,
        report_format=args.report_format if args.report_format is not None else config.report_format,
        report_file=args.report_file if args.report_file is not None else config.report_file,
        abort_on_error=getattr(args, "abort_on_error", False) or config.abort_on_error,
        config_node_ports=parse_node_port_map(getattr(args, "config_node_ports"))
        if getattr(args, "config_node_ports", None)
        else config.config_node_ports,
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Terminal lab host for sensor-system RS485 nodes")
    parser.add_argument("--config", default="host_config.json")
    parser.add_argument("--port")
    parser.add_argument("--baud", type=int)
    parser.add_argument("--node", type=int)
    parser.add_argument("--timeout", type=float)
    parser.add_argument("--burst-idle-timeout", type=float)
    parser.add_argument("--burst-session-timeout", type=float)
    parser.add_argument("--grant-packets", type=int)
    parser.add_argument("--duration", type=float)
    parser.add_argument("--refresh", type=float)
    parser.add_argument("--buffer-poll", type=float)
    parser.add_argument("--stats-poll", type=float)
    parser.add_argument("--nodes", help="Comma separated node ids, e.g. 1,2")
    parser.add_argument("--virtual-nodes", help="Comma separated simulated node ids, e.g. 2")
    parser.add_argument("--virtual-odr", type=float, choices=SUPPORTED_OUTPUT_ODR_HZ, help="Output ODR for all simulated nodes")
    parser.add_argument("--virtual-packet-capacity", type=int, help="Retained packet capacity for simulated nodes")
    parser.add_argument("--start-from", choices=["oldest", "newest"])
    parser.add_argument("--live", action="store_true", help="Show live table during the test")
    parser.add_argument("--report-format", choices=["plain", "markdown", "json"])
    parser.add_argument("--report-file")

    sub = parser.add_subparsers(dest="command", required=True)

    ping = sub.add_parser("ping")
    ping.add_argument("--payload", default="ping")

    sub.add_parser("buffer")
    sub.add_parser("stats")
    sub.add_parser("drain")
    baud_check = sub.add_parser("baud-check")
    baud_check.add_argument("--expect-baud", type=int, choices=SUPPORTED_BAUD_RATES)
    baud_check.add_argument("--require-all-same", action="store_true")

    baud_set = sub.add_parser("baud-set")
    baud_set.add_argument("value", type=int, choices=SUPPORTED_BAUD_RATES)
    baud_set.add_argument("--save", action="store_true")
    baud_set.add_argument("--settle-ms", type=int, default=100)
    baud_set.add_argument("--verify-channel", action="store_true")

    sweep = sub.add_parser("sweep")
    sweep.add_argument("--sweep-bauds", default="115200", help="Comma separated RS485 baudrates")
    sweep.add_argument(
        "--sweep-odrs",
        default="62.5,125,250",
        help="Comma separated ODR values; accepts sensor ODR (125,250,500) or output ODR (62.5,125,250)",
    )
    sweep.add_argument("--sweep-watermarks", default="30")
    sweep.add_argument("--sweep-node-counts", default="1,2,3,4,5,6")
    sweep.add_argument("--sweep-grant-packets", default="1,2,4")
    sweep.add_argument("--sweep-start-from", default="newest")
    sweep.add_argument("--min-duration", type=float, help="Minimum duration per variant in seconds")
    sweep.add_argument("--config-node-ports", help="Optional node=/dev/ttyXYZ mapping used for pre-test config")
    sweep.add_argument("--save-baud", action="store_true", help="Persist changed baudrate on node after each switch")
    sweep.add_argument("--settle-ms", type=int, default=100, help="Delay after baudrate change before verify")
    sweep.add_argument("--abort-on-error", action="store_true", help="Stop a running variant immediately on FAIL")
    sweep.add_argument("--stop-on-fail", action="store_true")
    sweep.add_argument("--keep-going-per-variant", action="store_true")
    return parser.parse_args()


def open_serial_on(port: str, baudrate: int) -> serial.Serial:
    return serial.Serial(
        port=port,
        baudrate=baudrate,
        timeout=0.03,
        write_timeout=0.5,
    )


def open_serial(config: LabConfig) -> serial.Serial:
    return open_serial_on(config.port, config.baud)


def send_simple_command(client: ProtocolClient, node_id: int, payload: bytes, timeout_s: float) -> Optional[Frame]:
    started = time.monotonic()
    sequence = client.send_command(node_id, payload)
    response = client.wait_for_response(node_id, sequence, timeout_s)
    elapsed_ms = (time.monotonic() - started) * 1000.0
    if response is not None:
        print(f"[RTT] {elapsed_ms:.2f} ms")
    return response


def virtual_line_time_s(config: LabConfig, packet_count: int) -> float:
    total_bytes = (packet_count * PACKET_FRAME_BYTES) + (2 * CONTROL_RESPONSE_BYTES)
    bytes_per_second = config.baud / 10.0
    return total_bytes / bytes_per_second


def mark_node_failure(node: DrainNodeState, config: LabConfig, now: float) -> None:
    node.online = False
    node.failures += 1
    if node.failures >= config.dead_node_fail_threshold:
        node.skip_until = now + config.dead_node_retry_s
        node.next_buffer_poll_at = node.skip_until
        node.next_stats_poll_at = node.skip_until
        node.next_service_at = node.skip_until


def initialize_window(
    client: ProtocolClient,
    node: DrainNodeState,
    config: LabConfig,
    buffer_state: BufferState,
) -> None:
    if node.initialized_window:
        return

    if config.start_from == "oldest":
        node.committed_sample_seq = buffer_state.committed_sample_seq
        node.expected_next_sample_seq = buffer_state.oldest_packet_first_seq
        node.initialized_window = True
        return

    if buffer_state.newest_packet_last_seq <= 0 or buffer_state.oldest_packet_first_seq <= 0:
        node.committed_sample_seq = buffer_state.committed_sample_seq
        node.expected_next_sample_seq = max(1, buffer_state.newest_seq)
        node.initialized_window = True
        return

    desired_start_seq = max(
        buffer_state.oldest_packet_first_seq,
        buffer_state.newest_packet_last_seq - (SAMPLES_PER_PACKET - 1),
    )
    desired_commit_seq = desired_start_seq - 1

    if desired_commit_seq > buffer_state.committed_sample_seq:
        sequence = client.send_command(
            node.node_id,
            build_commit_payload(desired_commit_seq),
        )
        response = client.wait_for_response(node.node_id, sequence, config.timeout)
        if response is not None:
            _, status, committed_sample_seq = parse_commit_response(response.payload)
            if status == STATUS_OK:
                node.committed_sample_seq = committed_sample_seq
            else:
                node.committed_sample_seq = buffer_state.committed_sample_seq
        else:
            node.committed_sample_seq = buffer_state.committed_sample_seq
    else:
        node.committed_sample_seq = buffer_state.committed_sample_seq

    node.expected_next_sample_seq = max(1, desired_start_seq)
    node.initialized_window = True


def cmd_ping(config: LabConfig, args: argparse.Namespace) -> int:
    with open_serial(config) as ser:
        client = ProtocolClient(ser)
        response = send_simple_command(
            client,
            config.node,
            build_ping_payload(args.payload.encode("utf-8")),
            config.timeout,
        )
        if response is None:
            print("[TIMEOUT] no ping response")
            return 1
        print(f"[RX] {response.payload.hex(' ')}")
        return 0


def cmd_buffer(config: LabConfig) -> int:
    with open_serial(config) as ser:
        client = ProtocolClient(ser)
        response = send_simple_command(client, config.node, build_buffer_state_payload(), config.timeout)
        if response is None:
            print("[TIMEOUT] no buffer-state response")
            return 1
        state = parse_buffer_state(response.payload)
        print(state)
        return 0


def cmd_stats(config: LabConfig) -> int:
    with open_serial(config) as ser:
        client = ProtocolClient(ser)
        response = send_simple_command(client, config.node, build_stats_payload(), config.timeout)
        if response is None:
            print("[TIMEOUT] no stats response")
            return 1
        stats = parse_stats(response.payload)
        print(stats)
        return 0


def get_real_node_config(client: ProtocolClient, node_id: int, timeout_s: float) -> Optional[ConfigView]:
    sequence = client.send_command(node_id, build_get_config_payload())
    response = client.wait_for_response(node_id, sequence, timeout_s)
    if response is None:
        return None
    return parse_config_view(response.payload)


def set_real_node_odr(client: ProtocolClient, node_id: int, odr_hz: int, timeout_s: float) -> bool:
    sequence = client.send_command(node_id, build_set_odr_payload(odr_hz))
    response = client.wait_for_response(node_id, sequence, timeout_s)
    if response is None or len(response.payload) < 2:
        return False
    return response.payload[1] == STATUS_OK


def set_real_node_watermark(client: ProtocolClient, node_id: int, watermark: int, timeout_s: float) -> bool:
    sequence = client.send_command(node_id, build_set_watermark_payload(watermark))
    response = client.wait_for_response(node_id, sequence, timeout_s)
    if response is None or len(response.payload) < 2:
        return False
    return response.payload[1] == STATUS_OK


def set_real_node_baudrate(client: ProtocolClient, node_id: int, baudrate: int, timeout_s: float) -> bool:
    sequence = client.send_command(node_id, build_set_baudrate_payload(baudrate))
    response = client.wait_for_response(node_id, sequence, timeout_s)
    if response is None or len(response.payload) < 2:
        return False
    return response.payload[1] == STATUS_OK


def save_real_node_config(client: ProtocolClient, node_id: int, timeout_s: float) -> bool:
    sequence = client.send_command(node_id, bytes([CMD_SAVE_CONFIG]))
    response = client.wait_for_response(node_id, sequence, timeout_s)
    if response is None or len(response.payload) < 2:
        return False
    return response.payload[1] == STATUS_OK


def effective_real_nodes(config: LabConfig) -> tuple[int, ...]:
    return config.nodes if config.nodes else (config.node,)


def config_port_for_node(config: LabConfig, node_id: int) -> str:
    if config.config_node_ports and node_id in config.config_node_ports:
        return config.config_node_ports[node_id]
    return config.port


def switch_node_baudrate(
    config: LabConfig,
    node_id: int,
    current_baudrate: int,
    target_baudrate: int,
    settle_ms: int,
    save: bool,
) -> tuple[bool, str]:
    if current_baudrate == target_baudrate:
        return True, "unchanged"

    port = config_port_for_node(config, node_id)
    try:
        with open_serial_on(port, current_baudrate) as ser:
            client = ProtocolClient(ser)
            before = get_real_node_config(client, node_id, config.timeout)
            if before is None:
                return False, "precheck_no_response"
            if before.baudrate != current_baudrate:
                return False, f"precheck_reported_baud_{before.baudrate}"
            if not set_real_node_baudrate(client, node_id, target_baudrate, config.timeout):
                return False, "set_baudrate_failed"
            client.rx_buffer.clear()
            time.sleep(max(settle_ms, 0) / 1000.0)
            ser.baudrate = target_baudrate
            after = get_real_node_config(client, node_id, config.timeout)
            if after is None:
                return False, "postcheck_no_response"
            if after.baudrate != target_baudrate:
                return False, f"postcheck_reported_baud_{after.baudrate}"
            if save and not save_real_node_config(client, node_id, config.timeout):
                return False, "save_failed"
    except serial.SerialException as exc:
        return False, f"serial_error:{exc}"

    return True, "ok"


def collect_node_configs(config: LabConfig, node_ids: tuple[int, ...]) -> dict[int, ConfigView]:
    configs: dict[int, ConfigView] = {}
    opened_clients: dict[tuple[str, int], tuple[serial.Serial, ProtocolClient]] = {}
    try:
        for node_id in node_ids:
            port = config_port_for_node(config, node_id)
            key = (port, config.baud)
            if key not in opened_clients:
                ser = open_serial_on(port, config.baud)
                opened_clients[key] = (ser, ProtocolClient(ser))
            _, client = opened_clients[key]
            cfg = get_real_node_config(client, node_id, config.timeout)
            if cfg is not None:
                configs[node_id] = cfg
    finally:
        for ser, _ in opened_clients.values():
            ser.close()
    return configs


def prepare_real_nodes_for_case(
    config: LabConfig,
    real_nodes: tuple[int, ...],
    target_baudrate: int,
    target_odr_hz: int,
    target_watermark: int,
    current_baudrates: dict[int, int],
    settle_ms: int,
    save_baud: bool,
) -> tuple[bool, str]:
    shared_port_groups: dict[str, list[int]] = {}
    for node_id in real_nodes:
        shared_port_groups.setdefault(config_port_for_node(config, node_id), []).append(node_id)

    for port, node_ids in shared_port_groups.items():
        if len(node_ids) > 1:
            differing = [node_id for node_id in node_ids if current_baudrates.get(node_id, config.baud) != target_baudrate]
            if differing:
                return False, f"shared_port_baud_switch_unsupported:{port}:{','.join(str(node) for node in differing)}"

    for node_id in real_nodes:
        current_baudrate = current_baudrates.get(node_id, config.baud)
        ok, reason = switch_node_baudrate(
            config=config,
            node_id=node_id,
            current_baudrate=current_baudrate,
            target_baudrate=target_baudrate,
            settle_ms=settle_ms,
            save=save_baud,
        )
        if not ok:
            return False, f"baud_setup_failed:n{node_id}:{reason}"
        current_baudrates[node_id] = target_baudrate

    case_serial_config = replace(config, baud=target_baudrate)
    with open_serial(case_serial_config) as ser:
        client = ProtocolClient(ser)
        for node_id in real_nodes:
            if not set_real_node_odr(client, node_id, target_odr_hz, case_serial_config.timeout):
                return False, f"odr_setup_failed:n{node_id}"
            if not set_real_node_watermark(client, node_id, target_watermark, case_serial_config.timeout):
                return False, f"watermark_setup_failed:n{node_id}"

    return True, "ok"


def cmd_baud_check(config: LabConfig, args: argparse.Namespace) -> int:
    node_ids = effective_real_nodes(config)
    configs: list[ConfigView] = []
    failures: list[int] = []

    with open_serial(config) as ser:
        client = ProtocolClient(ser)
        for node_id in node_ids:
            cfg = get_real_node_config(client, node_id, config.timeout)
            if cfg is None:
                print(f"[FAIL] node={node_id} no response at baud={config.baud}")
                failures.append(node_id)
                continue

            configs.append(cfg)
            print(
                f"[NODE] node={cfg.node_id} reported_baud={cfg.baudrate} "
                f"sensor_odr={cfg.odr_hz} output_odr={format_effective_output_odr(cfg.odr_hz)}"
            )

    if failures:
        return 1

    if args.expect_baud is not None:
        mismatched = [cfg.node_id for cfg in configs if cfg.baudrate != args.expect_baud]
        if mismatched:
            print(f"[FAIL] nodes with unexpected baudrate: {','.join(str(node) for node in mismatched)}")
            return 1

    if args.require_all_same and configs:
        first = configs[0].baudrate
        mismatched = [cfg.node_id for cfg in configs if cfg.baudrate != first]
        if mismatched:
            print(f"[FAIL] channel is not uniform at host baud={config.baud}")
            return 1

    print(f"[OK] {len(configs)} node(s) responded at baud={config.baud}")
    return 0


def cmd_baud_set(config: LabConfig, args: argparse.Namespace) -> int:
    node_ids = effective_real_nodes(config)
    if len(node_ids) != 1:
        print("[ERROR] baud-set supports exactly one addressed node at a time", file=sys.stderr)
        return 2

    node_id = node_ids[0]
    with open_serial(config) as ser:
        client = ProtocolClient(ser)
        before = get_real_node_config(client, node_id, config.timeout)
        if before is None:
            print(f"[FAIL] node={node_id} did not answer before baud change", file=sys.stderr)
            return 1

        print(f"[STEP] node={node_id} current_baud={before.baudrate} target_baud={args.value}")
        if not set_real_node_baudrate(client, node_id, args.value, config.timeout):
            print(f"[FAIL] set-baudrate command failed for node={node_id}", file=sys.stderr)
            return 1

        client.rx_buffer.clear()
        time.sleep(max(args.settle_ms, 0) / 1000.0)
        ser.baudrate = args.value

        after = get_real_node_config(client, node_id, config.timeout)
        if after is None:
            print(
                f"[FAIL] node={node_id} did not answer after switching host to baud={args.value}",
                file=sys.stderr,
            )
            return 1

        if after.baudrate != args.value:
            print(
                f"[FAIL] node={node_id} answered but reported baudrate={after.baudrate}",
                file=sys.stderr,
            )
            return 1

        if args.save and not save_real_node_config(client, node_id, config.timeout):
            print(f"[FAIL] save config failed for node={node_id} at baud={args.value}", file=sys.stderr)
            return 1

        print(
            f"[OK] node={node_id} switched to baud={after.baudrate} "
            f"sensor_odr={after.odr_hz} output_odr={format_effective_output_odr(after.odr_hz)}"
        )

        if args.verify_channel:
            node_ids = effective_real_nodes(config)
            mismatched: list[int] = []
            for check_node in node_ids:
                cfg = get_real_node_config(client, check_node, config.timeout)
                if cfg is None or cfg.baudrate != args.value:
                    mismatched.append(check_node)
            if mismatched:
                print(
                    f"[FAIL] channel verification failed at baud={args.value} for nodes: "
                    f"{','.join(str(node) for node in mismatched)}",
                    file=sys.stderr,
                )
                return 1
            print(f"[OK] channel verification passed at baud={args.value}")

    return 0


def refresh_node_state(client: ProtocolClient, node: DrainNodeState, config: LabConfig, now: float) -> None:
    if node.is_virtual:
        if node.virtual_started_monotonic == 0.0:
            node.virtual_started_monotonic = now
            node.online = True
            node.initialized_window = True
            node.packet_capacity = config.virtual_packet_capacity

        produced_samples = int((now - node.virtual_started_monotonic) * node.virtual_odr_hz)
        node.newest_sample_seq = produced_samples
        if config.start_from == "newest" and node.committed_sample_seq == 0:
            node.committed_sample_seq = max(0, node.newest_sample_seq - SAMPLES_PER_PACKET)
            node.expected_next_sample_seq = node.committed_sample_seq + 1

        queued_samples = max(0, node.newest_sample_seq - node.committed_sample_seq)
        max_retained_samples = config.virtual_packet_capacity * SAMPLES_PER_PACKET
        node.buffer_capacity_samples = max_retained_samples
        if queued_samples > max_retained_samples:
            overflow_samples = queued_samples - max_retained_samples
            overflow_packets = math.ceil(overflow_samples / SAMPLES_PER_PACKET)
            node.packet_overwrite_count += overflow_packets
            node.committed_sample_seq += overflow_packets * SAMPLES_PER_PACKET
            queued_samples = max_retained_samples
            node.window_valid = False

        node.queued_packets = math.ceil(queued_samples / SAMPLES_PER_PACKET) if queued_samples > 0 else 0
        node.lag_samples = queued_samples
        node.max_lag_samples = max(node.max_lag_samples, node.lag_samples)
        node.lag_sum_samples += node.lag_samples
        node.lag_samples_count += 1
        node.online = True
        return

    if now < node.skip_until:
        return

    if now >= node.next_buffer_poll_at:
        started = time.monotonic()
        sequence = client.send_command(node.node_id, build_buffer_state_payload())
        response = client.wait_for_response(node.node_id, sequence, config.timeout)
        if response is not None:
            buffer_state = parse_buffer_state(response.payload)
            node.online = True
            node.failures = 0
            node.last_cmd_rtt_ms = (time.monotonic() - started) * 1000.0
            node.newest_sample_seq = buffer_state.newest_packet_last_seq or buffer_state.newest_seq
            node.queued_packets = buffer_state.queued_packets
            node.packet_capacity = buffer_state.packet_capacity
            node.buffer_capacity_samples = buffer_state.capacity_samples
            node.packet_overwrite_count = buffer_state.packet_overwrite_count
            if not node.initialized_window:
                initialize_window(client, node, config, buffer_state)
            else:
                node.committed_sample_seq = max(node.committed_sample_seq, buffer_state.committed_sample_seq)
            node.last_update_monotonic = now
        else:
            mark_node_failure(node, config, now)
        node.next_buffer_poll_at = now + config.buffer_poll_s

    if now >= node.next_stats_poll_at:
        sequence = client.send_command(node.node_id, build_stats_payload())
        response = client.wait_for_response(node.node_id, sequence, config.timeout)
        if response is not None:
            stats = parse_stats(response.payload)
            node.online = True
            node.failures = 0
            node.dropped_samples = stats.dropped_samples
            node.sample_buffer_overwrite_count = stats.sample_buffer_overwrite_count
            node.rx_overflow_count = stats.rx_overflow_count
            node.packet_overwrite_count = max(node.packet_overwrite_count, stats.packet_overwrite_count)
            node.last_update_monotonic = now
        else:
            mark_node_failure(node, config, now)
        node.next_stats_poll_at = now + config.stats_poll_s

    baseline = node.committed_sample_seq if node.committed_sample_seq > 0 else max(node.expected_next_sample_seq - 1, 0)
    node.lag_samples = max(0, node.newest_sample_seq - baseline)
    node.max_lag_samples = max(node.max_lag_samples, node.lag_samples)
    node.lag_sum_samples += node.lag_samples
    node.lag_samples_count += 1


def choose_next_node(states: list[DrainNodeState], now: float) -> Optional[DrainNodeState]:
    eligible = [
        state for state in states
        if state.online and now >= state.skip_until and state.lag_samples > 0 and now >= state.next_service_at
    ]
    if not eligible:
        return None
    return max(eligible, key=lambda item: (item.lag_samples, -item.next_service_at, -item.node_id))


def drain_one_node(client: ProtocolClient, node: DrainNodeState, config: LabConfig) -> None:
    if node.is_virtual:
        queued_packets = math.ceil(node.lag_samples / SAMPLES_PER_PACKET) if node.lag_samples > 0 else 0
        grant_packets = min(config.grant_packets, queued_packets)
        if grant_packets <= 0:
            node.next_service_at = time.monotonic() + 0.05
            return

        line_time_s = virtual_line_time_s(config, grant_packets)
        time.sleep(line_time_s)
        committed_advance = grant_packets * SAMPLES_PER_PACKET
        node.committed_sample_seq = min(node.newest_sample_seq, node.committed_sample_seq + committed_advance)
        node.expected_next_sample_seq = node.committed_sample_seq + 1
        node.bursts_ok += 1
        node.last_cmd_rtt_ms = line_time_s * 1000.0
        node.next_service_at = time.monotonic() + 0.02
        return

    start_seq = node.committed_sample_seq + 1 if node.committed_sample_seq > 0 else node.expected_next_sample_seq
    if start_seq <= 0:
        return

    started = time.monotonic()
    grant_sequence = client.send_command(
        node.node_id,
        build_grant_burst_payload(start_seq, config.grant_packets),
    )
    response = client.wait_for_response(node.node_id, grant_sequence, config.timeout)
    if response is None:
        node.online = False
        node.failures += 1
        node.bursts_failed += 1
        node.next_service_at = time.monotonic() + 0.25
        return

    _, status, granted_start_seq, granted_max_frames = parse_grant_burst_response(response.payload)
    node.last_cmd_rtt_ms = (time.monotonic() - started) * 1000.0
    if status == STATUS_NO_DATA:
        node.next_service_at = time.monotonic() + 0.10
        return
    if status != STATUS_OK:
        node.bursts_failed += 1
        node.next_service_at = time.monotonic() + 0.25
        return

    packets = client.collect_burst_packets(
        node.node_id,
        max_packets=granted_max_frames,
        idle_timeout_s=config.burst_idle_timeout,
        session_timeout_s=config.burst_session_timeout,
    )
    if not packets:
        node.window_valid = False
        node.bursts_failed += 1
        node.next_service_at = time.monotonic() + 0.25
        return

    expected_seq = 0
    last_contiguous_seq = 0
    for packet in packets:
        if expected_seq == 0:
            expected_seq = packet.first_sample_seq
        if packet.first_sample_seq != expected_seq:
            node.window_valid = False
            node.gaps_detected += 1
            node.bursts_failed += 1
            node.next_service_at = time.monotonic() + 0.25
            return
        last_contiguous_seq = packet.first_sample_seq + packet.sample_count - 1
        expected_seq = last_contiguous_seq + 1

    commit_sequence = client.send_command(
        node.node_id,
        build_commit_payload(last_contiguous_seq),
    )
    commit_response = client.wait_for_response(node.node_id, commit_sequence, config.timeout)
    if commit_response is None:
        node.window_valid = False
        node.bursts_failed += 1
        node.next_service_at = time.monotonic() + 0.25
        return

    _, commit_status, committed_sample_seq = parse_commit_response(commit_response.payload)
    if commit_status != STATUS_OK:
        node.window_valid = False
        node.bursts_failed += 1
        node.next_service_at = time.monotonic() + 0.25
        return

    node.committed_sample_seq = committed_sample_seq
    node.expected_next_sample_seq = committed_sample_seq + 1
    node.online = True
    node.bursts_ok += 1
    node.next_service_at = time.monotonic() + 0.02


def render_states(states: list[DrainNodeState], started_at: float) -> None:
    os.write(sys.stdout.fileno(), b"\x1b[2J\x1b[H")
    uptime = time.monotonic() - started_at
    print(f"sensor-system lab host | uptime={uptime:7.2f}s")
    print("node  state   win   lag    queue      committed->newest      burst   gaps  pkt_ovf  sbuf_ovf  loss  rtt")
    for state in states:
        if not state.online and time.monotonic() < state.skip_until:
            state_label = "backoff"
        elif state.is_virtual:
            state_label = "virtual"
        elif state.online:
            state_label = "online "
        else:
            state_label = "offline"

        print(
            f"{state.node_id:>4}  "
            f"{state_label:<7} "
            f"{'ok' if state.window_valid else 'bad':<4} "
            f"{state.lag_samples:>5}  "
            f"{state.queued_packets:>3}/{state.packet_capacity:<3}  "
            f"{state.committed_sample_seq:>10}->{state.newest_sample_seq:<10}  "
            f"{state.bursts_ok:>5}  "
            f"{state.gaps_detected:>4}  "
            f"{state.packet_overwrite_count:>7}  "
            f"{state.sample_buffer_overwrite_count:>8}  "
            f"{state.dropped_samples:>4}  "
            f"{state.last_cmd_rtt_ms:>4.0f}ms"
        )
    sys.stdout.flush()


def average_lag(state: DrainNodeState) -> float:
    if state.lag_samples_count == 0:
        return 0.0
    return state.lag_sum_samples / state.lag_samples_count


def samples_to_seconds(sample_count: int, odr_hz: int) -> float:
    if odr_hz <= 0:
        return 0.0
    return sample_count / effective_output_odr_hz(odr_hz)


def evaluate_node(state: DrainNodeState) -> tuple[str, list[str]]:
    packet_overwrite_delta = state.packet_overwrite_count - state.initial_packet_overwrite_count
    sample_buffer_overwrite_delta = (
        state.sample_buffer_overwrite_count - state.initial_sample_buffer_overwrite_count
    )
    dropped_delta = state.dropped_samples - state.initial_dropped_samples
    rx_overflow_delta = state.rx_overflow_count - state.initial_rx_overflow_count

    reasons: list[str] = []
    if not state.online and not state.is_virtual:
        reasons.append("offline_end")
    if not state.window_valid:
        reasons.append("window_invalid")
    if state.gaps_detected > 0:
        reasons.append("gaps_detected")
    if dropped_delta > 0:
        reasons.append("loss_delta")
    if packet_overwrite_delta > 0:
        reasons.append("packet_overwrite_delta")
    if rx_overflow_delta > 0:
        reasons.append("rx_overflow_delta")
    if state.bursts_ok == 0 and state.online:
        reasons.append("no_successful_burst")

    hard_fail_reasons = {
        "offline_end",
        "gaps_detected",
        "loss_delta",
        "packet_overwrite_delta",
        "rx_overflow_delta",
        "no_successful_burst",
    }

    if not reasons:
        verdict = "PASS"
    elif any(reason in hard_fail_reasons for reason in reasons):
        verdict = "FAIL"
    else:
        verdict = "MARGINAL"

    return verdict, reasons


def overall_verdict(states: list[DrainNodeState]) -> tuple[str, list[str]]:
    verdict_rank = {"PASS": 0, "MARGINAL": 1, "FAIL": 2}
    worst_verdict = "PASS"
    reasons: list[str] = []

    for state in states:
        verdict, node_reasons = evaluate_node(state)
        if verdict_rank[verdict] > verdict_rank[worst_verdict]:
            worst_verdict = verdict
        if node_reasons:
            reasons.extend(f"n{state.node_id}:{reason}" for reason in node_reasons)

    return worst_verdict, reasons


def has_runtime_failure(state: DrainNodeState) -> bool:
    packet_overwrite_delta = state.packet_overwrite_count - state.initial_packet_overwrite_count
    dropped_delta = state.dropped_samples - state.initial_dropped_samples
    rx_overflow_delta = state.rx_overflow_count - state.initial_rx_overflow_count
    return (
        (not state.window_valid)
        or state.gaps_detected > 0
        or dropped_delta > 0
        or packet_overwrite_delta > 0
        or rx_overflow_delta > 0
    )


def print_summary(states: list[DrainNodeState], started_at: float) -> int:
    duration_s = time.monotonic() - started_at
    print(f"\nTest summary | duration={duration_s:.2f}s")

    invalid_windows = 0
    verdict_rank = {"PASS": 0, "MARGINAL": 1, "FAIL": 2}
    worst_verdict = "PASS"

    for state in states:
        packet_overwrite_delta = state.packet_overwrite_count - state.initial_packet_overwrite_count
        sample_buffer_overwrite_delta = (
            state.sample_buffer_overwrite_count - state.initial_sample_buffer_overwrite_count
        )
        dropped_delta = state.dropped_samples - state.initial_dropped_samples
        rx_overflow_delta = state.rx_overflow_count - state.initial_rx_overflow_count
        verdict, reasons = evaluate_node(state)
        if verdict_rank[verdict] > verdict_rank[worst_verdict]:
            worst_verdict = verdict
        if not state.window_valid:
            invalid_windows += 1

        print(
            f"node {state.node_id} ({'virtual' if state.is_virtual else 'real'}): "
            f"{verdict}"
        )
        print(
            "  "
            f"bursts_ok={state.bursts_ok} "
            f"gaps={state.gaps_detected} "
            f"window_valid={'yes' if state.window_valid else 'no'}"
        )
        print(
            "  "
            f"lag_avg={average_lag(state):.1f} "
            f"lag_max={state.max_lag_samples} "
            f"queue={state.queued_packets}/{state.packet_capacity}"
        )
        print(
            "  "
            f"loss_delta={dropped_delta} "
            f"pkt_ovf_delta={packet_overwrite_delta} "
            f"sbuf_ovf_delta={sample_buffer_overwrite_delta} "
            f"rx_ovf_delta={rx_overflow_delta}"
        )
        if reasons:
            print("  " + f"reasons={','.join(reasons)}")

    print(f"\nSummary: invalid_windows={invalid_windows}")
    return 1 if verdict_rank[worst_verdict] >= verdict_rank["FAIL"] else 0


def run_drain_session(client: ProtocolClient, config: LabConfig) -> tuple[list[DrainNodeState], float]:
    states = [DrainNodeState(node_id=node_id) for node_id in config.nodes]
    states.extend(
        DrainNodeState(
            node_id=node_id,
            is_virtual=True,
            virtual_odr_hz=config.virtual_odr_hz,
            online=True,
            initialized_window=(config.start_from == "newest"),
        )
        for node_id in config.virtual_nodes
    )

    started_at = time.monotonic()
    next_render_at = started_at

    for state in states:
        refresh_node_state(client, state, config, started_at)
        state.initial_packet_overwrite_count = state.packet_overwrite_count
        state.initial_sample_buffer_overwrite_count = state.sample_buffer_overwrite_count
        state.initial_dropped_samples = state.dropped_samples
        state.initial_rx_overflow_count = state.rx_overflow_count

    try:
        while True:
            now = time.monotonic()
            if config.duration_s > 0 and (now - started_at) >= config.duration_s:
                break

            for state in states:
                refresh_node_state(client, state, config, now)

            node = choose_next_node(states, now)
            if node is not None:
                drain_one_node(client, node, config)
            else:
                time.sleep(0.01)

            if config.live and now >= next_render_at:
                render_states(states, started_at)
                next_render_at = now + config.refresh_s

            if config.abort_on_error and any(has_runtime_failure(state) for state in states):
                break
    except KeyboardInterrupt:
        print("\n[HOST] interrupted")

    if config.live:
        render_states(states, started_at)

    return states, started_at


def cmd_drain(config: LabConfig) -> int:
    with open_serial(config) as ser:
        client = ProtocolClient(ser)
        states, started_at = run_drain_session(client, config)
        return print_summary(states, started_at)


def make_virtual_node_ids(real_nodes: tuple[int, ...], total_nodes: int) -> tuple[int, ...]:
    virtual_count = max(0, total_nodes - len(real_nodes))
    if virtual_count == 0:
        return ()
    next_node_id = max(real_nodes, default=0) + 1
    return tuple(range(next_node_id, next_node_id + virtual_count))


def build_sweep_result(
    baudrate: int,
    odr_hz: int,
    fifo_watermark: int,
    total_nodes: int,
    grant_packets: int,
    start_from: str,
    states: list[DrainNodeState],
) -> SweepResult:
    verdict, reasons = overall_verdict(states)
    real_states = [state for state in states if not state.is_virtual]
    if real_states:
        packet_capacity = max(state.packet_capacity for state in real_states)
        buffer_capacity_samples = max(state.buffer_capacity_samples for state in real_states)
        real_avg_lag = sum(average_lag(state) for state in real_states) / len(real_states)
        real_max_lag = max(state.max_lag_samples for state in real_states)
        real_bursts_ok = sum(state.bursts_ok for state in real_states)
        real_loss_delta = sum(state.dropped_samples - state.initial_dropped_samples for state in real_states)
        real_packet_overwrite_delta = sum(
            state.packet_overwrite_count - state.initial_packet_overwrite_count for state in real_states
        )
        real_sample_buffer_overwrite_delta = sum(
            state.sample_buffer_overwrite_count - state.initial_sample_buffer_overwrite_count for state in real_states
        )
        real_rx_overflow_delta = sum(
            state.rx_overflow_count - state.initial_rx_overflow_count for state in real_states
        )
    else:
        real_avg_lag = 0.0
        real_max_lag = 0
        real_bursts_ok = 0
        packet_capacity = 0
        buffer_capacity_samples = 0
        real_loss_delta = 0
        real_packet_overwrite_delta = 0
        real_sample_buffer_overwrite_delta = 0
        real_rx_overflow_delta = 0

    return SweepResult(
        baudrate=baudrate,
        odr_hz=odr_hz,
        fifo_watermark=fifo_watermark,
        total_nodes=total_nodes,
        virtual_nodes=max(0, total_nodes - len(real_states)),
        grant_packets=grant_packets,
        start_from=start_from,
        verdict=verdict,
        reason=",".join(reasons[:4]) if reasons else "-",
        packet_capacity=packet_capacity,
        buffer_capacity_samples=buffer_capacity_samples,
        packet_retention_s=samples_to_seconds(packet_capacity * SAMPLES_PER_PACKET, odr_hz),
        buffer_retention_s=samples_to_seconds(buffer_capacity_samples, odr_hz),
        real_bursts_ok=real_bursts_ok,
        real_avg_lag=real_avg_lag,
        real_max_lag=real_max_lag,
        real_loss_delta=real_loss_delta,
        real_packet_overwrite_delta=real_packet_overwrite_delta,
        real_sample_buffer_overwrite_delta=real_sample_buffer_overwrite_delta,
        real_rx_overflow_delta=real_rx_overflow_delta,
    )


def print_sweep_table(results: list[SweepResult]) -> None:
    print("\nSweep results")
    print("baud    odr  wtm  nodes  virt  grant  start   verdict   bursts  lag_avg  lag_max  loss  pkt_ovf  sbuf_ovf  rx_ovf  reason")
    for result in results:
        print(
            f"{result.baudrate:>6}  "
            f"{result.odr_hz:>3}  "
            f"{result.fifo_watermark:>3}  "
            f"{result.total_nodes:>5}  "
            f"{result.virtual_nodes:>4}  "
            f"{result.grant_packets:>5}  "
            f"{result.start_from:<7} "
            f"{result.verdict:<8}  "
            f"{result.real_bursts_ok:>6}  "
            f"{result.real_avg_lag:>7.1f}  "
            f"{result.real_max_lag:>7}  "
            f"{result.real_loss_delta:>4}  "
            f"{result.real_packet_overwrite_delta:>7}  "
            f"{result.real_sample_buffer_overwrite_delta:>8}  "
            f"{result.real_rx_overflow_delta:>6}  "
            f"{result.reason}"
        )


def print_sweep_envelope(results: list[SweepResult]) -> None:
    print("\nPass envelope")
    print("baud    odr  wtm  grant  start   max_pass_nodes  first_fail_nodes")
    grouped: dict[tuple[int, int, int, int, str], list[SweepResult]] = {}
    for result in results:
        key = (result.baudrate, result.odr_hz, result.fifo_watermark, result.grant_packets, result.start_from)
        grouped.setdefault(key, []).append(result)

    for key in sorted(grouped):
        baudrate, odr_hz, fifo_watermark, grant_packets, start_from = key
        ordered = sorted(grouped[key], key=lambda item: item.total_nodes)
        passing = [item.total_nodes for item in ordered if item.verdict == "PASS"]
        failing = [item.total_nodes for item in ordered if item.verdict != "PASS"]
        max_pass_nodes = max(passing) if passing else 0
        first_fail_nodes = min(failing) if failing else 0
        print(
            f"{baudrate:>6}  "
            f"{odr_hz:>3}  "
            f"{fifo_watermark:>3}  "
            f"{grant_packets:>5}  "
            f"{start_from:<7} "
            f"{max_pass_nodes:>14}  "
            f"{first_fail_nodes:>16}"
        )


def print_retention_profile(results: list[SweepResult]) -> None:
    print("\nRetention profile")
    print("baud    odr  pkt_cap  pkt_window_s  acq_cap  acq_window_s")
    seen: set[tuple[int, int, int, int]] = set()
    for result in sorted(results, key=lambda item: (item.baudrate, item.odr_hz, item.packet_capacity, item.buffer_capacity_samples)):
        key = (result.baudrate, result.odr_hz, result.packet_capacity, result.buffer_capacity_samples)
        if key in seen:
            continue
        seen.add(key)
        print(
            f"{result.baudrate:>6}  "
            f"{result.odr_hz:>3}  "
            f"{result.packet_capacity:>7}  "
            f"{result.packet_retention_s:>12.2f}  "
            f"{result.buffer_capacity_samples:>7}  "
            f"{result.buffer_retention_s:>12.2f}"
        )


def build_best_config_rows(results: list[SweepResult]) -> list[SweepResult]:
    grouped: dict[tuple[int, int, int, str], list[SweepResult]] = {}
    for result in results:
        key = (result.baudrate, result.odr_hz, result.fifo_watermark, result.start_from)
        grouped.setdefault(key, []).append(result)

    best_rows: list[SweepResult] = []
    for key in sorted(grouped):
        passing = [item for item in grouped[key] if item.verdict == "PASS"]
        if not passing:
            continue
        best = min(
            passing,
            key=lambda item: (
                -item.total_nodes,
                item.real_avg_lag,
                item.real_max_lag,
                -item.grant_packets,
            ),
        )
        best_rows.append(best)
    return best_rows


def print_best_config_table(results: list[SweepResult]) -> None:
    best_rows = build_best_config_rows(results)
    if not best_rows:
        return

    print("\nBest passing configs")
    print("baud    odr  wtm  start   best_nodes  grant  lag_avg  lag_max  pkt_window_s  acq_window_s")
    for result in best_rows:
        print(
            f"{result.baudrate:>6}  "
            f"{result.odr_hz:>3}  "
            f"{result.fifo_watermark:>3}  "
            f"{result.start_from:<7} "
            f"{result.total_nodes:>10}  "
            f"{result.grant_packets:>5}  "
            f"{result.real_avg_lag:>7.1f}  "
            f"{result.real_max_lag:>7}  "
            f"{result.packet_retention_s:>12.2f}  "
            f"{result.buffer_retention_s:>12.2f}"
        )


def render_sweep_markdown(results: list[SweepResult]) -> str:
    lines: list[str] = []
    lines.append("# Sweep Report")
    lines.append("")

    lines.append("## Retention Profile")
    lines.append("")
    lines.append("| Baud | ODR | Packet Queue Cap | Packet Window [s] | Acquisition Cap [samples] | Acquisition Window [s] |")
    lines.append("| ---: | ---: | ---: | ---: | ---: | ---: |")
    seen: set[tuple[int, int, int, int]] = set()
    for result in sorted(results, key=lambda item: (item.baudrate, item.odr_hz, item.packet_capacity, item.buffer_capacity_samples)):
        key = (result.baudrate, result.odr_hz, result.packet_capacity, result.buffer_capacity_samples)
        if key in seen:
            continue
        seen.add(key)
        lines.append(
            f"| {result.baudrate} | {result.odr_hz} | {result.packet_capacity} | {result.packet_retention_s:.2f} | "
            f"{result.buffer_capacity_samples} | {result.buffer_retention_s:.2f} |"
        )

    lines.append("")
    lines.append("## Best Passing Configs")
    lines.append("")
    lines.append("| Baud | ODR | Watermark | Start | Best Nodes | Grant | Lag Avg | Lag Max | Verdict |")
    lines.append("| ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: | --- |")
    for result in build_best_config_rows(results):
        lines.append(
            f"| {result.baudrate} | {result.odr_hz} | {result.fifo_watermark} | {result.start_from} | "
            f"{result.total_nodes} | {result.grant_packets} | {result.real_avg_lag:.1f} | "
            f"{result.real_max_lag} | {result.verdict} |"
        )

    lines.append("")
    lines.append("## Detailed Results")
    lines.append("")
    lines.append(
        "| Baud | ODR | Watermark | Nodes | Virtual | Grant | Start | Verdict | Bursts | Lag Avg | Lag Max | "
        "Loss | Packet Ovf | Sample Buffer Ovf | RX Ovf | Reason |"
    )
    lines.append(
        "| ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |"
    )
    for result in results:
        lines.append(
            f"| {result.baudrate} | {result.odr_hz} | {result.fifo_watermark} | {result.total_nodes} | {result.virtual_nodes} | "
            f"{result.grant_packets} | {result.start_from} | {result.verdict} | {result.real_bursts_ok} | "
            f"{result.real_avg_lag:.1f} | {result.real_max_lag} | {result.real_loss_delta} | "
            f"{result.real_packet_overwrite_delta} | {result.real_sample_buffer_overwrite_delta} | "
            f"{result.real_rx_overflow_delta} | {result.reason} |"
        )

    return "\n".join(lines) + "\n"


def emit_sweep_report(results: list[SweepResult], config: LabConfig) -> None:
    if config.report_format == "markdown":
        report = render_sweep_markdown(results)
        print()
        print(report, end="")
        if config.report_file:
            Path(config.report_file).write_text(report, encoding="utf-8")
            print(f"[REPORT] wrote markdown report to {config.report_file}")
        return

    if config.report_format == "json":
        payload = {
            "results": [result.__dict__ for result in results],
            "best": [result.__dict__ for result in build_best_config_rows(results)],
        }
        report = json.dumps(payload, indent=2)
        print(report)
        if config.report_file:
            Path(config.report_file).write_text(report + "\n", encoding="utf-8")
            print(f"[REPORT] wrote json report to {config.report_file}")
        return

    print_sweep_table(results)
    print_sweep_envelope(results)
    print_retention_profile(results)
    print_best_config_table(results)
    if config.report_file:
        report = render_sweep_markdown(results)
        Path(config.report_file).write_text(report, encoding="utf-8")
        print(f"[REPORT] wrote markdown report to {config.report_file}")


def cmd_sweep(config: LabConfig, args: argparse.Namespace) -> int:
    real_nodes = config.nodes if config.nodes else (config.node,)
    baudrates = parse_baud_list(args.sweep_bauds)
    odrs = parse_sweep_odr_list(args.sweep_odrs)
    watermarks = parse_fifo_watermark_list(args.sweep_watermarks)
    total_node_counts = tuple(sorted(set(parse_int_list(args.sweep_node_counts))))
    grant_packets_values = tuple(sorted(set(parse_int_list(args.sweep_grant_packets))))
    start_from_values = parse_str_list(args.sweep_start_from)
    duration_s = args.min_duration if args.min_duration is not None else (config.duration_s if config.duration_s > 0 else 8.0)
    results: list[SweepResult] = []

    original_configs = collect_node_configs(config, real_nodes)
    current_baudrates = {node_id: original_configs[node_id].baudrate if node_id in original_configs else config.baud for node_id in real_nodes}

    try:
        for baudrate in baudrates:
            for odr_hz in odrs:
                for fifo_watermark in watermarks:
                    for start_from in start_from_values:
                        for grant_packets in grant_packets_values:
                            for total_nodes in total_node_counts:
                                if total_nodes < len(real_nodes):
                                    continue

                                case_config = replace(
                                    config,
                                    baud=baudrate,
                                    nodes=real_nodes,
                                    grant_packets=grant_packets,
                                    start_from=start_from,
                                    virtual_nodes=make_virtual_node_ids(real_nodes, total_nodes),
                                    virtual_odr_hz=effective_output_odr_hz(odr_hz),
                                    duration_s=duration_s,
                                    live=False,
                                    abort_on_error=args.abort_on_error or config.abort_on_error,
                                )

                                print(
                                    f"[SWEEP] baud={baudrate} sensor_odr={odr_hz}Hz "
                                    f"output_odr={format_effective_output_odr(odr_hz)}Hz "
                                    f"wtm={fifo_watermark} total_nodes={total_nodes} "
                                    f"grant={grant_packets} start={start_from} "
                                    f"min_duration={duration_s:.1f}s abort_on_error={'yes' if case_config.abort_on_error else 'no'}"
                                )

                                setup_ok, setup_reason = prepare_real_nodes_for_case(
                                    config=config,
                                    real_nodes=real_nodes,
                                    target_baudrate=baudrate,
                                    target_odr_hz=odr_hz,
                                    target_watermark=fifo_watermark,
                                    current_baudrates=current_baudrates,
                                    settle_ms=args.settle_ms,
                                    save_baud=args.save_baud,
                                )

                                if not setup_ok:
                                    result = SweepResult(
                                        baudrate=baudrate,
                                        odr_hz=odr_hz,
                                        fifo_watermark=fifo_watermark,
                                        total_nodes=total_nodes,
                                        virtual_nodes=max(0, total_nodes - len(real_nodes)),
                                        grant_packets=grant_packets,
                                        start_from=start_from,
                                        verdict="FAIL",
                                        reason=setup_reason,
                                        packet_capacity=0,
                                        buffer_capacity_samples=0,
                                        packet_retention_s=0.0,
                                        buffer_retention_s=0.0,
                                        real_bursts_ok=0,
                                        real_avg_lag=0.0,
                                        real_max_lag=0,
                                        real_loss_delta=0,
                                        real_packet_overwrite_delta=0,
                                        real_sample_buffer_overwrite_delta=0,
                                        real_rx_overflow_delta=0,
                                    )
                                else:
                                    with open_serial(case_config) as ser:
                                        ser.reset_input_buffer()
                                        client = ProtocolClient(ser)
                                        time.sleep(0.2)
                                        states, _ = run_drain_session(client, case_config)
                                        result = build_sweep_result(
                                            baudrate=baudrate,
                                            odr_hz=odr_hz,
                                            fifo_watermark=fifo_watermark,
                                            total_nodes=total_nodes,
                                            grant_packets=grant_packets,
                                            start_from=start_from,
                                            states=states,
                                        )

                                results.append(result)
                                print(
                                    f"[RESULT] verdict={result.verdict} "
                                    f"lag_max={result.real_max_lag} "
                                    f"loss={result.real_loss_delta} "
                                    f"pkt_ovf={result.real_packet_overwrite_delta} "
                                    f"sbuf_ovf={result.real_sample_buffer_overwrite_delta} "
                                    f"rx_ovf={result.real_rx_overflow_delta} "
                                    f"reason={result.reason}"
                                )

                                if result.verdict != "PASS" and not args.keep_going_per_variant:
                                    break
                                if result.verdict != "PASS" and args.stop_on_fail:
                                    emit_sweep_report(results, config)
                                    return 1

    finally:
        if original_configs:
            for node_id, cfg in original_configs.items():
                current_baudrate = current_baudrates.get(node_id, cfg.baudrate)
                if current_baudrate != cfg.baudrate:
                    switch_node_baudrate(
                        config=config,
                        node_id=node_id,
                        current_baudrate=current_baudrate,
                        target_baudrate=cfg.baudrate,
                        settle_ms=args.settle_ms,
                        save=args.save_baud,
                    )
                    current_baudrates[node_id] = cfg.baudrate

            restored_ports: set[tuple[str, int]] = set()
            for node_id, cfg in original_configs.items():
                port = config_port_for_node(config, node_id)
                key = (port, cfg.baudrate)
                if key in restored_ports:
                    continue
                restored_ports.add(key)
                with open_serial_on(port, cfg.baudrate) as ser:
                    client = ProtocolClient(ser)
                    for restore_node_id, restore_cfg in original_configs.items():
                        if config_port_for_node(config, restore_node_id) != port:
                            continue
                        if restore_cfg.baudrate != cfg.baudrate:
                            continue
                        set_real_node_odr(client, restore_node_id, restore_cfg.odr_hz, config.timeout)
                        set_real_node_watermark(
                            client,
                            restore_node_id,
                            normalize_fifo_watermark(restore_cfg.fifo_watermark),
                            config.timeout,
                        )

    emit_sweep_report(results, config)
    return 1 if any(result.verdict != "PASS" for result in results) else 0


def main() -> int:
    args = parse_args()
    config = resolve_config(args)

    if args.command == "ping":
        return cmd_ping(config, args)
    if args.command == "buffer":
        return cmd_buffer(config)
    if args.command == "stats":
        return cmd_stats(config)
    if args.command == "drain":
        return cmd_drain(config)
    if args.command == "baud-check":
        return cmd_baud_check(config, args)
    if args.command == "baud-set":
        return cmd_baud_set(config, args)
    if args.command == "sweep":
        return cmd_sweep(config, args)
    raise ValueError(f"unsupported command: {args.command}")


if __name__ == "__main__":
    raise SystemExit(main())
