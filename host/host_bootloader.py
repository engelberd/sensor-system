#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import struct
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Optional, Protocol

try:
    import serial  # type: ignore
    from serial.tools import list_ports  # type: ignore
except ModuleNotFoundError:  # pragma: no cover - exercised in tests without pyserial
    serial = None  # type: ignore[assignment]
    list_ports = None  # type: ignore[assignment]


class SerialLike(Protocol):
    def read(self, size: int = ...) -> bytes: ...

    def write(self, data: bytes) -> int: ...

    def flush(self) -> None: ...

    def reset_input_buffer(self) -> None: ...

    def close(self) -> None: ...


@dataclass(frozen=True)
class SerialIdentity:
    vid: Optional[int]
    pid: Optional[int]
    serial_number: Optional[str]
    location: Optional[str]



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
ENTER_BOOTLOADER_COMMAND = 0x03
STATUS_OK = 0x00

UPDATE_PACKET_MAGIC = 0x55505444
UPDATE_PROTOCOL_VERSION = 2
UPDATE_HEADER_FORMAT = "<IBBHI"
UPDATE_HEADER_SIZE = struct.calcsize(UPDATE_HEADER_FORMAT)
UPDATE_CRC_SIZE = 4
UPDATE_MAX_PAYLOAD_SIZE = 1024 + 32

UPDATE_TYPE_HELLO = 1
UPDATE_TYPE_BEGIN = 2
UPDATE_TYPE_CHUNK = 3
UPDATE_TYPE_END = 4
UPDATE_TYPE_ABORT = 5
UPDATE_TYPE_ACK = 100
UPDATE_TYPE_ERROR = 101

SLOT_A = 1
SLOT_B = 2

UPDATE_STATUS_NAMES = {
    0: "Ok",
    1: "BadPacket",
    2: "BadState",
    3: "BadOffset",
    4: "BadLength",
    5: "BadCrc",
    6: "FlashError",
    7: "ImageTooLarge",
    8: "InvalidSlot",
    9: "InternalError",
    10: "Timeout",
}

FLASH_PAGE_SIZE = 256
DEFAULT_UPDATE_CHUNK_SIZE = 1024


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


def update_crc32(data: bytes) -> int:
    if not data:
        return 0

    crc = 0xFFFFFFFF
    for byte in data:
        crc ^= byte
        for _ in range(8):
            if crc & 1:
                crc = (crc >> 1) ^ 0xEDB88320
            else:
                crc >>= 1
    return (~crc) & 0xFFFFFFFF


def format_hex(data: bytes, limit: int = 64) -> str:
    if not data:
        return "(empty)"

    if len(data) <= limit:
        return " ".join(f"{byte:02X}" for byte in data)

    preview = " ".join(f"{byte:02X}" for byte in data[:limit])
    return f"{preview} ... ({len(data)} bytes)"


def load_json_config(path: Path) -> dict:
    if not path.exists():
        return {}

    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def open_serial_port(config: "HostConfig") -> SerialLike:
    return serial.Serial(  # type: ignore[union-attr]
        port=config.port,
        baudrate=config.baud,
        timeout=0.05,
        write_timeout=max(2.0, config.timeout),
    )


def serial_identity_for_port(port: str) -> Optional[SerialIdentity]:
    if list_ports is None:
        return None

    for info in list_ports.comports():
        if info.device == port:
            return SerialIdentity(
                vid=info.vid,
                pid=info.pid,
                serial_number=info.serial_number,
                location=info.location,
            )

    return None


def find_port_by_identity(identity: Optional[SerialIdentity],
                          previous_port: str) -> Optional[str]:
    if identity is None or list_ports is None:
        return previous_port

    fallback = None
    for info in list_ports.comports():
        if identity.vid is not None and info.vid != identity.vid:
            continue
        if identity.pid is not None and info.pid != identity.pid:
            continue
        if identity.serial_number and info.serial_number != identity.serial_number:
            continue
        if identity.location and info.location != identity.location:
            continue

        if info.device == previous_port:
            return info.device
        fallback = info.device

    return fallback


