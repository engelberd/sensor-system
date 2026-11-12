#!/usr/bin/env python3
"""Generate protocol-v3 golden vectors directly from the normative byte layout."""

from pathlib import Path


def crc32c(data: bytes) -> int:
    crc = 0xFFFFFFFF
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = (crc >> 1) ^ (0x82F63B78 if crc & 1 else 0)
    return (~crc) & 0xFFFFFFFF


def cobs(data: bytes) -> bytes:
    output = bytearray([0])
    code_index = 0
    code = 1
    for byte in data:
        if byte == 0:
            output[code_index] = code
            code_index = len(output)
            output.append(0)
            code = 1
        else:
            output.append(byte)
            code += 1
            if code == 0xFF:
                output[code_index] = code
                code_index = len(output)
                output.append(0)
                code = 1
    output[code_index] = code
    return bytes(output)


def frame(role: int, destination: int, source: int, service: int,
          message: int, transaction_id: int, payload: bytes) -> bytes:
    header = bytes((3, role, destination, source, service, message))
    header += transaction_id.to_bytes(4, "little")
    raw = header + payload
    raw += crc32c(raw).to_bytes(4, "little")
    return cobs(raw) + b"\x00"


ROOT = Path(__file__).resolve().parent
VECTORS = {
    "framing_v3_request_empty.bin": frame(1, 7, 0, 2, 3, 0x12345678, b""),
    "framing_v3_response_mixed.bin": frame(
        2, 0, 7, 2, 4, 0x89ABCDEF, bytes((0, 1, 0, 255, 16))
    ),
    "framing_v3_request_max.bin": frame(
        1, 7, 0, 2, 3, 0x12345678, bytes(i & 0xFF for i in range(512))
    ),
}

for name, contents in VECTORS.items():
    (ROOT / name).write_bytes(contents)
