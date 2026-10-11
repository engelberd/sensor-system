#!/usr/bin/env python3
"""Generate protocol-5 Measurement payloads from the normative byte tables."""

from pathlib import Path
import struct

ROOT = Path(__file__).resolve().parent
EPOCH = 0x1122334455667788


def state_fields(retained: int = 10, produced: int = 100, released: int = 8) -> bytes:
    return struct.pack("<QQQQIQ", EPOCH, retained, produced, released, 4096, 10)


def i24(value: int) -> bytes:
    assert -(1 << 23) <= value < (1 << 23)
    return (value & 0xFFFFFF).to_bytes(3, "little")


pull_request = struct.pack("<QQQH", EPOCH, 20, 16, 48)
release_request = struct.pack("<QQ", EPOCH, 16)
stream_state = bytes([0]) + state_fields()

chunk = (
    bytes([0, 0])
    + struct.pack("<QQQQHIIQQIH", EPOCH, 10, 100, 20, 2, 3, 4, 1000, 5000, 2, 1)
    + i24(-(1 << 23)) + i24(-1) + i24(0)
    + i24(1) + i24(0x123456) + i24((1 << 23) - 1)
)

vectors = {
    "measurement_v5_pull_request.bin": pull_request,
    "measurement_v5_release_request.bin": release_request,
    "measurement_v5_stream_state_response.bin": stream_state,
    "measurement_v5_chunk_response.bin": chunk,
    "measurement_v5_no_data_response.bin": bytes([0, 1]) + state_fields(),
    "measurement_v5_range_unavailable_response.bin": bytes([0, 2]) + state_fields(),
    "measurement_v5_stream_changed_response.bin": bytes([0, 3]) + struct.pack("<Q", 9),
    "measurement_v5_invalid_release_response.bin": bytes([0, 4]) + state_fields(),
    "measurement_v5_gap_response.bin": (
        bytes([0, 5]) + struct.pack("<QQQQQB", EPOCH, 10, 100, 20, 3, 1)
    ),
    "measurement_v5_continuity_break_response.bin": (
        bytes([0, 6]) + struct.pack("<QQQQB", EPOCH, 10, 100, 20, 2)
    ),
}

for name, payload in vectors.items():
    (ROOT / name).write_bytes(payload)
