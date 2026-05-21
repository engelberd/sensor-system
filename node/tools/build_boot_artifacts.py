#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import re
import struct
from dataclasses import dataclass
from pathlib import Path


BOOT_METADATA_MAGIC = 0x424F4F54
BOOT_METADATA_VERSION = 2
SLOT_NONE = 0
SLOT_A = 1
SLOT_B = 2


def crc32(data: bytes) -> int:
    crc = 0xFFFFFFFF
    for byte in data:
        crc ^= byte
        for _ in range(8):
            if crc & 1:
                crc = (crc >> 1) ^ 0xEDB88320
            else:
                crc >>= 1
    return (~crc) & 0xFFFFFFFF


def parse_version_header(path: Path) -> int:
    text = path.read_text(encoding="utf-8")
    major = int(re.search(r"FW_VERSION_MAJOR\s*=\s*(\d+)", text).group(1))
    minor = int(re.search(r"FW_VERSION_MINOR\s*=\s*(\d+)", text).group(1))
    patch = int(re.search(r"FW_VERSION_PATCH\s*=\s*(\d+)", text).group(1))
    return ((major & 0xFF) << 16) | ((minor & 0xFF) << 8) | (patch & 0xFF)


def pack_slot_metadata(
    *,
    image_size: int,
    image_crc32: int,
    image_version: int,
    confirmed_boots: int,
    failed_trial_boots: int,
    image_valid: int,
) -> bytes:
    return struct.pack(
        "<IIIII BBH",
        image_size,
        image_crc32,
        image_version,
        confirmed_boots,
        failed_trial_boots,
        image_valid,
        0,
        0,
    )


def build_boot_metadata(
    *,
    node_id: int,
    slot_a_size: int,
    slot_a_crc32: int,
    slot_a_version: int,
) -> bytes:
    slot_a = pack_slot_metadata(
        image_size=slot_a_size,
        image_crc32=slot_a_crc32,
        image_version=slot_a_version,
        confirmed_boots=1,
        failed_trial_boots=0,
        image_valid=1,
    )
    slot_b = pack_slot_metadata(
        image_size=0,
        image_crc32=0,
        image_version=0,
        confirmed_boots=0,
        failed_trial_boots=0,
        image_valid=0,
    )

    prefix = struct.pack(
        "<IHHIBBBBBBB",
        BOOT_METADATA_MAGIC,
        BOOT_METADATA_VERSION,
        0,
        1,          # generation
        SLOT_A,     # active_slot
        SLOT_A,     # boot_slot
        SLOT_NONE,  # trial_slot
        0,          # trial_armed
        0,          # trial_attempted
        node_id & 0xFF,
        0,          # boot_flags
    )
    suffix = struct.pack("<II", 0, 0)  # boot_counter, last_error
    without_crc = prefix + slot_a + slot_b + suffix + struct.pack("<I", 0)
    metadata_crc = crc32(without_crc)
    return prefix + slot_a + slot_b + suffix + struct.pack("<I", metadata_crc)


def cmd_update_package(args: argparse.Namespace) -> int:
    slot_a_bin = Path(args.slot_a_bin).resolve()
    slot_b_bin = Path(args.slot_b_bin).resolve()
    version = parse_version_header(Path(args.version_header))

    slot_a_data = slot_a_bin.read_bytes()
    slot_b_data = slot_b_bin.read_bytes()

    payload = {
        "schema_version": 1,
        "format": "sensor_system_node_update_package",
        "version": version,
        "slot_a": {
            "slot_id": SLOT_A,
            "path": slot_a_bin.name,
            "size": len(slot_a_data),
            "crc32": crc32(slot_a_data),
        },
        "slot_b": {
            "slot_id": SLOT_B,
            "path": slot_b_bin.name,
            "size": len(slot_b_data),
            "crc32": crc32(slot_b_data),
        },
    }

    output = Path(args.output)
    output.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return 0


def cmd_factory_image(args: argparse.Namespace) -> int:
    bootloader_bin = Path(args.bootloader_bin).resolve()
    slot_a_bin = Path(args.slot_a_bin).resolve()
    output = Path(args.output)
    version = parse_version_header(Path(args.version_header))

    bootloader = bootloader_bin.read_bytes()
    slot_a = slot_a_bin.read_bytes()
    slot_a_crc = crc32(slot_a)
    metadata = build_boot_metadata(
        node_id=args.node_id,
        slot_a_size=len(slot_a),
        slot_a_crc32=slot_a_crc,
        slot_a_version=version,
    )

    image_end = args.slot_a_offset + len(slot_a)
    image = bytearray(b"\xFF" * image_end)
    image[:len(bootloader)] = bootloader
    image[args.metadata_primary_offset:args.metadata_primary_offset + len(metadata)] = metadata
    image[args.metadata_secondary_offset:args.metadata_secondary_offset + len(metadata)] = metadata
    image[args.slot_a_offset:args.slot_a_offset + len(slot_a)] = slot_a

    output.write_bytes(bytes(image))
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Build helper for the Sensor System node boot artifacts")
    subparsers = parser.add_subparsers(dest="command", required=True)

    update_package = subparsers.add_parser("update-package")
    update_package.add_argument("--slot-a-bin", required=True)
    update_package.add_argument("--slot-b-bin", required=True)
    update_package.add_argument("--version-header", required=True)
    update_package.add_argument("--output", required=True)
    update_package.set_defaults(func=cmd_update_package)

    factory_image = subparsers.add_parser("factory-image")
    factory_image.add_argument("--bootloader-bin", required=True)
    factory_image.add_argument("--slot-a-bin", required=True)
    factory_image.add_argument("--slot-a-offset", type=lambda raw: int(raw, 0), required=True)
    factory_image.add_argument("--metadata-primary-offset", type=lambda raw: int(raw, 0), required=True)
    factory_image.add_argument("--metadata-secondary-offset", type=lambda raw: int(raw, 0), required=True)
    factory_image.add_argument("--node-id", type=int, required=True)
    factory_image.add_argument("--version-header", required=True)
    factory_image.add_argument("--output", required=True)
    factory_image.set_defaults(func=cmd_factory_image)

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
