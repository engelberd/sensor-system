#!/usr/bin/env python3
"""Generate the application-protocol-5 Time response from its wire spec."""

from pathlib import Path


ROOT = Path(__file__).resolve().parent

response = bytearray((0,))
response += (0x0102030405060708).to_bytes(8, "little")
response += (0x1112131415161718).to_bytes(8, "little")
response += (0x2122232425262728).to_bytes(8, "little")
assert len(response) == 25

(ROOT / "time_v5_observe_response.bin").write_bytes(response)