def reopen_serial_after_reenumeration(ser: SerialLike,
                                      config: "HostConfig",
                                      identity: Optional[SerialIdentity],
                                      min_wait_s: float = 0.0) -> SerialLike:
    try:
        ser.close()
    except Exception:
        pass

    deadline = time.monotonic() + max(2.0, config.enter_wait_s + 4.0, min_wait_s)
    last_error: Optional[Exception] = None

    while time.monotonic() < deadline:
        port = find_port_by_identity(identity, config.port)
        if port is None:
            time.sleep(0.1)
            continue

        try:
            if port != config.port:
                print(f"[ENTER] serial adapter re-enumerated: {config.port} -> {port}")
                config.port = port
            return open_serial_port(config)
        except Exception as exc:  # noqa: BLE001 - serial backend exceptions vary
            last_error = exc
            time.sleep(0.1)

    if last_error is not None:
        raise RuntimeError(f"failed to reopen serial port after reboot: {last_error}") from last_error
    raise RuntimeError("failed to find serial adapter after reboot")


def wait_for_application_ready(ser: SerialLike,
                               config: "HostConfig",
                               identity: Optional[SerialIdentity]) -> SerialLike:
    print(f"[VERIFY] waiting {config.verify_app_wait_s:.2f}s for application boot")
    time.sleep(config.verify_app_wait_s)

    ser = reopen_serial_after_reenumeration(
        ser,
        config,
        identity,
        min_wait_s=config.verify_app_wait_s + 4.0,
    )
    ser.reset_input_buffer()

    last_error: Optional[Exception] = None
    for attempt in range(1, config.verify_app_retries + 1):
        try:
            print(f"[VERIFY] pinging application ({attempt}/{config.verify_app_retries})")
            _command, status = send_app_ping(ser, config.node, config.timeout)
            if status != STATUS_OK:
                raise RuntimeError(f"application ping status=0x{status:02X}")
            print("[VERIFY] application ping OK")
            return ser
        except Exception as exc:  # noqa: BLE001 - keep retry context for CLI
            last_error = exc
            time.sleep(config.verify_app_retry_delay_s)

    if last_error is None:
        raise RuntimeError("application verification failed for an unknown reason")
    raise RuntimeError(f"application did not verify after update: {last_error}") from last_error


