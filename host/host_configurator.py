#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import struct
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

import serial


FRAME_MAGIC = 0xAA55
FRAME_PROTOCOL_VERSION = 2
FRAME_HEADER_FORMAT = "<HBBBBBHI"
FRAME_HEADER_SIZE = struct.calcsize(FRAME_HEADER_FORMAT)
FRAME_CRC_SIZE = 2
FRAME_MAX_PAYLOAD_SIZE = 1024

FRAME_TYPE_COMMAND = 0x02
FRAME_TYPE_RESPONSE = 0x03

UNASSIGNED_NODE_ID = 0x00
BROADCAST_NODE_ID = 0xFF
HOST_NODE_ID = 0xFE

CMD_GET_CONFIG = 0x20
CMD_SET_NODE_ID = 0x21
CMD_SET_ODR = 0x22
CMD_SET_RANGE = 0x23
CMD_SET_OFFSETS = 0x24
CMD_SET_FIFO_WATERMARK = 0x25
CMD_SAVE_CONFIG = 0x26
CMD_LOAD_CONFIG = 0x27
CMD_RESET_CONFIG_TO_DEFAULTS = 0x28
CMD_SET_BAUD_RATE = 0x29
CMD_COMMISSION_DISCOVER = 0x2A
CMD_COMMISSION_ASSIGN_NODE_ID = 0x2B
CMD_SET_HIGH_PASS = 0x2C
CMD_RESTART = 0x02
CMD_GET_STATUS = 0x40
CMD_GET_DIAGNOSTIC_INFO = 0x44
CMD_GET_FAULT_SNAPSHOT = 0x45
CMD_READ_DIAGNOSTIC_EVENTS = 0x46
CMD_CLEAR_DIAGNOSTIC_EVENTS = 0x47
CMD_GET_PERSISTENT_DIAGNOSTIC_RECORD = 0x48
CMD_CLEAR_PERSISTENT_DIAGNOSTIC_RECORD = 0x49

STATUS_NAMES = {
    0: "Ok",
    1: "BadFrame",
    2: "Unsupported",
    3: "InvalidParam",
    4: "InvalidState",
    5: "Busy",
    6: "NoData",
    7: "SensorError",
    8: "ConfigError",
    9: "StorageError",
    10: "SaveFailed",
    11: "LoadFailed",
    12: "InternalError",
}

GET_CONFIG_FORMAT = "<BBBHb"  # placeholder, replaced below
GET_STATUS_FORMAT = "<BBBBHIBBI"  # placeholder, replaced below
GET_STATUS_FORMAT_V2 = "<BBBBHBIIIIQIHBB"
GET_STATUS_FORMAT_V3 = "<BBBBHBIIIIQIHBBIIII"
GET_STATUS_FORMAT_V4 = "<BBBBHBIIIIQIHBBIIIIII"
GET_STATUS_FORMAT_V5 = GET_STATUS_FORMAT_V4 + "III"
GET_STATUS_FORMAT_V6 = GET_STATUS_FORMAT_V5 + "IIII"
GET_STATUS_FORMAT_V7 = GET_STATUS_FORMAT_V6 + "IIII"
GET_STATUS_FORMAT_V8 = GET_STATUS_FORMAT_V7 + "IIII"
GET_STATUS_FORMAT_V9 = GET_STATUS_FORMAT_V8 + "B"
GET_DIAGNOSTIC_INFO_FORMAT = "<BBIBBHHIIIIH"
# Current firmware places debug fields before arg0/arg1. Keep the legacy
# layouts below so dumps from older nodes remain readable.
GET_FAULT_SNAPSHOT_FORMAT = "<BBIIHBBQIIIIIIii"
GET_FAULT_SNAPSHOT_FORMAT_V2 = "<BBIIHBBQIIIIIIIIIii"
GET_FAULT_SNAPSHOT_FORMAT_V3 = "<BBIIHBBQIIIIIIIIIIii"
GET_PERSISTENT_DIAGNOSTIC_RECORD_FORMAT = "<BBIIIIIHBBBBHQIIIIIIii"
GET_PERSISTENT_DIAGNOSTIC_RECORD_FORMAT_V2 = "<BBIIIIIHBBBBHQIIIIIIIIIii"
GET_PERSISTENT_DIAGNOSTIC_RECORD_FORMAT_V3 = "<BBIIIIIHBBBBHQIIIIIIIIIIii"
READ_DIAGNOSTIC_EVENTS_REQUEST_FORMAT = "<BIB"
READ_DIAGNOSTIC_EVENTS_HEADER_FORMAT = "<BBBBII"
READ_DIAGNOSTIC_EVENT_FORMAT = "<IIHBBQii"

# Actual packed layouts from firmware.
GET_CONFIG_FORMAT_V1 = "<BBBIHBiiiBHBB"
GET_CONFIG_FORMAT_V2 = "<BBBIHBiiiBHBBBBIQ"
GET_CONFIG_FORMAT = GET_CONFIG_FORMAT_V2 + "B"
GET_STATUS_FORMAT = "<BBBBHBI I I".replace(" ", "")

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
DEVICE_HARDWARE_ID_SIZE = 8
DEFAULT_SYSTEM_CONFIG_PATH = "host/system_config.json"


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


def effective_output_odr_hz(sensor_odr_hz: int) -> float:
    return sensor_odr_hz / float(OUTPUT_DECIMATION_FACTOR)


def status_name(status: int) -> str:
    return STATUS_NAMES.get(status, f"Unknown({status})")


def sensor_status_name(status: int) -> str:
    return {
        0: "Ok",
        1: "NotInitialized",
        2: "Busy",
        3: "CommError",
        4: "Timeout",
        5: "InvalidParam",
        6: "NotSupported",
        7: "InvalidDevice",
        8: "NoData",
        9: "InvalidSample",
        10: "InternalError",
    }.get(status, f"UnknownSensorStatus({status})")


@dataclass
class HostConfig:
    port: str = "/dev/sensor-system-rs485"
    baud: int = 115200
    node: int = 1
    timeout: float = 2.0

    @classmethod
    def from_dict(cls, data: dict) -> "HostConfig":
        return cls(
            port=str(data.get("port", cls.port)),
            baud=int(data.get("baud", cls.baud)),
            node=int(data.get("node", cls.node)),
            timeout=float(data.get("timeout", cls.timeout)),
        )


@dataclass
class Frame:
    frame_type: int
    flags: int
    destination: int
    source: int
    sequence: int
    payload: bytes


@dataclass
class ConfigView:
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
    filter_profile: int = 255
    decimation_factor: int = OUTPUT_DECIMATION_FACTOR
    config_revision: int = 0
    config_effective_sample_seq: int = 0
    board_revision: int | None = None


@dataclass
class StatusView:
    node_id: int
    node_state: int
    odr_hz: int
    range_g: int
    protocol_version: int
    firmware_version: int
    dropped_samples: int
    uptime_ms: int = 0
    last_sample_seq: int = 0
    last_progress_ms_ago: int = 0
    last_error_code: int = 0
    reset_cause: int = 0
    diagnostic_flags: int = 0
    fifo_poll_fallback_reads: int = 0
    soft_recover_count: int = 0
    no_data_with_irq: int = 0
    no_data_without_irq: int = 0
    irq_int1_events: int = 0
    irq_drdy_events: int = 0
    gpio_int1_edges: int = 0
    gpio_drdy_edges: int = 0
    debug_config_snapshot: int = 0
    irq_status_not_full: int = 0
    irq_fifo_entries_lt_3: int = 0
    irq_fifo_entries_lt_watermark: int = 0
    debug_irq_snapshot: int = 0
    spurious_int1_events: int = 0
    fifo_overrun_events: int = 0
    fifo_discarded_samples: int = 0
    fifo_uncertain_loss_events: int = 0
    drdy_timestamp_ring_overflow: int = 0
    timing_binding_mismatch: int = 0
    timing_binding_invalidations: int = 0
    timing_segment_id: int = 0
    board_revision: int | None = None


@dataclass
class DiagnosticInfoView:
    uptime_ms: int
    reset_cause: int
    live_usb_enabled: int
    stored_event_count: int
    event_capacity: int
    dropped_event_count: int
    first_event_id: int
    next_event_id: int
    last_error_event_id: int
    last_error_code: int


@dataclass
class FaultSnapshotView:
    event_id: int
    time_ms: int
    event_code: int
    severity: int
    reset_cause: int
    sample_seq: int
    last_progress_ms: int
    fifo_no_data: int
    sensor_errors: int
    dropped_samples: int
    rx_overflow_count: int
    packet_overwrite_count: int
    debug_gpio_int1_edges: int = 0
    debug_gpio_drdy_edges: int = 0
    debug_config_snapshot: int = 0
    debug_irq_snapshot: int = 0
    arg0: int = 0
    arg1: int = 0


