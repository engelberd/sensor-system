#!/usr/bin/env python3
"""Generate application-protocol-5 identity payloads from the wire specification."""

from pathlib import Path


ROOT = Path(__file__).resolve().parent

identity = bytearray((0,))
identity += bytes.fromhex("2F13349450FA4084")
identity += (0x1122334455667788).to_bytes(8, "little")
identity += bytes((7, 2, 0, 5, 0, 3, 5))
assert len(identity) == 24

service_flags = (1 << 0) | (1 << 1) | (1 << 2)
feature_flags = (1 << 0) | (1 << 1)
capabilities = bytearray((0, 1))
capabilities += service_flags.to_bytes(4, "little")
capabilities += feature_flags.to_bytes(8, "little")
capabilities += (512).to_bytes(2, "little")
capabilities += (4096).to_bytes(4, "little")
capabilities += (0x07FF).to_bytes(2, "little")
capabilities += (48).to_bytes(2, "little")
assert len(capabilities) == 24

(ROOT / "identity_v5_response.bin").write_bytes(identity)
(ROOT / "capabilities_v5_response.bin").write_bytes(capabilities)