def round_up(value: int, multiple: int) -> int:
    if multiple <= 0:
        raise ValueError("multiple must be > 0")
    return ((value + multiple - 1) // multiple) * multiple


def action_name(packet_type: int) -> str:
    return {
        UPDATE_TYPE_HELLO: "Hello",
        UPDATE_TYPE_BEGIN: "Begin",
        UPDATE_TYPE_CHUNK: "Chunk",
        UPDATE_TYPE_END: "End",
        UPDATE_TYPE_ABORT: "Abort",
        UPDATE_TYPE_ACK: "Ack",
        UPDATE_TYPE_ERROR: "Error",
    }.get(packet_type, f"Unknown({packet_type})")


def status_name(status: int) -> str:
    return UPDATE_STATUS_NAMES.get(status, f"Unknown({status})")


@dataclass
class HostConfig:
    port: str = "/dev/sensor-system-rs485"
    baud: int = 115200
    node: int = 1
    timeout: float = 4.0
    enter: str = "none"
    enter_wait_s: float = 1.5
    chunk_size: int = DEFAULT_UPDATE_CHUNK_SIZE
    hello_retries: int = 8
    hello_retry_delay_s: float = 0.35
    verify_app: bool = True
    verify_app_wait_s: float = 12.0
    verify_app_retries: int = 10
    verify_app_retry_delay_s: float = 0.5

    @classmethod
    def from_dict(cls, data: dict) -> "HostConfig":
        return cls(
            port=str(data.get("port", cls.port)),
            baud=int(data.get("baud", cls.baud)),
            node=int(data.get("node", cls.node)),
            timeout=float(data.get("timeout", cls.timeout)),
            enter=str(data.get("boot_enter", data.get("enter", cls.enter))),
            enter_wait_s=float(data.get("boot_enter_wait_s", cls.enter_wait_s)),
            chunk_size=int(data.get("boot_chunk_size", cls.chunk_size)),
            hello_retries=int(data.get("boot_hello_retries", cls.hello_retries)),
            hello_retry_delay_s=float(
                data.get("boot_hello_retry_delay_s", cls.hello_retry_delay_s)
            ),
            verify_app=bool(data.get("boot_verify_app", cls.verify_app)),
            verify_app_wait_s=float(
                data.get("boot_verify_app_wait_s", cls.verify_app_wait_s)
            ),
            verify_app_retries=int(
                data.get("boot_verify_app_retries", cls.verify_app_retries)
            ),
            verify_app_retry_delay_s=float(
                data.get(
                    "boot_verify_app_retry_delay_s",
                    cls.verify_app_retry_delay_s,
                )
            ),
        )


@dataclass
class TransportFrame:
    frame_type: int
    flags: int
    destination: int
    source: int
    sequence: int
    payload: bytes


@dataclass
class UpdatePacket:
    packet_type: int
    destination: int
    sequence: int
    payload: bytes


class TransportCodec:
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
    def try_decode(rx_buffer: bytearray) -> Optional[TransportFrame]:
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

        return TransportFrame(
            frame_type=frame_type,
            flags=flags,
            destination=destination,
            source=source,
            sequence=sequence,
            payload=raw_frame[FRAME_HEADER_SIZE:-2],
        )


class UpdateCodec:
    @staticmethod
    def encode(packet_type: int, destination: int, sequence: int, payload: bytes) -> bytes:
        if len(payload) > UPDATE_MAX_PAYLOAD_SIZE:
            raise ValueError(f"update payload too large: {len(payload)} bytes")

        header = struct.pack(
            UPDATE_HEADER_FORMAT,
            UPDATE_PACKET_MAGIC,
            packet_type,
            destination,
            len(payload),
            sequence,
        )
        crc = update_crc32(header + payload)
        return header + payload + struct.pack("<I", crc)

    @staticmethod
    def try_decode(rx_buffer: bytearray) -> Optional[UpdatePacket]:
        magic_bytes = struct.pack("<I", UPDATE_PACKET_MAGIC)

        while len(rx_buffer) >= len(magic_bytes):
            if bytes(rx_buffer[:4]) == magic_bytes:
                break
            del rx_buffer[0]

        if len(rx_buffer) < UPDATE_HEADER_SIZE:
            return None

        header = bytes(rx_buffer[:UPDATE_HEADER_SIZE])
        try:
            magic, packet_type, destination, payload_length, sequence = struct.unpack(
                UPDATE_HEADER_FORMAT,
                header,
            )
        except struct.error:
            return None

        if magic != UPDATE_PACKET_MAGIC:
            del rx_buffer[0]
            return None

        if payload_length > UPDATE_MAX_PAYLOAD_SIZE:
            del rx_buffer[0]
            return None

        total_length = UPDATE_HEADER_SIZE + payload_length + UPDATE_CRC_SIZE
        if len(rx_buffer) < total_length:
            return None

        raw_packet = bytes(rx_buffer[:total_length])
        del rx_buffer[:total_length]

        expected_crc = struct.unpack("<I", raw_packet[-4:])[0]
        calculated_crc = update_crc32(raw_packet[:-4])
        if expected_crc != calculated_crc:
            return None

        return UpdatePacket(
            packet_type=packet_type,
            destination=destination,
            sequence=sequence,
            payload=raw_packet[UPDATE_HEADER_SIZE:-4],
        )


class TransportClient:
    def __init__(self, ser: SerialLike, host_node_id: int = HOST_NODE_ID) -> None:
        self.ser = ser
        self.host_node_id = host_node_id
        self.rx_buffer = bytearray()
        self.sequence = 1

    def next_sequence(self) -> int:
        current = self.sequence
        self.sequence = 1 if self.sequence == 0xFFFFFFFF else self.sequence + 1
        return current

    def send_command(self, node_id: int, command: int, payload: bytes = b"") -> int:
        sequence = self.next_sequence()
        frame_payload = bytes([command]) + payload
        frame = TransportCodec.encode(
            frame_type=FRAME_TYPE_COMMAND,
            destination=node_id,
            source=self.host_node_id,
            sequence=sequence,
            payload=frame_payload,
        )
        self.ser.reset_input_buffer()
        self.ser.write(frame)
        self.ser.flush()
        return sequence

    def wait_for_response(self, node_id: int, sequence: int, timeout_s: float) -> Optional[TransportFrame]:
        deadline = time.monotonic() + timeout_s

        while time.monotonic() < deadline:
            chunk = self.ser.read(256)
            if chunk:
                self.rx_buffer.extend(chunk)

            while True:
                buffer_size_before = len(self.rx_buffer)
                frame = TransportCodec.try_decode(self.rx_buffer)
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


class UpdateClient:
    def __init__(self, ser: SerialLike) -> None:
        self.ser = ser
        self.rx_buffer = bytearray()
        self.sequence = 1

    def next_sequence(self) -> int:
        current = self.sequence
        self.sequence = 1 if self.sequence == 0xFFFFFFFF else self.sequence + 1
        return current

    def send_packet(self, packet_type: int, destination: int, payload: bytes = b"") -> int:
        sequence = self.next_sequence()
        packet = UpdateCodec.encode(packet_type, destination, sequence, payload)
        self.ser.write(packet)
        self.ser.flush()
        return sequence

    def wait_for_reply(self, destination: int, sequence: int, timeout_s: float) -> Optional[UpdatePacket]:
        deadline = time.monotonic() + timeout_s

        while time.monotonic() < deadline:
            chunk = self.ser.read(256)
            if chunk:
                self.rx_buffer.extend(chunk)

            while True:
                buffer_size_before = len(self.rx_buffer)
                packet = UpdateCodec.try_decode(self.rx_buffer)
                if packet is None:
                    if len(self.rx_buffer) == buffer_size_before:
                        break
                    continue

                if packet.destination != destination:
                    continue

                if packet.packet_type not in (UPDATE_TYPE_ACK, UPDATE_TYPE_ERROR):
                    continue

                if packet.sequence == sequence:
                    return packet

        return None


def decode_ack_payload(payload: bytes) -> tuple[int, int, int, int]:
    if len(payload) != 8:
        raise ValueError(f"invalid ACK payload length: expected 8, got {len(payload)}")
    status, reserved0, reserved1, value = struct.unpack("<BBHI", payload)
    return status, reserved0, reserved1, value


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Bootloader host tool for sensor-system")
    parser.add_argument(
        "action",
        choices=["probe", "hello", "abort", "upload"],
        help="Bootloader action",
    )
    parser.add_argument("--config", default="host_config.json", help="Path to JSON config file")
    parser.add_argument("--port", help="Serial port, e.g. /dev/sensor-system-rs485")
    parser.add_argument("--baud", type=int, help="Serial baud rate")
    parser.add_argument("--node", type=int, help="Destination node id")
    parser.add_argument("--timeout", type=float, help="Per-request timeout in seconds")
    parser.add_argument(
        "--enter",
        choices=["auto", "none", "app", "maintenance"],
        help="How to enter bootloader before the selected action",
    )
    parser.add_argument("--enter-wait", type=float, help="Wait time after enter request, in seconds")
    parser.add_argument("--image", help="Path to firmware binary for upload")
    parser.add_argument("--version", type=lambda raw: int(raw, 0), default=0, help="Image version")
    parser.add_argument("--chunk-size", type=int, help="Chunk payload size before page padding")
    parser.add_argument("--hello-retries", type=int, help="How many hello retries to do after enter")
    parser.add_argument("--hello-retry-delay", type=float, help="Delay between hello retries, in seconds")
    parser.add_argument(
        "--no-verify-app",
        action="store_true",
        help="Do not ping the application after a successful upload",
    )
    parser.add_argument(
        "--verify-app-wait",
        type=float,
        help="Seconds to wait for the updated application to boot before ping verification",
    )
    parser.add_argument(
        "--verify-app-retries",
        type=int,
        help="How many post-update application ping attempts to do",
    )
    parser.add_argument(
        "--verify-app-retry-delay",
        type=float,
        help="Delay between post-update application ping attempts, in seconds",
    )
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
        enter=args.enter if args.enter is not None else file_config.enter,
        enter_wait_s=args.enter_wait if args.enter_wait is not None else file_config.enter_wait_s,
        chunk_size=args.chunk_size if args.chunk_size is not None else file_config.chunk_size,
        hello_retries=args.hello_retries if args.hello_retries is not None else file_config.hello_retries,
        hello_retry_delay_s=(
            args.hello_retry_delay
            if args.hello_retry_delay is not None
            else file_config.hello_retry_delay_s
        ),
        verify_app=False if args.no_verify_app else file_config.verify_app,
        verify_app_wait_s=(
            args.verify_app_wait
            if args.verify_app_wait is not None
            else file_config.verify_app_wait_s
        ),
        verify_app_retries=(
            args.verify_app_retries
            if args.verify_app_retries is not None
            else file_config.verify_app_retries
        ),
        verify_app_retry_delay_s=(
            args.verify_app_retry_delay
            if args.verify_app_retry_delay is not None
            else file_config.verify_app_retry_delay_s
        ),
    )