@dataclass
class PersistentDiagnosticRecordView:
    generation: int
    boot_counter: int
    firmware_version: int
    event_id: int
    time_ms: int
    event_code: int
    severity: int
    repeat_count: int
    reset_cause: int
    sample_seq: int
    last_progress_ms: int
    fifo_no_data: int
    sensor_errors: int
    dropped_samples: int
    rx_overflow_count: int
    packet_overwrite_count: int
    debug_gpio_int1_edges: int = 0
    debug_gpio_drdy_edges: int = 0
    debug_config_snapshot: int = 0
    debug_irq_snapshot: int = 0
    arg0: int = 0
    arg1: int = 0


@dataclass
class DiagnosticEventView:
    event_id: int
    time_ms: int
    event_code: int
    severity: int
    repeat_count: int
    sample_seq: int
    arg0: int
    arg1: int


@dataclass
class CommissionIdentity:
    node_id: int
    hardware_id: bytes
    board_revision: int | None = None


@dataclass(frozen=True)
class SystemConfigSyncResult:
    path: Path
    channel_name: str
    node_id: int


def _normalize_channel_node_entry(raw: object) -> dict | None:
    if isinstance(raw, int):
        return {"id": raw}
    if isinstance(raw, dict):
        return dict(raw)
    return None


def sync_system_config_from_device_config(
    system_config_path: Path,
    *,
    port: str,
    previous_node_id: int,
    updated: ConfigView,
) -> SystemConfigSyncResult:
    if not system_config_path.exists():
        raise RuntimeError(f"system config '{system_config_path}' does not exist")

    data = json.loads(system_config_path.read_text(encoding="utf-8"))
    channels = data.get("channels")
    if not isinstance(channels, list) or not channels:
        raise RuntimeError(f"system config '{system_config_path}' does not define any channels")

    channel_matches = [
        channel
        for channel in channels
        if isinstance(channel, dict) and str(channel.get("port")) == port
    ]
    if not channel_matches:
        if len(channels) == 1 and isinstance(channels[0], dict):
            channel = channels[0]
        else:
            raise RuntimeError(f"no channel in '{system_config_path}' matches port '{port}'")
    elif len(channel_matches) == 1:
        channel = channel_matches[0]
    else:
        raise RuntimeError(f"multiple channels in '{system_config_path}' match port '{port}'")

    raw_nodes = channel.get("nodes")
    if not isinstance(raw_nodes, list) or not raw_nodes:
        raise RuntimeError(f"channel '{channel.get('name', '?')}' does not define any nodes")

    matched_index: int | None = None
    matched_entry: dict | None = None
    for index, raw_node in enumerate(raw_nodes):
        entry = _normalize_channel_node_entry(raw_node)
        if entry is None:
            continue
        entry_node_id = int(entry.get("id", entry.get("node_id", -1)))
        if entry_node_id == previous_node_id:
            matched_index = index
            matched_entry = entry
            break

    if matched_index is None:
        for index, raw_node in enumerate(raw_nodes):
            entry = _normalize_channel_node_entry(raw_node)
            if entry is None:
                continue
            entry_node_id = int(entry.get("id", entry.get("node_id", -1)))
            if entry_node_id == updated.node_id:
                matched_index = index
                matched_entry = entry
                break

    if matched_index is None or matched_entry is None:
        if len(raw_nodes) == 1:
            matched_index = 0
            matched_entry = _normalize_channel_node_entry(raw_nodes[0]) or {}
        else:
            raise RuntimeError(
                f"no node in channel '{channel.get('name', '?')}' matches ids {previous_node_id} or {updated.node_id}"
            )

    matched_entry["id"] = updated.node_id
    if "node_id" in matched_entry:
        matched_entry["node_id"] = updated.node_id
    matched_entry["expected_odr_hz"] = effective_output_odr_hz(updated.odr_hz)
    matched_entry["sensor_odr_hz"] = updated.odr_hz
    matched_entry["range_g"] = updated.range_g
    matched_entry["high_pass_corner"] = updated.high_pass_corner
    matched_entry["filter_profile"] = updated.filter_profile
    matched_entry["decimation_factor"] = updated.decimation_factor
    matched_entry["fifo_watermark"] = updated.fifo_watermark
    matched_entry["offset_x"] = updated.offset_x
    matched_entry["offset_y"] = updated.offset_y
    matched_entry["offset_z"] = updated.offset_z
    if updated.board_revision is not None:
        matched_entry["board_revision"] = updated.board_revision
    raw_nodes[matched_index] = matched_entry
    channel["baud"] = updated.baudrate

    system_config_path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    return SystemConfigSyncResult(
        path=system_config_path,
        channel_name=str(channel.get("name", "channel")),
        node_id=updated.node_id,
    )


class FrameCodec:
    @staticmethod
    def encode(frame_type: int, destination: int, source: int, sequence: int, payload: bytes) -> bytes:
        if len(payload) > FRAME_MAX_PAYLOAD_SIZE:
            raise ValueError(f"payload too large: {len(payload)} bytes")

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
    def __init__(self, ser: serial.Serial) -> None:
        self.ser = ser
        self.rx_buffer = bytearray()
        self.sequence = 1

    def next_sequence(self) -> int:
        current = self.sequence
        self.sequence = 1 if self.sequence == 0xFFFFFFFF else self.sequence + 1
        return current

    def send_command(self, node_id: int, payload: bytes) -> int:
        sequence = self.next_sequence()
        frame = FrameCodec.encode(
            frame_type=FRAME_TYPE_COMMAND,
            destination=node_id,
            source=HOST_NODE_ID,
            sequence=sequence,
            payload=payload,
        )
        self.ser.write(frame)
        self.ser.flush()
        return sequence

    def wait_for_response(self, node_id: int, sequence: int, timeout_s: float) -> Optional[Frame]:
        deadline = time.monotonic() + timeout_s

        while time.monotonic() < deadline:
            frame = self.poll_for_matching_frame(
                lambda candidate: (
                    candidate.frame_type == FRAME_TYPE_RESPONSE
                    and candidate.source == node_id
                    and candidate.destination == HOST_NODE_ID
                    and candidate.sequence == sequence
                ),
                deadline,
            )
            if frame is not None:
                return frame

        return None

    def poll_for_matching_frame(self, predicate, deadline: float) -> Optional[Frame]:
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

                if predicate(frame):
                    return frame

        return None


def parse_config_view(payload: bytes) -> ConfigView:
    if len(payload) >= struct.calcsize(GET_CONFIG_FORMAT):
        config_format = GET_CONFIG_FORMAT
    elif len(payload) >= struct.calcsize(GET_CONFIG_FORMAT_V2):
        config_format = GET_CONFIG_FORMAT_V2
    else:
        config_format = GET_CONFIG_FORMAT_V1
    values = struct.unpack(
        config_format,
        payload[: struct.calcsize(config_format)],
    )
    return ConfigView(
        node_id=values[2],
        baudrate=values[3],
        odr_hz=values[4],
        range_g=values[5],
        offset_x=values[6],
        offset_y=values[7],
        offset_z=values[8],
        fifo_watermark=values[9],
        act_threshold=values[10],
        act_count=values[11],
        high_pass_corner=values[12],
        filter_profile=values[13] if len(values) > 13 else 255,
        decimation_factor=(
            values[14] if len(values) > 14 else OUTPUT_DECIMATION_FACTOR
        ),
        config_revision=values[15] if len(values) > 15 else 0,
        config_effective_sample_seq=values[16] if len(values) > 16 else 0,
        board_revision=values[17] if len(values) > 17 else None,
    )


