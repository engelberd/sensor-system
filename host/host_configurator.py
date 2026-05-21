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
CMD_GET_STATUS = 0x40

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

# Actual packed layouts from firmware.
GET_CONFIG_FORMAT = "<BBBIHBiiiBHBB"
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


@dataclass
class StatusView:
    node_id: int
    node_state: int
    odr_hz: int
    range_g: int
    protocol_version: int
    firmware_version: int
    dropped_samples: int


@dataclass
class CommissionIdentity:
    node_id: int
    hardware_id: bytes


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
    matched_entry["fifo_watermark"] = updated.fifo_watermark
    matched_entry["offset_x"] = updated.offset_x
    matched_entry["offset_y"] = updated.offset_y
    matched_entry["offset_z"] = updated.offset_z
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
    values = struct.unpack(GET_CONFIG_FORMAT, payload[: struct.calcsize(GET_CONFIG_FORMAT)])
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
    )


def parse_status_view(payload: bytes) -> StatusView:
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


def parse_commission_identity(payload: bytes) -> CommissionIdentity:
    if len(payload) < 3 + DEVICE_HARDWARE_ID_SIZE:
        raise ValueError("commission response payload too short")

    return CommissionIdentity(
        node_id=payload[2],
        hardware_id=bytes(payload[3:3 + DEVICE_HARDWARE_ID_SIZE]),
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


def print_config(config: ConfigView) -> None:
    print("Config:")
    print(f"  node_id        : {config.node_id}")
    print(f"  baudrate       : {config.baudrate}")
    print(f"  sensor_odr_hz  : {config.odr_hz}")
    print(f"  output_odr_hz  : {format_effective_output_odr(config.odr_hz)}")
    print(f"  range_g        : {config.range_g}")
    print(f"  high_pass      : {format_high_pass_corner(config.high_pass_corner)}")
    print(f"  high_pass_code : {config.high_pass_corner}")
    print(f"  offset_x       : {config.offset_x}")
    print(f"  offset_y       : {config.offset_y}")
    print(f"  offset_z       : {config.offset_z}")
    print(f"  fifo_watermark : {config.fifo_watermark}")
    print(f"  act_threshold  : {config.act_threshold}")
    print(f"  act_count      : {config.act_count}")


def print_status(status: StatusView) -> None:
    firmware_version = (
        f"{(status.firmware_version >> 16) & 0xFF}."
        f"{(status.firmware_version >> 8) & 0xFF}."
        f"{status.firmware_version & 0xFF}"
    )
    print("Status:")
    print(f"  node_id          : {status.node_id}")
    print(f"  node_state       : {status.node_state}")
    print(f"  sensor_odr_hz    : {status.odr_hz}")
    print(f"  output_odr_hz    : {format_effective_output_odr(status.odr_hz)}")
    print(f"  range_g          : {status.range_g}")
    print(f"  protocol_version : {status.protocol_version}")
    print(f"  firmware_version : {firmware_version}")
    print(f"  dropped_samples  : {status.dropped_samples}")


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
            f"node_id={identity.node_id}"
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