def validate_config(config: HostConfig, action: str, image_path: Optional[str]) -> Optional[str]:
    if config.node <= 0 or config.node >= 0xFF:
        return f"invalid node id: {config.node}"

    if config.timeout <= 0:
        return f"timeout must be > 0, got {config.timeout}"

    if config.enter_wait_s < 0:
        return f"enter-wait must be >= 0, got {config.enter_wait_s}"

    if config.hello_retries <= 0:
        return f"hello-retries must be > 0, got {config.hello_retries}"

    if config.hello_retry_delay_s < 0:
        return f"hello-retry-delay must be >= 0, got {config.hello_retry_delay_s}"

    if config.verify_app_wait_s < 0:
        return f"verify-app-wait must be >= 0, got {config.verify_app_wait_s}"

    if config.verify_app_retries <= 0:
        return f"verify-app-retries must be > 0, got {config.verify_app_retries}"

    if config.verify_app_retry_delay_s < 0:
        return (
            "verify-app-retry-delay must be >= 0, "
            f"got {config.verify_app_retry_delay_s}"
        )

    if config.chunk_size <= 0 or config.chunk_size > DEFAULT_UPDATE_CHUNK_SIZE:
        return (
            f"chunk-size must be in range 1..{DEFAULT_UPDATE_CHUNK_SIZE}, "
            f"got {config.chunk_size}"
        )

    if action == "upload" and not image_path:
        return "--image is required for upload"

    return None