def parse_status_view(payload: bytes) -> StatusView:
    if len(payload) >= struct.calcsize(GET_STATUS_FORMAT_V9):
        status = parse_status_view(payload[: struct.calcsize(GET_STATUS_FORMAT_V8)])
        status.board_revision = payload[struct.calcsize(GET_STATUS_FORMAT_V8)]
        return status

    if len(payload) >= struct.calcsize(GET_STATUS_FORMAT_V8):
        values = struct.unpack(
            GET_STATUS_FORMAT_V8,
            payload[: struct.calcsize(GET_STATUS_FORMAT_V8)],
        )
        return StatusView(
            node_id=values[2],
            node_state=values[3],
            odr_hz=values[4],
            range_g=values[5],
            protocol_version=values[6],
            firmware_version=values[7],
            dropped_samples=values[8],
            uptime_ms=values[9],
            last_sample_seq=values[10],
            last_progress_ms_ago=values[11],
            last_error_code=values[12],
            reset_cause=values[13],
            diagnostic_flags=values[14],
            fifo_poll_fallback_reads=values[15],
            soft_recover_count=values[16],
            no_data_with_irq=values[17],
            no_data_without_irq=values[18],
            irq_int1_events=values[19],
            irq_drdy_events=values[20],
            gpio_int1_edges=values[21],
            gpio_drdy_edges=values[22],
            debug_config_snapshot=values[23],
            irq_status_not_full=values[24],
            irq_fifo_entries_lt_3=values[25],
            irq_fifo_entries_lt_watermark=values[26],
            debug_irq_snapshot=values[27],
            spurious_int1_events=values[28],
            fifo_overrun_events=values[29],
            fifo_discarded_samples=values[30],
            fifo_uncertain_loss_events=values[31],
            drdy_timestamp_ring_overflow=values[32],
            timing_binding_mismatch=values[33],
            timing_binding_invalidations=values[34],
            timing_segment_id=values[35],
        )

    if len(payload) >= struct.calcsize(GET_STATUS_FORMAT_V7):
        values = struct.unpack(GET_STATUS_FORMAT_V7, payload[: struct.calcsize(GET_STATUS_FORMAT_V7)])
        return StatusView(
            node_id=values[2],
            node_state=values[3],
            odr_hz=values[4],
            range_g=values[5],
            protocol_version=values[6],
            firmware_version=values[7],
            dropped_samples=values[8],
            uptime_ms=values[9],
            last_sample_seq=values[10],
            last_progress_ms_ago=values[11],
            last_error_code=values[12],
            reset_cause=values[13],
            diagnostic_flags=values[14],
            fifo_poll_fallback_reads=values[15],
            soft_recover_count=values[16],
            no_data_with_irq=values[17],
            no_data_without_irq=values[18],
            irq_int1_events=values[19],
            irq_drdy_events=values[20],
            gpio_int1_edges=values[21],
            gpio_drdy_edges=values[22],
            debug_config_snapshot=values[23],
            irq_status_not_full=values[24],
            irq_fifo_entries_lt_3=values[25],
            irq_fifo_entries_lt_watermark=values[26],
            debug_irq_snapshot=values[27],
            spurious_int1_events=values[28],
            fifo_overrun_events=values[29],
            fifo_discarded_samples=values[30],
            fifo_uncertain_loss_events=values[31],
        )

    if len(payload) >= struct.calcsize(GET_STATUS_FORMAT_V6):
        values = struct.unpack(GET_STATUS_FORMAT_V6, payload[: struct.calcsize(GET_STATUS_FORMAT_V6)])
        return StatusView(
            node_id=values[2],
            node_state=values[3],
            odr_hz=values[4],
            range_g=values[5],
            protocol_version=values[6],
            firmware_version=values[7],
            dropped_samples=values[8],
            uptime_ms=values[9],
            last_sample_seq=values[10],
            last_progress_ms_ago=values[11],
            last_error_code=values[12],
            reset_cause=values[13],
            diagnostic_flags=values[14],
            fifo_poll_fallback_reads=values[15],
            soft_recover_count=values[16],
            no_data_with_irq=values[17],
            no_data_without_irq=values[18],
            irq_int1_events=values[19],
            irq_drdy_events=values[20],
            gpio_int1_edges=values[21],
            gpio_drdy_edges=values[22],
            debug_config_snapshot=values[23],
            irq_status_not_full=values[24],
            irq_fifo_entries_lt_3=values[25],
            irq_fifo_entries_lt_watermark=values[26],
            debug_irq_snapshot=values[27],
        )

    if len(payload) >= struct.calcsize(GET_STATUS_FORMAT_V5):
        values = struct.unpack(GET_STATUS_FORMAT_V5, payload[: struct.calcsize(GET_STATUS_FORMAT_V5)])
        return StatusView(
            node_id=values[2],
            node_state=values[3],
            odr_hz=values[4],
            range_g=values[5],
            protocol_version=values[6],
            firmware_version=values[7],
            dropped_samples=values[8],
            uptime_ms=values[9],
            last_sample_seq=values[10],
            last_progress_ms_ago=values[11],
            last_error_code=values[12],
            reset_cause=values[13],
            diagnostic_flags=values[14],
            fifo_poll_fallback_reads=values[15],
            soft_recover_count=values[16],
            no_data_with_irq=values[17],
            no_data_without_irq=values[18],
            irq_int1_events=values[19],
            irq_drdy_events=values[20],
            gpio_int1_edges=values[21],
            gpio_drdy_edges=values[22],
            debug_config_snapshot=values[23],
        )

    if len(payload) >= struct.calcsize(GET_STATUS_FORMAT_V4):
        values = struct.unpack(GET_STATUS_FORMAT_V4, payload[: struct.calcsize(GET_STATUS_FORMAT_V4)])
        return StatusView(
            node_id=values[2],
            node_state=values[3],
            odr_hz=values[4],
            range_g=values[5],
            protocol_version=values[6],
            firmware_version=values[7],
            dropped_samples=values[8],
            uptime_ms=values[9],
            last_sample_seq=values[10],
            last_progress_ms_ago=values[11],
            last_error_code=values[12],
            reset_cause=values[13],
            diagnostic_flags=values[14],
            fifo_poll_fallback_reads=values[15],
            soft_recover_count=values[16],
            no_data_with_irq=values[17],
            no_data_without_irq=values[18],
            irq_int1_events=values[19],
            irq_drdy_events=values[20],
        )

    if len(payload) >= struct.calcsize(GET_STATUS_FORMAT_V3):
        values = struct.unpack(GET_STATUS_FORMAT_V3, payload[: struct.calcsize(GET_STATUS_FORMAT_V3)])
        return StatusView(
            node_id=values[2],
            node_state=values[3],
            odr_hz=values[4],
            range_g=values[5],
            protocol_version=values[6],
            firmware_version=values[7],
            dropped_samples=values[8],
            uptime_ms=values[9],
            last_sample_seq=values[10],
            last_progress_ms_ago=values[11],
            last_error_code=values[12],
            reset_cause=values[13],
            diagnostic_flags=values[14],
            fifo_poll_fallback_reads=values[15],
            soft_recover_count=values[16],
            no_data_with_irq=values[17],
            no_data_without_irq=values[18],
        )

    if len(payload) >= struct.calcsize(GET_STATUS_FORMAT_V2):
        values = struct.unpack(GET_STATUS_FORMAT_V2, payload[: struct.calcsize(GET_STATUS_FORMAT_V2)])
        return StatusView(
            node_id=values[2],
            node_state=values[3],
            odr_hz=values[4],
            range_g=values[5],
            protocol_version=values[6],
            firmware_version=values[7],
            dropped_samples=values[8],
            uptime_ms=values[9],
            last_sample_seq=values[10],
            last_progress_ms_ago=values[11],
            last_error_code=values[12],
            reset_cause=values[13],
            diagnostic_flags=values[14],
        )

    values = struct.unpack(GET_STATUS_FORMAT, payload[: struct.calcsize(GET_STATUS_FORMAT)])
    return StatusView(
        node_id=values[2],
        node_state=values[3],
        odr_hz=values[4],
        range_g=values[5],
        protocol_version=values[6],
        firmware_version=values[7],
        dropped_samples=values[8],
    )


def parse_diagnostic_info_view(payload: bytes) -> DiagnosticInfoView:
    values = struct.unpack(
        GET_DIAGNOSTIC_INFO_FORMAT,
        payload[: struct.calcsize(GET_DIAGNOSTIC_INFO_FORMAT)],
    )
    return DiagnosticInfoView(
        uptime_ms=values[2],
        reset_cause=values[3],
        live_usb_enabled=values[4],
        stored_event_count=values[5],
        event_capacity=values[6],
        dropped_event_count=values[7],
        first_event_id=values[8],
        next_event_id=values[9],
        last_error_event_id=values[10],
        last_error_code=values[11],
    )


def parse_fault_snapshot_view(payload: bytes) -> FaultSnapshotView:
    if len(payload) >= struct.calcsize(GET_FAULT_SNAPSHOT_FORMAT_V3):
        values = struct.unpack(
            GET_FAULT_SNAPSHOT_FORMAT_V3,
            payload[: struct.calcsize(GET_FAULT_SNAPSHOT_FORMAT_V3)],
        )
        return FaultSnapshotView(
            event_id=values[2],
            time_ms=values[3],
            event_code=values[4],
            severity=values[5],
            reset_cause=values[6],
            sample_seq=values[7],
            last_progress_ms=values[8],
            fifo_no_data=values[9],
            sensor_errors=values[10],
            dropped_samples=values[11],
            rx_overflow_count=values[12],
            packet_overwrite_count=values[13],
            debug_gpio_int1_edges=values[14],
            debug_gpio_drdy_edges=values[15],
            debug_config_snapshot=values[16],
            debug_irq_snapshot=values[17],
            arg0=values[18],
            arg1=values[19],
        )

    if len(payload) >= struct.calcsize(GET_FAULT_SNAPSHOT_FORMAT_V2):
        values = struct.unpack(
            GET_FAULT_SNAPSHOT_FORMAT_V2,
            payload[: struct.calcsize(GET_FAULT_SNAPSHOT_FORMAT_V2)],
        )
        return FaultSnapshotView(
            event_id=values[2],
            time_ms=values[3],
            event_code=values[4],
            severity=values[5],
            reset_cause=values[6],
            sample_seq=values[7],
            last_progress_ms=values[8],
            fifo_no_data=values[9],
            sensor_errors=values[10],
            dropped_samples=values[11],
            rx_overflow_count=values[12],
            packet_overwrite_count=values[13],
            debug_gpio_int1_edges=values[14],
            debug_gpio_drdy_edges=values[15],
            debug_config_snapshot=values[16],
            arg0=values[17],
            arg1=values[18],
        )

    values = struct.unpack(
        GET_FAULT_SNAPSHOT_FORMAT,
        payload[: struct.calcsize(GET_FAULT_SNAPSHOT_FORMAT)],
    )
    return FaultSnapshotView(
        event_id=values[2],
        time_ms=values[3],
        event_code=values[4],
        severity=values[5],
        reset_cause=values[6],
        sample_seq=values[7],
        last_progress_ms=values[8],
        fifo_no_data=values[9],
        sensor_errors=values[10],
        dropped_samples=values[11],
        rx_overflow_count=values[12],
        packet_overwrite_count=values[13],
        arg0=values[14],
        arg1=values[15],
    )


