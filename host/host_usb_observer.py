#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import struct
import sys
import time
from dataclasses import asdict, dataclass
from typing import Any, Optional

import serial

try:
    from host.host_ping import (
        FRAME_TYPE_COMMAND,
        FRAME_TYPE_RESPONSE,
        HOST_NODE_ID,
        Frame,
        FrameCodec,
    )
except ModuleNotFoundError:  # Direct execution from the host directory.
    from host_ping import (  # type: ignore[no-redef]
        FRAME_TYPE_COMMAND,
        FRAME_TYPE_RESPONSE,
        HOST_NODE_ID,
        Frame,
        FrameCodec,
    )


CMD_GET_CAPABILITIES = 0x01
CMD_READ_SNAPSHOT = 0x02
CMD_GET_STATS = 0x03
STATUS_OK = 0
OBSERVER_FORMAT_VERSION = 1

CAPABILITIES_FORMAT = "<BBBBBBBBBBBBHH8s"
SNAPSHOT_HEADER_FORMAT = "<BBBBIHHHHII"
SAMPLE_FORMAT = "<QiiiQIHBB"
STATS_FORMAT = "<BBBBIIIH"


@dataclass(frozen=True)
class ObserverCapabilities:
    format_version: int
    product_protocol_version: int
    board_revision: int
    timestamping_enabled: bool
    node_id: int
    firmware: str
    snapshot_capacity: int
    max_wire_samples: int
    hardware_id: str


@dataclass(frozen=True)
class ObserverSample:
    sample_seq: int
    x: int
    y: int
    z: int
    device_time_us: int
    timing_segment_id: int
    quality_flags: int
    timestamp_source: int


def _require_ok(payload: bytes, command: int) -> None:
    if len(payload) < 2:
        raise ValueError("observer response is shorter than command/status")
    if payload[0] != command:
        raise ValueError(
            f"observer command mismatch: expected 0x{command:02x}, got 0x{payload[0]:02x}"
        )
    if payload[1] != STATUS_OK:
        raise ValueError(f"observer returned status {payload[1]}")


def parse_capabilities(payload: bytes) -> ObserverCapabilities:
    _require_ok(payload, CMD_GET_CAPABILITIES)
    if len(payload) != struct.calcsize(CAPABILITIES_FORMAT):
        raise ValueError(f"invalid capabilities size: {len(payload)}")

    values = struct.unpack(CAPABILITIES_FORMAT, payload)
    if values[2] != OBSERVER_FORMAT_VERSION:
        raise ValueError(f"unsupported observer format: {values[2]}")

    return ObserverCapabilities(
        format_version=values[2],
        product_protocol_version=values[3],
        board_revision=values[4],
        timestamping_enabled=bool(values[5]),
        node_id=values[6],
        firmware=f"{values[8]}.{values[9]}.{values[10]}",
        snapshot_capacity=values[12],
        max_wire_samples=values[13],
        hardware_id=values[14].hex(),
    )


def parse_snapshot(payload: bytes) -> dict[str, Any]:
    _require_ok(payload, CMD_READ_SNAPSHOT)
    header_size = struct.calcsize(SNAPSHOT_HEADER_FORMAT)
    if len(payload) < header_size:
        raise ValueError(f"invalid snapshot size: {len(payload)}")

    header = struct.unpack(SNAPSHOT_HEADER_FORMAT, payload[:header_size])
    _, _, version, flags, generation, source_count, returned_count, first_index, entry_size, published, truncated = header
    if version != OBSERVER_FORMAT_VERSION:
        raise ValueError(f"unsupported observer format: {version}")
    if entry_size != struct.calcsize(SAMPLE_FORMAT):
        raise ValueError(f"unsupported sample entry size: {entry_size}")
    if len(payload) != header_size + returned_count * entry_size:
        raise ValueError("snapshot payload length does not match returned sample count")

    samples = []
    offset = header_size
    for _ in range(returned_count):
        values = struct.unpack(SAMPLE_FORMAT, payload[offset:offset + entry_size])
        samples.append(
            asdict(
                ObserverSample(
                    sample_seq=values[0],
                    x=values[1],
                    y=values[2],
                    z=values[3],
                    device_time_us=values[4],
                    timing_segment_id=values[5],
                    quality_flags=values[6],
                    timestamp_source=values[7],
                )
            )
        )
        offset += entry_size

    return {
        "format_version": version,
        "truncated_for_wire": bool(flags & 1),
        "generation": generation,
        "source_sample_count": source_count,
        "returned_sample_count": returned_count,
        "first_source_index": first_index,
        "published_batches": published,
        "truncated_batches": truncated,
        "samples": samples,
    }


def parse_stats(payload: bytes) -> dict[str, int]:
    _require_ok(payload, CMD_GET_STATS)
    if len(payload) != struct.calcsize(STATS_FORMAT):
        raise ValueError(f"invalid observer stats size: {len(payload)}")
    _, _, version, _, published, truncated, retries, capacity = struct.unpack(
        STATS_FORMAT, payload
    )
    if version != OBSERVER_FORMAT_VERSION:
        raise ValueError(f"unsupported observer format: {version}")
    return {
        "format_version": version,
        "published_batches": published,
        "truncated_batches": truncated,
        "read_retries": retries,
        "snapshot_capacity": capacity,
    }


class UsbObserverClient:
    def __init__(self, connection: serial.Serial) -> None:
        self.connection = connection
        self.rx_buffer = bytearray()
        self.sequence = 1

    def request(self, command: int, timeout_s: float) -> Frame:
        sequence = self.sequence
        self.sequence = 1 if sequence == 0xFFFFFFFF else sequence + 1
        request = FrameCodec.encode(
            FRAME_TYPE_COMMAND,
            0,
            HOST_NODE_ID,
            sequence,
            bytes([command]),
        )
        self.connection.reset_input_buffer()
        self.connection.write(request)
        self.connection.flush()

        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            chunk = self.connection.read(256)
            if chunk:
                self.rx_buffer.extend(chunk)
            frame = FrameCodec.try_decode(self.rx_buffer)
            if (
                frame is not None
                and frame.frame_type == FRAME_TYPE_RESPONSE
                and frame.destination == HOST_NODE_ID
                and frame.sequence == sequence
            ):
                return frame
        raise TimeoutError(f"no USB observer response within {timeout_s:.2f}s")


def main() -> int:
    parser = argparse.ArgumentParser(description="Read the node's passive USB observer")
    parser.add_argument("--port", required=True, help="Pico USB CDC port")
    parser.add_argument("--timeout", type=float, default=2.0)
    parser.add_argument(
        "command", choices=("capabilities", "snapshot", "stats"), nargs="?", default="snapshot"
    )
    args = parser.parse_args()

    command = {
        "capabilities": CMD_GET_CAPABILITIES,
        "snapshot": CMD_READ_SNAPSHOT,
        "stats": CMD_GET_STATS,
    }[args.command]

    try:
        connection = serial.Serial(
            args.port, baudrate=115200, timeout=0.05, write_timeout=0.5
        )
        try:
            payload = UsbObserverClient(connection).request(command, args.timeout).payload
        finally:
            connection.close()

        if command == CMD_GET_CAPABILITIES:
            result: Any = asdict(parse_capabilities(payload))
        elif command == CMD_READ_SNAPSHOT:
            result = parse_snapshot(payload)
        else:
            result = parse_stats(payload)
    except (OSError, serial.SerialException, TimeoutError, ValueError) as exc:
        print(f"[ERROR] {exc}", file=sys.stderr)
        return 1

    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