def enter_bootloader_via_app(ser: SerialLike, node_id: int, timeout_s: float) -> None:
    print("[ENTER] requesting bootloader through application command 0x03")
    client = TransportClient(ser)
    sequence = client.send_command(node_id, ENTER_BOOTLOADER_COMMAND)
    response = client.wait_for_response(node_id, sequence, timeout_s)

    if response is None:
        raise RuntimeError("no response to EnterBootloader command")

    if len(response.payload) < 2:
        raise RuntimeError("EnterBootloader response payload too short")

    command = response.payload[0]
    status = response.payload[1]

    if command != ENTER_BOOTLOADER_COMMAND:
        raise RuntimeError(
            f"EnterBootloader response command mismatch: expected 0x{ENTER_BOOTLOADER_COMMAND:02X}, "
            f"got 0x{command:02X}"
        )

    if status != STATUS_OK:
        raise RuntimeError(f"EnterBootloader rejected by app with status=0x{status:02X}")

    print(f"[ENTER] app acknowledged reboot request, seq={sequence}")


def send_app_ping(ser: SerialLike, node_id: int, timeout_s: float) -> tuple[int, int]:
    client = TransportClient(ser)
    sequence = client.send_command(node_id, PING_COMMAND, b"probe")
    response = client.wait_for_response(node_id, sequence, timeout_s)

    if response is None:
        raise RuntimeError("no response to application Ping command")

    if len(response.payload) < 2:
        raise RuntimeError("Ping response payload too short")

    command = response.payload[0]
    status = response.payload[1]

    if command != PING_COMMAND:
        raise RuntimeError(
            f"Ping response command mismatch: expected 0x{PING_COMMAND:02X}, "
            f"got 0x{command:02X}"
        )

    return command, status


def enter_bootloader_via_maintenance(ser: SerialLike) -> None:
    print("[ENTER] sending maintenance command: UPDATE")
    ser.reset_input_buffer()
    ser.write(b"UPDATE\r\n")
    ser.flush()


def expect_update_reply(
    client: UpdateClient,
    node_id: int,
    sequence: int,
    timeout_s: float,
) -> tuple[int, int, int, int, int]:
    reply = client.wait_for_reply(node_id, sequence, timeout_s)
    if reply is None:
        raise RuntimeError(f"timeout waiting for bootloader reply to seq={sequence}")

    try:
        status, reserved0, reserved1, value = decode_ack_payload(reply.payload)
    except ValueError as exc:
        raise RuntimeError(
            f"invalid {action_name(reply.packet_type)} payload for seq={sequence}: {exc}"
        ) from exc

    return reply.packet_type, status, reserved0, reserved1, value