def parse_persistent_diagnostic_record_view(payload: bytes) -> PersistentDiagnosticRecordView:
    if len(payload) >= struct.calcsize(GET_PERSISTENT_DIAGNOSTIC_RECORD_FORMAT_V3):
        values = struct.unpack(
            GET_PERSISTENT_DIAGNOSTIC_RECORD_FORMAT_V3,
            payload[: struct.calcsize(GET_PERSISTENT_DIAGNOSTIC_RECORD_FORMAT_V3)],
        )
        return PersistentDiagnosticRecordView(
            generation=values[2],
            boot_counter=values[3],
            firmware_version=values[4],
            event_id=values[5],
            time_ms=values[6],
            event_code=values[7],
            severity=values[8],
            repeat_count=values[9],
            reset_cause=values[10],
            sample_seq=values[13],
            last_progress_ms=values[14],
            fifo_no_data=values[15],
            sensor_errors=values[16],
            dropped_samples=values[17],
            rx_overflow_count=values[18],
            packet_overwrite_count=values[19],
            debug_gpio_int1_edges=values[20],
            debug_gpio_drdy_edges=values[21],
            debug_config_snapshot=values[22],
            debug_irq_snapshot=values[23],
            arg0=values[24],
            arg1=values[25],
        )

    if len(payload) >= struct.calcsize(GET_PERSISTENT_DIAGNOSTIC_RECORD_FORMAT_V2):
        values = struct.unpack(
            GET_PERSISTENT_DIAGNOSTIC_RECORD_FORMAT_V2,
            payload[: struct.calcsize(GET_PERSISTENT_DIAGNOSTIC_RECORD_FORMAT_V2)],
        )
        return PersistentDiagnosticRecordView(
            generation=values[2],
            boot_counter=values[3],
            firmware_version=values[4],
            event_id=values[5],
            time_ms=values[6],
            event_code=values[7],
            severity=values[8],
            repeat_count=values[9],
            reset_cause=values[10],
            sample_seq=values[13],
            last_progress_ms=values[14],
            fifo_no_data=values[15],
            sensor_errors=values[16],
            dropped_samples=values[17],
            rx_overflow_count=values[18],
            packet_overwrite_count=values[19],
            debug_gpio_int1_edges=values[20],
            debug_gpio_drdy_edges=values[21],
            debug_config_snapshot=values[22],
            arg0=values[23],
            arg1=values[24],
        )

    values = struct.unpack(
        GET_PERSISTENT_DIAGNOSTIC_RECORD_FORMAT,
        payload[: struct.calcsize(GET_PERSISTENT_DIAGNOSTIC_RECORD_FORMAT)],
    )
    return PersistentDiagnosticRecordView(
        generation=values[2],
        boot_counter=values[3],
        firmware_version=values[4],
        event_id=values[5],
        time_ms=values[6],
        event_code=values[7],
        severity=values[8],
        repeat_count=values[9],
        reset_cause=values[10],
        sample_seq=values[13],
        last_progress_ms=values[14],
        fifo_no_data=values[15],
        sensor_errors=values[16],
        dropped_samples=values[17],
        rx_overflow_count=values[18],
        packet_overwrite_count=values[19],
        arg0=values[20],
        arg1=values[21],
    )


def parse_diagnostic_events(payload: bytes) -> tuple[int, int, list[DiagnosticEventView]]:
    header_size = struct.calcsize(READ_DIAGNOSTIC_EVENTS_HEADER_FORMAT)
    event_size = struct.calcsize(READ_DIAGNOSTIC_EVENT_FORMAT)
    command, status, returned_count, _reserved, first_event_id, next_event_id = struct.unpack(
        READ_DIAGNOSTIC_EVENTS_HEADER_FORMAT,
        payload[:header_size],
    )
    if command != CMD_READ_DIAGNOSTIC_EVENTS or status != 0:
        raise ValueError("diagnostic events payload header mismatch")

    events: list[DiagnosticEventView] = []
    offset = header_size
    for _ in range(returned_count):
        values = struct.unpack(
            READ_DIAGNOSTIC_EVENT_FORMAT,
            payload[offset:offset + event_size],
        )
        events.append(
            DiagnosticEventView(
                event_id=values[0],
                time_ms=values[1],
                event_code=values[2],
                severity=values[3],
                repeat_count=values[4],
                sample_seq=values[5],
                arg0=values[6],
                arg1=values[7],
            )
        )
        offset += event_size

    return first_event_id, next_event_id, events


def parse_commission_identity(payload: bytes) -> CommissionIdentity:
    if len(payload) < 3 + DEVICE_HARDWARE_ID_SIZE:
        raise ValueError("commission response payload too short")

    return CommissionIdentity(
        node_id=payload[2],
        hardware_id=bytes(payload[3:3 + DEVICE_HARDWARE_ID_SIZE]),
        board_revision=(
            payload[3 + DEVICE_HARDWARE_ID_SIZE]
            if len(payload) > 3 + DEVICE_HARDWARE_ID_SIZE
            else None
        ),
    )


def resolve_config(args: argparse.Namespace) -> HostConfig:
    config_path = Path(args.config)
    if not config_path.is_absolute() and not config_path.exists():
        config_path = Path(__file__).resolve().parent / args.config

    file_config = HostConfig.from_dict(load_json_config(config_path))
    return HostConfig(
        port=args.port if args.port is not None else file_config.port,
        baud=args.baud if args.baud is not None else file_config.baud,
        node=args.node if args.node is not None else file_config.node,
        timeout=args.timeout if args.timeout is not None else file_config.timeout,
    )


def maybe_sync_system_config(
    args: argparse.Namespace,
    config: HostConfig,
    previous_node_id: int,
    updated: ConfigView,
) -> None:
    if getattr(args, "no_sync_system_config", False):
        return
    raw_path = getattr(args, "system_config", DEFAULT_SYSTEM_CONFIG_PATH)
    if not raw_path:
        return

    system_config_path = Path(raw_path)
    if not system_config_path.is_absolute() and not system_config_path.exists():
        system_config_path = Path(__file__).resolve().parent.parent / raw_path

    result = sync_system_config_from_device_config(
        system_config_path,
        port=config.port,
        previous_node_id=previous_node_id,
        updated=updated,
    )
    print(
        f"[SYNC] updated {result.path} for channel={result.channel_name} node={result.node_id}"
    )


