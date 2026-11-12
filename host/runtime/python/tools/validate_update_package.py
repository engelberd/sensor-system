#!/usr/bin/env python3
"""Validate immutable identity fields of one node A/B update package."""

from __future__ import annotations

import json
from pathlib import Path
import sys


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: validate_update_package.py PACKAGE.json", file=sys.stderr)
        return 64
    try:
        package = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
        if package.get("format") != "sensor_system_node_update_package":
            raise ValueError("unsupported update package format")
        if package.get("schema_version") != 1:
            raise ValueError("unsupported update package schema")
        packed = package["version"]
        board_revision = package["board_revision"]
        if not isinstance(packed, int) or not 0 < packed <= 0xFFFFFF:
            raise ValueError("invalid packed firmware version")
        if board_revision not in (1, 2):
            raise ValueError("invalid board revision")
        for slot in ("slot_a", "slot_b"):
            entry = package.get(slot)
            if not isinstance(entry, dict) or not isinstance(entry.get("path"), str):
                raise ValueError(f"missing {slot} image")
        firmware = f"{(packed >> 16) & 0xFF}.{(packed >> 8) & 0xFF}.{packed & 0xFF}"
        print(f"{firmware} {board_revision}")
        return 0
    except (OSError, KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
        print(f"invalid update package: {error}", file=sys.stderr)
        return 65


if __name__ == "__main__":
    raise SystemExit(main())