def send_hello(client: UpdateClient, node_id: int, timeout_s: float) -> tuple[int, int, int, int]:
    payload = struct.pack("<HH", UPDATE_PROTOCOL_VERSION, DEFAULT_UPDATE_CHUNK_SIZE)
    sequence = client.send_packet(UPDATE_TYPE_HELLO, node_id, payload)
    packet_type, status, target_slot, _reserved1, value = expect_update_reply(client, node_id, sequence, timeout_s)

    if packet_type != UPDATE_TYPE_ACK:
        raise RuntimeError(
            f"hello rejected: status={status_name(status)}({status}) value=0x{value:08X}"
        )

    if status != 0:
        raise RuntimeError(
            f"hello failed: status={status_name(status)}({status}) value=0x{value:08X}"
        )

    packed_node = (value >> 24) & 0xFF
    packed_protocol = (value >> 16) & 0xFF
    packed_chunk = value & 0xFFFF
    slot_name = {SLOT_A: "A", SLOT_B: "B"}.get(target_slot, f"?({target_slot})")
    print(
        "[HELLO] "
        f"node={packed_node} protocol={packed_protocol} max_chunk={packed_chunk} target_slot={slot_name}"
    )
    return packed_node, packed_protocol, packed_chunk, target_slot


def wait_until_bootloader_ready(client: UpdateClient, config: HostConfig) -> tuple[int, int, int, int]:
    last_error: Optional[Exception] = None

    for attempt in range(1, config.hello_retries + 1):
        try:
            print(f"[HELLO] probing bootloader ({attempt}/{config.hello_retries})")
            return send_hello(client, config.node, config.timeout)
        except Exception as exc:  # noqa: BLE001 - retain last failure context
            last_error = exc
            time.sleep(config.hello_retry_delay_s)

    if last_error is None:
        raise RuntimeError("bootloader probe failed for an unknown reason")
    raise RuntimeError(f"bootloader did not answer hello: {last_error}") from last_error


def upload_image(
    client: UpdateClient,
    node_id: int,
    image_path: Path,
    version: int,
    timeout_s: float,
    chunk_size: int,
) -> None:
    image = image_path.read_bytes()
    image_size = len(image)
    image_crc32 = update_crc32(image)

    print(
        f"[UPLOAD] image={image_path} size={image_size} version=0x{version:08X} "
        f"crc32=0x{image_crc32:08X}"
    )

    begin_payload = struct.pack("<III", image_size, image_crc32, version & 0xFFFFFFFF)
    sequence = client.send_packet(UPDATE_TYPE_BEGIN, node_id, begin_payload)
    packet_type, status, _reserved0, _reserved1, value = expect_update_reply(client, node_id, sequence, timeout_s)

    if packet_type != UPDATE_TYPE_ACK or status != 0:
        raise RuntimeError(
            f"begin failed: status={status_name(status)}({status}) value=0x{value:08X}"
        )

    print(f"[BEGIN] target_slot={value}")

    image_offset = 0
    flash_offset = 0
    chunk_index = 0

    while image_offset < image_size:
        chunk_index += 1
        chunk = image[image_offset:image_offset + chunk_size]
        valid_length = len(chunk)
        flash_length = round_up(valid_length, FLASH_PAGE_SIZE)
        padded_chunk = chunk + (b"\xFF" * (flash_length - valid_length))

        payload = struct.pack("<IHH", flash_offset, flash_length, valid_length) + padded_chunk
        sequence = client.send_packet(UPDATE_TYPE_CHUNK, node_id, payload)
        packet_type, status, _reserved0, _reserved1, value = expect_update_reply(client, node_id, sequence, timeout_s)

        if packet_type != UPDATE_TYPE_ACK or status != 0:
            raise RuntimeError(
                "chunk failed: "
                f"index={chunk_index} flash_offset={flash_offset} valid_length={valid_length} "
                f"status={status_name(status)}({status}) value=0x{value:08X}"
            )

        image_offset += valid_length
        flash_offset += flash_length
        progress = (image_offset / image_size) * 100.0 if image_size else 100.0
        print(
            f"[CHUNK] #{chunk_index} valid={valid_length} flash={flash_length} "
            f"written={image_offset}/{image_size} ({progress:.1f}%) ack_value={value}"
        )

    end_payload = struct.pack("<I", image_crc32)
    sequence = client.send_packet(UPDATE_TYPE_END, node_id, end_payload)
    packet_type, status, _reserved0, _reserved1, value = expect_update_reply(client, node_id, sequence, timeout_s)

    if packet_type != UPDATE_TYPE_ACK or status != 0:
        raise RuntimeError(
            f"end failed: status={status_name(status)}({status}) value=0x{value:08X}"
        )

    print(f"[END] update accepted, target_slot={value}")