def parse_high_pass_corner_arg(raw: str) -> int:
    token = raw.strip().lower()
    if token in {"off", "disable", "disabled", "0"}:
        return 0

    try:
        value = int(token, 10)
    except ValueError as exc:
        raise argparse.ArgumentTypeError(
            "high pass must be 'off' or an integer from 0 to 7"
        ) from exc

    if value not in SUPPORTED_HIGH_PASS_CORNERS:
        raise argparse.ArgumentTypeError("high pass must be in range 0..7")

    return value


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Node configuration host for sensor-system firmware")
    parser.add_argument("--config", default="host_config.json")
    parser.add_argument(
        "--system-config",
        default=DEFAULT_SYSTEM_CONFIG_PATH,
        help="Host system_config.json to update after confirmed set/load/reset commands",
    )
    parser.add_argument(
        "--no-sync-system-config",
        action="store_true",
        help="Do not write confirmed device config changes back to system_config.json",
    )
    parser.add_argument("--port")
    parser.add_argument("--baud", type=int)
    parser.add_argument("--node", type=int)
    parser.add_argument("--timeout", type=float)

    sub = parser.add_subparsers(dest="command", required=True)

    scan = sub.add_parser("scan", help="Scan node ids with GetConfig")
    scan.add_argument("--start", type=int, default=1)
    scan.add_argument("--end", type=int, default=247)
    scan.add_argument("--per-node-timeout", type=float, default=0.08)

    sub.add_parser("get-config")
    sub.add_parser("get-status")
    sub.add_parser("get-diagnostic-info")
    sub.add_parser("get-fault-snapshot")
    sub.add_parser("get-persistent-diagnostic-record")
    dump_diag = sub.add_parser("dump-diagnostics")
    dump_diag.add_argument("--start-event-id", type=int, default=1)
    dump_diag.add_argument("--limit", type=int, default=32)

    read_diag_events = sub.add_parser("read-diagnostic-events")
    read_diag_events.add_argument("--start-event-id", type=int, default=1)
    read_diag_events.add_argument("--limit", type=int, default=16)

    sub.add_parser("clear-diagnostic-events")
    sub.add_parser("clear-persistent-diagnostic-record")
    sub.add_parser("restart")

    set_node = sub.add_parser("set-node-id")
    set_node.add_argument("value", type=int)

    set_odr = sub.add_parser("set-odr", help="Set ADXL355 sensor ODR")
    set_odr.add_argument("value", type=int, choices=SUPPORTED_ODR_HZ)

    set_range = sub.add_parser("set-range")
    set_range.add_argument("value", type=int)

    set_high_pass = sub.add_parser("set-high-pass", help="Set ADXL355 high-pass corner code; use 0/off to disable")
    set_high_pass.add_argument("value", type=parse_high_pass_corner_arg)

    set_offsets = sub.add_parser("set-offsets")
    set_offsets.add_argument("x", type=int)
    set_offsets.add_argument("y", type=int)
    set_offsets.add_argument("z", type=int)

    set_watermark = sub.add_parser("set-watermark")
    set_watermark.add_argument("value", type=int, choices=SUPPORTED_FIFO_WATERMARKS)

    set_baudrate = sub.add_parser("set-baudrate")
    set_baudrate.add_argument("value", type=int, choices=SUPPORTED_BAUD_RATES)
    set_baudrate.add_argument("--settle-ms", type=int, default=100)

    commission_scan = sub.add_parser("commission-scan", help="Scan unassigned devices by hardware id")
    commission_scan.add_argument("--slots", type=int, default=251)
    commission_scan.add_argument("--per-slot-timeout", type=float, default=0.03)

    commission_assign = sub.add_parser("commission-assign", help="Assign node id to an unassigned device")
    commission_assign.add_argument("--hardware-id", required=True, help="16 hex chars from commission-scan")
    commission_assign.add_argument("--node-id", required=True, type=int)

    sub.add_parser("save")
    sub.add_parser("load")
    sub.add_parser("reset-defaults")
    return parser


def send_and_wait(client: ProtocolClient, node_id: int, payload: bytes, timeout_s: float) -> Frame:
    started = time.monotonic()
    sequence = client.send_command(node_id, payload)
    response = client.wait_for_response(node_id, sequence, timeout_s)
    elapsed_ms = (time.monotonic() - started) * 1000.0

    if response is None:
        raise RuntimeError(f"no response within {timeout_s:.2f}s")

    if len(response.payload) < 2:
        raise RuntimeError("response payload too short")

    command = response.payload[0]
    status = response.payload[1]
    print(f"[RTT] {elapsed_ms:.2f} ms")

    if status != 0:
        raise RuntimeError(f"command 0x{command:02X} failed with status={status_name(status)}")

    return response


def format_effective_output_odr(sensor_odr_hz: int) -> str:
    output_odr = effective_output_odr_hz(sensor_odr_hz)
    if output_odr.is_integer():
        return str(int(output_odr))
    return f"{output_odr:g}"


def format_high_pass_corner(high_pass_corner: int) -> str:
    return "disabled" if high_pass_corner == 0 else str(high_pass_corner)


def format_filter_profile(filter_profile: int) -> str:
    return {0: "light", 1: "balanced", 2: "aggressive"}.get(
        filter_profile,
        f"unknown({filter_profile})",
    )


def print_config(config: ConfigView) -> None:
    print("Config:")
    print(f"  node_id        : {config.node_id}")
    print(f"  board_revision : {config.board_revision if config.board_revision is not None else 'unknown'}")
    print(f"  baudrate       : {config.baudrate}")
    print(f"  sensor_odr_hz  : {config.odr_hz}")
    print(f"  output_odr_hz  : {format_effective_output_odr(config.odr_hz)}")
    print(f"  range_g        : {config.range_g}")
    print(f"  high_pass      : {format_high_pass_corner(config.high_pass_corner)}")
    print(f"  high_pass_code : {config.high_pass_corner}")
    print(f"  filter_profile : {format_filter_profile(config.filter_profile)}")
    print(f"  decimation     : x{config.decimation_factor}")
    print(f"  config_revision: {config.config_revision}")
    print(f"  effective_seq  : {config.config_effective_sample_seq}")
    print(f"  offset_x       : {config.offset_x}")
    print(f"  offset_y       : {config.offset_y}")
    print(f"  offset_z       : {config.offset_z}")
    print(f"  fifo_watermark : {config.fifo_watermark} entries ({config.fifo_watermark // 3} XYZ samples)")
    print(f"  act_threshold  : {config.act_threshold}")
    print(f"  act_count      : {config.act_count}")


def print_status(status: StatusView) -> None:
    firmware_version = format_firmware_version(status.firmware_version)
    debug = decode_debug_config_snapshot(status.debug_config_snapshot)
    irq_debug = decode_irq_debug_snapshot(status.debug_irq_snapshot)
    print("Status:")
    print(f"  node_id          : {status.node_id}")
    print(f"  board_revision   : {status.board_revision if status.board_revision is not None else 'unknown'}")
    print(f"  node_state       : {status.node_state}")
    print(f"  sensor_odr_hz    : {status.odr_hz}")
    print(f"  output_odr_hz    : {format_effective_output_odr(status.odr_hz)}")
    print(f"  range_g          : {status.range_g}")
    print(f"  protocol_version : {status.protocol_version}")
    print(f"  firmware_version : {firmware_version}")
    print(f"  dropped_samples  : {status.dropped_samples}")
    print(f"  uptime_ms        : {status.uptime_ms}")
    print(f"  last_sample_seq  : {status.last_sample_seq}")
    print(f"  last_progress_ms : {status.last_progress_ms_ago}")
    print(f"  last_error_code  : {status.last_error_code}")
    print(f"  reset_cause      : {format_reset_cause(status.reset_cause)}")
    print(f"  diagnostic_flags : 0x{status.diagnostic_flags:02X}")
    print(f"  diag_flag_names  : {format_diagnostic_flags(status.diagnostic_flags)}")
    print(f"  poll_fallbacks   : {status.fifo_poll_fallback_reads}")
    print(f"  soft_recovers    : {status.soft_recover_count}")
    print(f"  no_data_irq      : {status.no_data_with_irq}")
    print(f"  no_data_poll     : {status.no_data_without_irq}")
    print(f"  irq_int1_events  : {status.irq_int1_events}")
    print(f"  irq_drdy_events  : {status.irq_drdy_events}")
    print(f"  gpio_int1_edges  : {status.gpio_int1_edges}")
    print(f"  gpio_drdy_edges  : {status.gpio_drdy_edges}")
    print(f"  int_map          : 0x{debug['int_map']:02X}")
    print(f"  fifo_samples     : {debug['fifo_samples']}")
    print(f"  int1_level       : {debug['int1_level']}")
    print(f"  drdy_level       : {debug['drdy_level']}")
    print(f"  irq_status_nf    : {status.irq_status_not_full}")
    print(f"  irq_entries_lt3  : {status.irq_fifo_entries_lt_3}")
    print(f"  irq_entries_ltwm : {status.irq_fifo_entries_lt_watermark}")
    print(f"  spurious_int1    : {status.spurious_int1_events}")
    print(f"  fifo_overruns    : {status.fifo_overrun_events}")
    print(f"  fifo_discarded   : {status.fifo_discarded_samples}")
    print(f"  fifo_loss_unknown: {status.fifo_uncertain_loss_events}")
    print(f"  drdy_ring_ovf    : {status.drdy_timestamp_ring_overflow}")
    print(f"  timing_mismatch  : {status.timing_binding_mismatch}")
    print(f"  timing_invalid   : {status.timing_binding_invalidations}")
    print(f"  timing_segment   : {status.timing_segment_id}")
    print(f"  irq_last_status  : 0x{irq_debug['status_reg']:02X}")
    print(f"  irq_last_entries : {irq_debug['fifo_entries']}")
    print(f"  irq_last_wm      : {irq_debug['watermark']}")
    print(f"  irq_last_flags   : {format_irq_debug_flags(irq_debug['flags'])}")


def format_reset_cause(reset_cause: int) -> str:
    return {
        0: "unknown",
        1: "power_on",
        2: "watchdog",
        3: "watchdog_timeout",
    }.get(reset_cause, f"unknown({reset_cause})")


def format_diagnostic_flags(flags: int) -> str:
    names: list[str] = []
    if flags & 0x01:
        names.append("fault_snapshot")
    if flags & 0x02:
        names.append("live_usb")
    if flags & 0x04:
        names.append("watchdog_reset")
    if flags & 0x08:
        names.append("sample_stall")
    if flags & 0x10:
        names.append("degraded_acquisition")
    if flags & 0x20:
        names.append("irq_fallback_active")
    if flags & 0x40:
        names.append("confirmed_data_loss")
    if flags & 0x80:
        names.append("uncertain_data_loss")
    return ",".join(names) if names else "none"


