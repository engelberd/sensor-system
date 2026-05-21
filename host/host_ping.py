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

HOST_NODE_ID = 0xFE
PING_COMMAND = 0x01
STATUS_OK = 0x00


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


def format_hex(data: bytes) -> str:
    if not data:
        return "(empty)"
    return " ".join(f"{byte:02X}" for byte in data)


def parse_payload(payload: str, hex_mode: bool) -> bytes:
    if not hex_mode:
        return payload.encode("utf-8")

    cleaned = payload.replace(" ", "").replace(":", "")
    if len(cleaned) % 2 != 0:
        raise ValueError("hex payload must contain an even number of digits")
    return bytes.fromhex(cleaned)


def load_json_config(path: Path) -> dict:
    if not path.exists():
        return {}

    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


@dataclass
class HostConfig:
    port: str = "/dev/sensor-system-rs485"
    baud: int = 115200
    node: int = 1
    timeout: float = 2.0
    payload: str = "ping"
    hex_payload: bool = False

    @classmethod
    def from_dict(cls, data: dict) -> "HostConfig":
        return cls(
            port=str(data.get("port", cls.port)),
            baud=int(data.get("baud", cls.baud)),
            node=int(data.get("node", cls.node)),
            timeout=float(data.get("timeout", cls.timeout)),
            payload=str(data.get("payload", cls.payload)),
            hex_payload=bool(data.get("hex_payload", cls.hex_payload)),
        )


@dataclass
class Frame:
    frame_type: int
    flags: int
    destination: int
    source: int
    sequence: int
    payload: bytes


class FrameCodec:
    @staticmethod
    def encode(
        frame_type: int,
        destination: int,
        source: int,
        sequence: int,
        payload: bytes,
    ) -> bytes:
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

        if magic != FRAME_MAGIC or version != FRAME_PROTOCOL_VERSION:
            del rx_buffer[0]
            return None

        if payload_length > FRAME_MAX_PAYLOAD_SIZE:
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


class PingClient:
    def __init__(self, ser: serial.Serial, host_node_id: int = HOST_NODE_ID) -> None:
        self.ser = ser
        self.host_node_id = host_node_id
        self.rx_buffer = bytearray()
        self.sequence = 1

    def next_sequence(self) -> int:
        current = self.sequence
        self.sequence = 1 if self.sequence == 0xFFFFFFFF else self.sequence + 1
        return current

    def send_ping(self, node_id: int, user_payload: bytes) -> int:
        sequence = self.next_sequence()
        payload = bytes([PING_COMMAND]) + user_payload
        frame = FrameCodec.encode(
            frame_type=FRAME_TYPE_COMMAND,
            destination=node_id,
            source=self.host_node_id,
            sequence=sequence,
            payload=payload,
        )
        self.ser.reset_input_buffer()
        self.ser.write(frame)
        self.ser.flush()
        return sequence

    def wait_for_ping_response(self, node_id: int, sequence: int, timeout_s: float) -> Optional[Frame]:
        deadline = time.monotonic() + timeout_s

        while time.monotonic() < deadline:
            chunk = self.ser.read(256)
            if chunk:
                self.rx_buffer.extend(chunk)

            while True:
                buffer_size_before = len(self.rx_buffer)
                frame = FrameCodec.try_decode(self.rx_buffer)
                if frame is None:
                    if len(self.rx_buffer) == buffer_size_before:
                        break
                    continue

                if (
                    frame.frame_type == FRAME_TYPE_RESPONSE
                    and frame.source == node_id
                    and frame.destination == self.host_node_id
                    and frame.sequence == sequence
                ):
                    return frame

        return None


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Simple RS ping tester for sensor-system node firmware")
    parser.add_argument("--config", default="host_config.json", help="Path to JSON config file")
    parser.add_argument("--port", help="Serial port, e.g. /dev/sensor-system-rs485")
    parser.add_argument("--baud", type=int, help="Serial baud rate")
    parser.add_argument("--node", type=int, help="Destination node id")
    parser.add_argument("--timeout", type=float, help="Response timeout in seconds")
    parser.add_argument("--payload", help="ASCII payload appended after ping command byte")
    parser.add_argument("--hex-payload", action="store_true", help="Interpret payload as hex bytes")
    return parser


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
        payload=args.payload if args.payload is not None else file_config.payload,
        hex_payload=args.hex_payload or file_config.hex_payload,
    )


def main() -> int:
    args = build_parser().parse_args()
    config = resolve_config(args)

    try:
        user_payload = parse_payload(config.payload, config.hex_payload)
    except ValueError as exc:
        print(f"[ERROR] invalid payload: {exc}", file=sys.stderr)
        return 2

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

    client = PingClient(ser)
    request_payload = bytes([PING_COMMAND]) + user_payload

    print(
        f"[HOST] port={config.port} baud={config.baud} node={config.node} "
        f"timeout={config.timeout:.2f}s"
    )
    print(f"[TX] payload={format_hex(request_payload)}")

    started_at = time.monotonic()
    try:
        sequence = client.send_ping(config.node, user_payload)
        response = client.wait_for_ping_response(config.node, sequence, config.timeout)
    finally:
        ser.close()

    if response is None:
        print(f"[TIMEOUT] no response for seq={sequence} within {config.timeout:.2f}s")
        return 1

    elapsed_ms = (time.monotonic() - started_at) * 1000.0
    print(
        f"[RX] seq={response.sequence} from={response.source} to={response.destination} "
        f"len={len(response.payload)} rtt={elapsed_ms:.2f} ms"
    )
    print(f"[RX] payload={format_hex(response.payload)}")

    if len(response.payload) < 2:
        print("[ERROR] response payload too short, expected at least: <command> <status>")
        return 1

    response_command = response.payload[0]
    response_status = response.payload[1]

    if response_command != PING_COMMAND:
        print(
            f"[ERROR] response command mismatch: expected 0x{PING_COMMAND:02X}, "
            f"got 0x{response_command:02X}"
        )
        return 1

    if response_status != STATUS_OK:
        print(f"[ERROR] ping failed with status=0x{response_status:02X}")
        return 1

    print("[OK] ping response is consistent with firmware protocol: 01 00")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