def abort_update(client: UpdateClient, node_id: int, timeout_s: float) -> None:
    sequence = client.send_packet(UPDATE_TYPE_ABORT, node_id)
    packet_type, status, _reserved0, _reserved1, value = expect_update_reply(client, node_id, sequence, timeout_s)

    if packet_type != UPDATE_TYPE_ACK or status != 0:
        raise RuntimeError(
            f"abort failed: status={status_name(status)}({status}) value=0x{value:08X}"
        )

    print("[ABORT] bootloader acknowledged abort")


def maybe_enter_bootloader(ser: SerialLike,
                           config: HostConfig,
                           identity: Optional[SerialIdentity]) -> SerialLike:
    if config.enter == "none":
        return ser

    entered_via_app = False

    if config.enter == "auto":
        print("[ENTER] auto-detecting node mode")
        try:
            send_hello(UpdateClient(ser), config.node, min(config.timeout, 2.0))
            print("[ENTER] bootloader is already active")
            ser.reset_input_buffer()
            return ser
        except Exception as boot_exc:  # noqa: BLE001 - auto mode falls back to app
            print(f"[ENTER] bootloader probe did not answer: {boot_exc}")
            ser.reset_input_buffer()

        try:
            enter_bootloader_via_app(ser, config.node, config.timeout)
            entered_via_app = True
        except Exception as app_exc:  # noqa: BLE001 - interrupted update may already be in bootloader
            print(f"[ENTER] app enter failed, checking bootloader recovery: {app_exc}")
            ser.reset_input_buffer()
            try:
                send_hello(UpdateClient(ser), config.node, min(config.timeout, 2.0))
                print("[ENTER] bootloader recovery mode detected")
                ser.reset_input_buffer()
                return ser
            except Exception as boot_exc:  # noqa: BLE001 - report both paths
                raise RuntimeError(
                    f"auto enter failed: app={app_exc}; bootloader={boot_exc}"
                ) from app_exc

    elif config.enter == "app":
        enter_bootloader_via_app(ser, config.node, config.timeout)
        entered_via_app = True
    elif config.enter == "maintenance":
        enter_bootloader_via_maintenance(ser)
    else:
        raise RuntimeError(f"unsupported enter method: {config.enter}")

    if config.enter_wait_s > 0:
        print(f"[ENTER] waiting {config.enter_wait_s:.2f}s for reboot/transition")
        time.sleep(config.enter_wait_s)

    if entered_via_app:
        ser = reopen_serial_after_reenumeration(ser, config, identity)

    ser.reset_input_buffer()
    return ser


def probe_device(ser: SerialLike, config: HostConfig) -> None:
    print("[PROBE] checking bootloader update protocol")
    bootloader_ok = False
    try:
        client = UpdateClient(ser)
        send_hello(client, config.node, config.timeout)
        bootloader_ok = True
        print("[PROBE] bootloader responded")
    except Exception as exc:  # noqa: BLE001 - probe should report all paths
        print(f"[PROBE] bootloader did not respond: {exc}")

    print("[PROBE] checking application transport protocol")
    app_ok = False
    try:
        _command, status = send_app_ping(ser, config.node, config.timeout)
        app_ok = status == STATUS_OK
        print(f"[PROBE] application responded: status=0x{status:02X}")
    except Exception as exc:  # noqa: BLE001 - probe should report all paths
        print(f"[PROBE] application did not respond: {exc}")

    if bootloader_ok or app_ok:
        return

    print(
        "[PROBE] no protocol reply. Check that the host is connected to UART0/RS485 "
        "GPIO0=TX, GPIO1=RX, GPIO2=DE at 115200 8N1; Pico USB CDC is only stdio/logs."
    )


@dataclass
class ResolvedImage:
    path: Path
    version: int