def format_firmware_version(raw_version: int) -> str:
    return (
        f"{(raw_version >> 16) & 0xFF}."
        f"{(raw_version >> 8) & 0xFF}."
        f"{raw_version & 0xFF}"
    )


def decode_debug_config_snapshot(raw_value: int) -> dict[str, int]:
    raw = raw_value & 0xFFFFFFFF
    return {
        "int_map": raw & 0xFF,
        "fifo_samples": (raw >> 8) & 0xFF,
        "int1_level": (raw >> 16) & 0xFF,
        "drdy_level": (raw >> 24) & 0xFF,
    }


def decode_irq_debug_snapshot(raw_value: int) -> dict[str, int]:
    raw = raw_value & 0xFFFFFFFF
    return {
        "status_reg": raw & 0xFF,
        "fifo_entries": (raw >> 8) & 0xFF,
        "watermark": (raw >> 16) & 0xFF,
        "flags": (raw >> 24) & 0xFF,
    }


def format_irq_debug_flags(flags: int) -> str:
    names: list[str] = []
    if flags & 0x01:
        names.append("status_fifo_full")
    if flags & 0x02:
        names.append("status_fifo_ovr")
    if flags & 0x04:
        names.append("entries_lt3")
    if flags & 0x08:
        names.append("entries_lt_watermark")
    return ",".join(names) if names else "none"


def format_diagnostic_severity(severity: int) -> str:
    return {
        0: "Debug",
        1: "Info",
        2: "Warn",
        3: "Error",
        4: "Critical",
    }.get(severity, f"Unknown({severity})")


def format_diagnostic_event_code(event_code: int) -> str:
    return {
        1: "Boot",
        2: "ControllerInitOk",
        3: "ControllerInitFailed",
        4: "Rs485InitFailed",
        5: "TransportInitFailed",
        16: "SensorInitFailed",
        17: "SensorCheckFailed",
        18: "SensorConfigFailed",
        19: "SensorOffsetsFailed",
        32: "FifoOverrun",
        33: "FifoStatusReadError",
        34: "FifoRepeatedNoData",
        35: "SensorReadError",
        36: "InvalidSample",
        37: "AcquisitionRecovered",
        48: "TemperatureReadFailed",
        49: "TemperatureReadRecovered",
        64: "RuntimeConfigApplyFailed",
        65: "RuntimeConfigApplied",
        80: "RestartCommand",
        81: "EnterBootloaderCommand",
        82: "DiagnosticsCleared",
    }.get(event_code, f"Unknown({event_code})")


def decode_sensor_snapshot_arg(arg0: int) -> dict[str, object]:
    raw = arg0 & 0xFFFFFFFF
    status_reg = raw & 0xFF
    fifo_entries = (raw >> 8) & 0xFF
    fifo_read_status = (raw >> 16) & 0xFF
    flags = (raw >> 24) & 0xFF
    return {
        "status_reg": status_reg,
        "fifo_entries": fifo_entries,
        "fifo_read_status": fifo_read_status,
        "irq_seen": bool(flags & 0x80),
        "empty_entry": bool(flags & 0x01),
        "axis_mismatch": bool(flags & 0x02),
        "int1_event": bool(flags & 0x04),
        "drdy_event": bool(flags & 0x08),
    }


def decode_sensor_status_streak_arg(arg1: int) -> tuple[int, int]:
    raw = arg1 & 0xFFFFFFFF
    sensor_status = raw & 0xFF
    streak = (raw >> 8) & 0x00FFFFFF
    return sensor_status, streak


def format_diagnostic_detail(event_code: int, arg0: int, arg1: int) -> str | None:
    if event_code in {32, 34, 35, 36, 33}:
        snapshot = decode_sensor_snapshot_arg(arg0)
        parts = [
            f"status_reg=0x{snapshot['status_reg']:02X}",
            f"fifo_entries={snapshot['fifo_entries']}",
            f"fifo_read_status={sensor_status_name(int(snapshot['fifo_read_status']))}",
            f"irq_seen={snapshot['irq_seen']}",
            f"empty_entry={snapshot['empty_entry']}",
            f"axis_mismatch={snapshot['axis_mismatch']}",
            f"int1_event={snapshot['int1_event']}",
            f"drdy_event={snapshot['drdy_event']}",
        ]
        if event_code == 32:
            parts.append(f"dropped_counter={arg1}")
        elif event_code == 34:
            parts.append(f"no_data_streak={arg1}")
        else:
            sensor_status, streak = decode_sensor_status_streak_arg(arg1)
            parts.append(f"sensor_status={sensor_status_name(sensor_status)}")
            parts.append(f"error_streak={streak}")
        return ", ".join(parts)

    if event_code == 37:
        return f"previous_no_data={arg0}, previous_sensor_errors={arg1}"

    return None


def print_diagnostic_info(info: DiagnosticInfoView) -> None:
    print("Diagnostic Info:")
    print(f"  uptime_ms           : {info.uptime_ms}")
    print(f"  reset_cause         : {format_reset_cause(info.reset_cause)}")
    print(f"  live_usb_enabled    : {bool(info.live_usb_enabled)}")
    print(f"  stored_event_count  : {info.stored_event_count}")
    print(f"  event_capacity      : {info.event_capacity}")
    print(f"  dropped_event_count : {info.dropped_event_count}")
    print(f"  first_event_id      : {info.first_event_id}")
    print(f"  next_event_id       : {info.next_event_id}")
    print(f"  last_error_event_id : {info.last_error_event_id}")
    print(f"  last_error_code     : {format_diagnostic_event_code(info.last_error_code)}")


def print_fault_snapshot(snapshot: FaultSnapshotView) -> None:
    debug = decode_debug_config_snapshot(snapshot.debug_config_snapshot)
    irq_debug = decode_irq_debug_snapshot(snapshot.debug_irq_snapshot)
    print("Fault Snapshot:")
    print(f"  event_id               : {snapshot.event_id}")
    print(f"  time_ms                : {snapshot.time_ms}")
    print(f"  event_code             : {format_diagnostic_event_code(snapshot.event_code)}")
    print(f"  severity               : {format_diagnostic_severity(snapshot.severity)}")
    print(f"  reset_cause            : {format_reset_cause(snapshot.reset_cause)}")
    print(f"  sample_seq             : {snapshot.sample_seq}")
    print(f"  last_progress_ms       : {snapshot.last_progress_ms}")
    print(f"  fifo_no_data           : {snapshot.fifo_no_data}")
    print(f"  sensor_errors          : {snapshot.sensor_errors}")
    print(f"  dropped_samples        : {snapshot.dropped_samples}")
    print(f"  rx_overflow_count      : {snapshot.rx_overflow_count}")
    print(f"  packet_overwrite_count : {snapshot.packet_overwrite_count}")
    print(f"  debug_gpio_int1_edges  : {snapshot.debug_gpio_int1_edges}")
    print(f"  debug_gpio_drdy_edges  : {snapshot.debug_gpio_drdy_edges}")
    print(f"  debug_int_map          : 0x{debug['int_map']:02X}")
    print(f"  debug_fifo_samples     : {debug['fifo_samples']}")
    print(f"  debug_int1_level       : {debug['int1_level']}")
    print(f"  debug_drdy_level       : {debug['drdy_level']}")
    print(f"  debug_irq_status       : 0x{irq_debug['status_reg']:02X}")
    print(f"  debug_irq_entries      : {irq_debug['fifo_entries']}")
    print(f"  debug_irq_watermark    : {irq_debug['watermark']}")
    print(f"  debug_irq_flags        : {format_irq_debug_flags(irq_debug['flags'])}")
    print(f"  arg0                   : {snapshot.arg0}")
    print(f"  arg1                   : {snapshot.arg1}")
    detail = format_diagnostic_detail(snapshot.event_code, snapshot.arg0, snapshot.arg1)
    if detail is not None:
        print(f"  detail                 : {detail}")


def print_persistent_diagnostic_record(record: PersistentDiagnosticRecordView) -> None:
    debug = decode_debug_config_snapshot(record.debug_config_snapshot)
    irq_debug = decode_irq_debug_snapshot(record.debug_irq_snapshot)
    print("Persistent Diagnostic Record:")
    print(f"  generation             : {record.generation}")
    print(f"  boot_counter           : {record.boot_counter}")
    print(f"  firmware_version       : {format_firmware_version(record.firmware_version)}")
    print(f"  event_id               : {record.event_id}")
    print(f"  time_ms                : {record.time_ms}")
    print(f"  event_code             : {format_diagnostic_event_code(record.event_code)}")
    print(f"  severity               : {format_diagnostic_severity(record.severity)}")
    print(f"  repeat_count           : {record.repeat_count}")
    print(f"  reset_cause            : {format_reset_cause(record.reset_cause)}")
    print(f"  sample_seq             : {record.sample_seq}")
    print(f"  last_progress_ms       : {record.last_progress_ms}")
    print(f"  fifo_no_data           : {record.fifo_no_data}")
    print(f"  sensor_errors          : {record.sensor_errors}")
    print(f"  dropped_samples        : {record.dropped_samples}")
    print(f"  rx_overflow_count      : {record.rx_overflow_count}")
    print(f"  packet_overwrite_count : {record.packet_overwrite_count}")
    print(f"  debug_gpio_int1_edges  : {record.debug_gpio_int1_edges}")
    print(f"  debug_gpio_drdy_edges  : {record.debug_gpio_drdy_edges}")
    print(f"  debug_int_map          : 0x{debug['int_map']:02X}")
    print(f"  debug_fifo_samples     : {debug['fifo_samples']}")
    print(f"  debug_int1_level       : {debug['int1_level']}")
    print(f"  debug_drdy_level       : {debug['drdy_level']}")
    print(f"  debug_irq_status       : 0x{irq_debug['status_reg']:02X}")
    print(f"  debug_irq_entries      : {irq_debug['fifo_entries']}")
    print(f"  debug_irq_watermark    : {irq_debug['watermark']}")
    print(f"  debug_irq_flags        : {format_irq_debug_flags(irq_debug['flags'])}")
    print(f"  arg0                   : {record.arg0}")
    print(f"  arg1                   : {record.arg1}")
    detail = format_diagnostic_detail(record.event_code, record.arg0, record.arg1)
    if detail is not None:
        print(f"  detail                 : {detail}")


def print_diagnostic_events(first_event_id: int,
                            next_event_id: int,
                            events: list[DiagnosticEventView]) -> None:
    print("Diagnostic Events:")
    print(f"  first_event_id : {first_event_id}")
    print(f"  next_event_id  : {next_event_id}")
    print(f"  returned_count : {len(events)}")
    for event in events:
        detail = format_diagnostic_detail(event.event_code, event.arg0, event.arg1)
        print(
            "  "
            f"event_id={event.event_id} time_ms={event.time_ms} "
            f"severity={format_diagnostic_severity(event.severity)} "
            f"code={format_diagnostic_event_code(event.event_code)} "
            f"repeat_count={event.repeat_count} "
            f"sample_seq={event.sample_seq} arg0={event.arg0} arg1={event.arg1}"
        )
        if detail is not None:
            print(f"    detail={detail}")


def read_all_diagnostic_events(
    client: ProtocolClient,
    node_id: int,
    timeout_s: float,
    *,
    start_event_id: int = 1,
    limit: int = 32,
) -> tuple[int, int, list[DiagnosticEventView]]:
    all_events: list[DiagnosticEventView] = []
    request_start = max(0, start_event_id)
    limit = max(1, min(limit, 32))
    first_event_id = request_start
    next_event_id = request_start

    while True:
        payload = struct.pack(
            READ_DIAGNOSTIC_EVENTS_REQUEST_FORMAT,
            CMD_READ_DIAGNOSTIC_EVENTS,
            request_start,
            limit,
        )
        response = send_and_wait(client, node_id, payload, timeout_s)
        batch_first_event_id, batch_next_event_id, events = parse_diagnostic_events(response.payload)
        first_event_id = batch_first_event_id
        next_event_id = batch_next_event_id
        all_events.extend(events)

        if not events:
            break

        request_start = events[-1].event_id + 1
        if request_start >= batch_next_event_id:
            break

    return first_event_id, next_event_id, all_events


def format_hardware_id(hardware_id: bytes) -> str:
    return hardware_id.hex().upper()


def parse_hardware_id(raw: str) -> bytes:
    cleaned = raw.strip().replace("-", "").replace(":", "")
    if len(cleaned) != DEVICE_HARDWARE_ID_SIZE * 2:
        raise RuntimeError("hardware id must be 16 hex characters")

    try:
        return bytes.fromhex(cleaned)
    except ValueError as exc:
        raise RuntimeError(f"invalid hardware id: {exc}") from exc


def scan_nodes(client: ProtocolClient,
               start_node: int,
               end_node: int,
               timeout_s: float) -> int:
    found = 0;
    start_node = max(1, start_node)
    end_node = min(247, end_node)
    if start_node > end_node:
        raise RuntimeError("invalid scan range")

    print(f"Scanning nodes {start_node}..{end_node} on current port")
    for node_id in range(start_node, end_node + 1):
        sequence = client.send_command(node_id, bytes([CMD_GET_CONFIG]))
        response = client.wait_for_response(node_id, sequence, timeout_s)
        if response is None or len(response.payload) < struct.calcsize(GET_CONFIG_FORMAT):
            continue

        config = parse_config_view(response.payload)
        print(
            f"  node={node_id} baud={config.baudrate} sensor_odr={config.odr_hz}Hz "
            f"output_odr={format_effective_output_odr(config.odr_hz)}Hz "
            f"range={config.range_g}g high_pass={format_high_pass_corner(config.high_pass_corner)} "
            f"watermark={config.fifo_watermark}"
        )
        found += 1

    if found == 0:
        print("  no nodes found")
    return found


def wait_for_commission_response(client: ProtocolClient,
                                 sequence: int,
                                 timeout_s: float,
                                 expected_command: int,
                                 expected_hardware_id: bytes | None = None) -> Optional[Frame]:
    deadline = time.monotonic() + timeout_s

    def predicate(frame: Frame) -> bool:
        if frame.frame_type != FRAME_TYPE_RESPONSE:
            return False
        if frame.destination != HOST_NODE_ID or frame.sequence != sequence:
            return False
        if len(frame.payload) < 2 or frame.payload[0] != expected_command or frame.payload[1] != 0:
            return False
        if expected_hardware_id is None:
            return True

        try:
            identity = parse_commission_identity(frame.payload)
        except ValueError:
            return False
        return identity.hardware_id == expected_hardware_id

    return client.poll_for_matching_frame(predicate, deadline)


def commission_scan(client: ProtocolClient, slot_count: int, timeout_s: float) -> list[CommissionIdentity]:
    if slot_count <= 0:
        raise RuntimeError("slot count must be positive")

    print(f"Scanning {slot_count} commissioning slots on current port")
    discovered: dict[bytes, CommissionIdentity] = {}

    for slot_index in range(slot_count):
        payload = struct.pack("<BHH", CMD_COMMISSION_DISCOVER, slot_count, slot_index)
        sequence = client.send_command(BROADCAST_NODE_ID, payload)
        response = wait_for_commission_response(
            client,
            sequence,
            timeout_s,
            CMD_COMMISSION_DISCOVER,
        )
        if response is None:
            continue

        identity = parse_commission_identity(response.payload)
        discovered[identity.hardware_id] = identity

    if not discovered:
        print("  no unassigned nodes found")
        return []

    ordered = [discovered[key] for key in sorted(discovered)]
    for identity in ordered:
        print(
            f"  hardware_id={format_hardware_id(identity.hardware_id)} "
            f"node_id={identity.node_id} "
            f"board_revision={identity.board_revision if identity.board_revision is not None else 'unknown'}"
        )
    return ordered


def commission_assign(client: ProtocolClient,
                      hardware_id: bytes,
                      node_id: int,
                      timeout_s: float) -> ConfigView:
    if node_id <= 0 or node_id >= BROADCAST_NODE_ID or node_id == HOST_NODE_ID:
        raise RuntimeError("node id must be in range 1..254 excluding host id")

    payload = struct.pack(
        "<B8sB",
        CMD_COMMISSION_ASSIGN_NODE_ID,
        hardware_id,
        node_id,
    )
    sequence = client.send_command(BROADCAST_NODE_ID, payload)
    response = wait_for_commission_response(
        client,
        sequence,
        timeout_s,
        CMD_COMMISSION_ASSIGN_NODE_ID,
        expected_hardware_id=hardware_id,
    )
    if response is None:
        raise RuntimeError("no commissioning acknowledgement received")

    identity = parse_commission_identity(response.payload)
    if identity.node_id != node_id:
        raise RuntimeError(
            f"device acknowledged node_id={identity.node_id} instead of requested {node_id}"
        )

    print(
        f"[OK] assigned hardware_id={format_hardware_id(identity.hardware_id)} "
        f"to node_id={identity.node_id}"
    )
    verify = send_and_wait(client, node_id, bytes([CMD_GET_CONFIG]), timeout_s)
    config = parse_config_view(verify.payload)
    if config.node_id != node_id:
        raise RuntimeError(f"verification mismatch: node reports node_id={config.node_id}")
    return config