def resolve_update_image(image_path: Path, target_slot: int, version_override: int) -> ResolvedImage:
    if image_path.suffix.lower() != ".json":
        return ResolvedImage(path=image_path, version=version_override)

    package = json.loads(image_path.read_text(encoding="utf-8"))
    if package.get("format") != "sensor_system_node_update_package":
        raise RuntimeError(f"unsupported update package format: {package.get('format')}")

    slot_key = "slot_a" if target_slot == SLOT_A else "slot_b" if target_slot == SLOT_B else None
    if slot_key is None or slot_key not in package:
        raise RuntimeError(f"update package does not contain target slot {target_slot}")

    slot_entry = package[slot_key]
    resolved_path = (image_path.parent / slot_entry["path"]).resolve()
    if not resolved_path.exists():
        raise RuntimeError(f"update image not found: {resolved_path}")

    version = version_override if version_override != 0 else int(package.get("version", 0))
    return ResolvedImage(path=resolved_path, version=version)


def main() -> int:
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(line_buffering=True)

    args = build_parser().parse_args()
    config = resolve_config(args)

    validation_error = validate_config(config, args.action, args.image)
    if validation_error is not None:
        print(f"[ERROR] {validation_error}", file=sys.stderr)
        return 2

    image_path = Path(args.image).expanduser() if args.image else None
    if image_path is not None and not image_path.exists():
        print(f"[ERROR] image file does not exist: {image_path}", file=sys.stderr)
        return 2

    if serial is None:
        print(
            "[ERROR] missing dependency: pyserial. Install with: "
            "`python -m pip install pyserial`",
            file=sys.stderr,
        )
        return 2

    identity = serial_identity_for_port(config.port)

    try:
        ser = open_serial_port(config)
    except Exception as exc:  # noqa: BLE001 - serial backend may raise varied exceptions
        print(f"[ERROR] cannot open serial port {config.port}: {exc}", file=sys.stderr)
        return 2

    print(
        f"[HOST] action={args.action} port={config.port} baud={config.baud} "
        f"node={config.node} timeout={config.timeout:.2f}s enter={config.enter}"
    )

    try:
        ser = maybe_enter_bootloader(ser, config, identity)

        update_client = UpdateClient(ser)

        if args.action == "probe":
            probe_device(ser, config)
            return 0

        if args.action == "hello":
            if config.enter == "none":
                send_hello(update_client, config.node, config.timeout)
            else:
                wait_until_bootloader_ready(update_client, config)
            print("[OK] bootloader hello succeeded")
            return 0

        if args.action == "abort":
            if config.enter == "none":
                send_hello(update_client, config.node, config.timeout)
            else:
                wait_until_bootloader_ready(update_client, config)
            abort_update(update_client, config.node, config.timeout)
            print("[OK] bootloader abort succeeded")
            return 0

        if args.action == "upload":
            if image_path is None:
                print("[ERROR] --image is required for upload", file=sys.stderr)
                return 2

            packed_node, packed_protocol, packed_chunk, target_slot = (
                send_hello(update_client, config.node, config.timeout)
                if config.enter == "none"
                else wait_until_bootloader_ready(update_client, config)
            )

            if packed_node != config.node:
                print(
                    f"[WARN] hello replied from node={packed_node}, expected node={config.node}",
                    file=sys.stderr,
                )

            if packed_protocol != UPDATE_PROTOCOL_VERSION:
                print(
                    f"[WARN] protocol mismatch: host={UPDATE_PROTOCOL_VERSION}, device={packed_protocol}",
                    file=sys.stderr,
                )

            resolved_image = resolve_update_image(image_path, target_slot, args.version)
            effective_chunk_size = min(config.chunk_size, packed_chunk)
            upload_image(
                update_client,
                config.node,
                resolved_image.path,
                resolved_image.version,
                config.timeout,
                effective_chunk_size,
            )
            print("[OK] bootloader upload protocol completed")
            if config.verify_app:
                ser = wait_for_application_ready(ser, config, identity)
                print("[OK] post-update application verification completed")
            return 0

        print(f"[ERROR] unsupported action: {args.action}", file=sys.stderr)
        return 2

    except Exception as exc:  # noqa: BLE001 - CLI should print user-facing error details
        print(f"[ERROR] {exc}", file=sys.stderr)
        return 1
    finally:
        ser.close()


if __name__ == "__main__":
    raise SystemExit(main())