def main() -> int:
    args = build_parser().parse_args()
    config = resolve_config(args)

    try:
        ser = serial.Serial(
            port=config.port,
            baudrate=config.baud,
            timeout=0.05,
            write_timeout=0.5,
        )
    except serial.SerialException as exc:
        print(f"[ERROR] cannot open serial port {config.port}: {exc}", file=sys.stderr)
        return 2

    client = ProtocolClient(ser)

    try:
        if args.command == "scan":
            scan_nodes(client, args.start, args.end, args.per_node_timeout)
            return 0

        if args.command == "get-config":
            response = send_and_wait(client, config.node, bytes([CMD_GET_CONFIG]), config.timeout)
            print_config(parse_config_view(response.payload))
            return 0

        if args.command == "get-status":
            response = send_and_wait(client, config.node, bytes([CMD_GET_STATUS]), config.timeout)
            print_status(parse_status_view(response.payload))
            return 0

        if args.command == "get-diagnostic-info":
            response = send_and_wait(
                client,
                config.node,
                bytes([CMD_GET_DIAGNOSTIC_INFO]),
                config.timeout,
            )
            print_diagnostic_info(parse_diagnostic_info_view(response.payload))
            return 0

        if args.command == "get-fault-snapshot":
            response = send_and_wait(
                client,
                config.node,
                bytes([CMD_GET_FAULT_SNAPSHOT]),
                config.timeout,
            )
            print_fault_snapshot(parse_fault_snapshot_view(response.payload))
            return 0

        if args.command == "get-persistent-diagnostic-record":
            response = send_and_wait(
                client,
                config.node,
                bytes([CMD_GET_PERSISTENT_DIAGNOSTIC_RECORD]),
                config.timeout,
            )
            print_persistent_diagnostic_record(parse_persistent_diagnostic_record_view(response.payload))
            return 0

        if args.command == "dump-diagnostics":
            status_response = send_and_wait(
                client,
                config.node,
                bytes([CMD_GET_STATUS]),
                config.timeout,
            )
            print_status(parse_status_view(status_response.payload))
            print()

            info_response = send_and_wait(
                client,
                config.node,
                bytes([CMD_GET_DIAGNOSTIC_INFO]),
                config.timeout,
            )
            info = parse_diagnostic_info_view(info_response.payload)
            print_diagnostic_info(info)
            print()

            try:
                fault_response = send_and_wait(
                    client,
                    config.node,
                    bytes([CMD_GET_FAULT_SNAPSHOT]),
                    config.timeout,
                )
            except RuntimeError as exc:
                print(f"Fault Snapshot:\n  unavailable          : {exc}")
            else:
                print_fault_snapshot(parse_fault_snapshot_view(fault_response.payload))
            print()

            try:
                persistent_response = send_and_wait(
                    client,
                    config.node,
                    bytes([CMD_GET_PERSISTENT_DIAGNOSTIC_RECORD]),
                    config.timeout,
                )
            except RuntimeError as exc:
                print(f"Persistent Diagnostic Record:\n  unavailable          : {exc}")
            else:
                print_persistent_diagnostic_record(
                    parse_persistent_diagnostic_record_view(persistent_response.payload)
                )
            print()

            first_event_id, next_event_id, events = read_all_diagnostic_events(
                client,
                config.node,
                config.timeout,
                start_event_id=args.start_event_id,
                limit=args.limit,
            )
            print_diagnostic_events(first_event_id, next_event_id, events)
            return 0

        if args.command == "read-diagnostic-events":
            payload = struct.pack(
                READ_DIAGNOSTIC_EVENTS_REQUEST_FORMAT,
                CMD_READ_DIAGNOSTIC_EVENTS,
                max(0, args.start_event_id),
                max(1, min(args.limit, 32)),
            )
            response = send_and_wait(client, config.node, payload, config.timeout)
            first_event_id, next_event_id, events = parse_diagnostic_events(response.payload)
            print_diagnostic_events(first_event_id, next_event_id, events)
            return 0

        if args.command == "clear-diagnostic-events":
            send_and_wait(
                client,
                config.node,
                bytes([CMD_CLEAR_DIAGNOSTIC_EVENTS]),
                config.timeout,
            )
            print("[OK] diagnostic events cleared")
            return 0

        if args.command == "clear-persistent-diagnostic-record":
            send_and_wait(
                client,
                config.node,
                bytes([CMD_CLEAR_PERSISTENT_DIAGNOSTIC_RECORD]),
                config.timeout,
            )
            print("[OK] persistent diagnostic record cleared")
            return 0

        if args.command == "restart":
            send_and_wait(client, config.node, bytes([CMD_RESTART]), config.timeout)
            print("[OK] restart command acknowledged")
            return 0

        if args.command == "set-node-id":
            send_and_wait(client, config.node, struct.pack("<BB", CMD_SET_NODE_ID, args.value), config.timeout)
            response = send_and_wait(client, args.value, bytes([CMD_GET_CONFIG]), config.timeout)
            updated = parse_config_view(response.payload)
            print_config(updated)
            maybe_sync_system_config(args, config, config.node, updated)
            return 0

        if args.command == "set-odr":
            send_and_wait(client, config.node, struct.pack("<BH", CMD_SET_ODR, args.value), config.timeout)
            response = send_and_wait(client, config.node, bytes([CMD_GET_CONFIG]), config.timeout)
            updated = parse_config_view(response.payload)
            print_config(updated)
            maybe_sync_system_config(args, config, config.node, updated)
            return 0

        if args.command == "set-range":
            send_and_wait(client, config.node, struct.pack("<BB", CMD_SET_RANGE, args.value), config.timeout)
            response = send_and_wait(client, config.node, bytes([CMD_GET_CONFIG]), config.timeout)
            updated = parse_config_view(response.payload)
            print_config(updated)
            maybe_sync_system_config(args, config, config.node, updated)
            return 0

        if args.command == "set-high-pass":
            send_and_wait(
                client,
                config.node,
                struct.pack("<BB", CMD_SET_HIGH_PASS, args.value),
                config.timeout,
            )
            response = send_and_wait(client, config.node, bytes([CMD_GET_CONFIG]), config.timeout)
            updated = parse_config_view(response.payload)
            print_config(updated)
            maybe_sync_system_config(args, config, config.node, updated)
            return 0

        if args.command == "set-offsets":
            payload = struct.pack("<Biii", CMD_SET_OFFSETS, args.x, args.y, args.z)
            send_and_wait(client, config.node, payload, config.timeout)
            response = send_and_wait(client, config.node, bytes([CMD_GET_CONFIG]), config.timeout)
            updated = parse_config_view(response.payload)
            print_config(updated)
            maybe_sync_system_config(args, config, config.node, updated)
            return 0

        if args.command == "set-watermark":
            send_and_wait(client, config.node, struct.pack("<BB", CMD_SET_FIFO_WATERMARK, args.value), config.timeout)
            response = send_and_wait(client, config.node, bytes([CMD_GET_CONFIG]), config.timeout)
            updated = parse_config_view(response.payload)
            print_config(updated)
            maybe_sync_system_config(args, config, config.node, updated)
            return 0

        if args.command == "set-baudrate":
            send_and_wait(client, config.node, struct.pack("<BI", CMD_SET_BAUD_RATE, args.value), config.timeout)
            client.rx_buffer.clear()
            time.sleep(max(args.settle_ms, 0) / 1000.0)
            ser.baudrate = args.value
            response = send_and_wait(client, config.node, bytes([CMD_GET_CONFIG]), config.timeout)
            updated = parse_config_view(response.payload)
            if updated.baudrate != args.value:
                raise RuntimeError(
                    f"node responded, but reported baudrate={updated.baudrate} instead of {args.value}"
                )
            print(f"[OK] host switched to baud={args.value} for verification")
            print_config(updated)
            maybe_sync_system_config(args, config, config.node, updated)
            return 0

        if args.command == "commission-scan":
            commission_scan(client, args.slots, args.per_slot_timeout)
            return 0

        if args.command == "commission-assign":
            hardware_id = parse_hardware_id(args.hardware_id)
            config_view = commission_assign(client, hardware_id, args.node_id, config.timeout)
            print_config(config_view)
            return 0

        if args.command == "save":
            send_and_wait(client, config.node, bytes([CMD_SAVE_CONFIG]), config.timeout)
            print("[OK] configuration saved")
            return 0

        if args.command == "load":
            send_and_wait(client, config.node, bytes([CMD_LOAD_CONFIG]), config.timeout)
            response = send_and_wait(client, config.node, bytes([CMD_GET_CONFIG]), config.timeout)
            updated = parse_config_view(response.payload)
            print_config(updated)
            maybe_sync_system_config(args, config, config.node, updated)
            return 0

        if args.command == "reset-defaults":
            send_and_wait(client, config.node, bytes([CMD_RESET_CONFIG_TO_DEFAULTS]), config.timeout)
            response = send_and_wait(client, config.node, bytes([CMD_GET_CONFIG]), config.timeout)
            updated = parse_config_view(response.payload)
            print_config(updated)
            maybe_sync_system_config(args, config, config.node, updated)
            return 0

        raise RuntimeError(f"unsupported command: {args.command}")
    except RuntimeError as exc:
        print(f"[ERROR] {exc}", file=sys.stderr)
        return 1
    finally:
        ser.close()


if __name__ == "__main__":
    raise SystemExit(main())
