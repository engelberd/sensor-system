#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
from pathlib import Path


def parse_version_header(path: Path) -> tuple[int, int, int]:
    text = path.read_text(encoding="utf-8")

    def read_constant(name: str) -> int:
        match = re.search(rf"{name}\s*=\s*(\d+)", text)
        if match is None:
            raise RuntimeError(f"missing {name} in {path}")
        return int(match.group(1))

    return (
        read_constant("FW_VERSION_MAJOR"),
        read_constant("FW_VERSION_MINOR"),
        read_constant("FW_VERSION_PATCH"),
    )


def version_int(version: tuple[int, int, int]) -> int:
    major, minor, patch = version
    return ((major & 0xFF) << 16) | ((minor & 0xFF) << 8) | (patch & 0xFF)


def version_name(version: tuple[int, int, int]) -> str:
    return f"v{version[0]}.{version[1]}.{version[2]}"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def copy_required(src: Path, dst: Path) -> None:
    if not src.exists():
        raise RuntimeError(f"missing build artifact: {src}")
    shutil.copy2(src, dst)


def write_release_readme(path: Path, release_name: str) -> None:
    path.write_text(
        f"""# Sensor System Node {release_name}

This package contains node firmware artifacts only. Host tools and host
configuration are released separately.

## Files

- `sensor-system-node-{release_name}-factory.uf2`: first-time BOOTSEL flash image.
- `sensor-system-node-{release_name}-factory.bin`: raw full factory image.
- `sensor-system-node-{release_name}-update-package.json`: RS485 A/B update manifest.
- `sensor-system-node-{release_name}-slot-a.bin`: slot A application image.
- `sensor-system-node-{release_name}-slot-b.bin`: slot B application image.
- `sensor-system-node-{release_name}-direct.uf2`: bench-only diagnostic image without the A/B bootloader.
- `SHA256SUMS.txt`: checksums for all release files.

## First-Time Flash

1. Hold BOOTSEL on the Pico 2 and plug USB into the computer.
2. Copy `sensor-system-node-{release_name}-factory.uf2` to the mounted `RPI-RP2`
   drive.
3. Let the board reboot.
4. Commission a runtime node id over RS485 with the host release tools.

Factory images boot with `node_id=0`, so the node will not answer normal
runtime commands until it is commissioned.

## Remote Update

Use the host release tools with:

```bash
./hostctl update --port /dev/sensor-system-rs485 --node <node_id> \\
  --image sensor-system-node-{release_name}-update-package.json
```

Keep `sensor-system-node-{release_name}-slot-a.bin` and
`sensor-system-node-{release_name}-slot-b.bin` next to the JSON manifest.

## ADXL355 Wiring

The firmware uses `spi1` for the ADXL355 evaluation board.

| ADXL355 signal | ADXL355 pin | Pico 2 pin |
| --- | ---: | --- |
| VDDIO | 1 | 3V3 |
| VDD | 3 | 3V3 |
| GND | 5 | GND |
| SCLK/Vssio | 10 | GPIO10 / SPI1 SCK |
| MOSI/SDA | 12 | GPIO11 / SPI1 TX |
| MISO/SDA | 11 | GPIO12 / SPI1 RX |
| CS/SCL | 8 | GPIO13 |
| DRDY | 6 | GPIO14 |
| INT1 | 2 | GPIO15 |
| INT2 | 4 | Not connected |

Do not drive the ADXL355 `V1P8ANA`, `V1P8DIG`, or `Vddio` output pins from the
Pico. Keep the Pico and RS485 adapter grounds common.

## RS485 Wiring

| Signal | Pico 2 pin |
| --- | --- |
| UART TX | GPIO0 |
| UART RX | GPIO1 |
| Driver enable / DE | GPIO2 |
| GND | GND |

Connect the external RS485 transceiver A/B lines according to the transceiver
board markings, and keep polarity consistent across the bus.
""",
        encoding="utf-8",
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Package Sensor System node release artifacts")
    node_dir = Path(__file__).resolve().parents[1]
    parser.add_argument("--node-dir", default=str(node_dir))
    parser.add_argument("--build-dir", default=None)
    parser.add_argument("--output-dir", default=None)
    return parser


def main() -> int:
    args = build_parser().parse_args()

    node_dir = Path(args.node_dir).resolve()
    build_dir = Path(args.build_dir).resolve() if args.build_dir else node_dir / "build"
    version = parse_version_header(node_dir / "common" / "protocol_ids.h")
    release_name = version_name(version)
    release_dir = (
        Path(args.output_dir).resolve()
        if args.output_dir
        else build_dir / "releases" / f"sensor-system-node-{release_name}"
    )

    release_dir.mkdir(parents=True, exist_ok=True)

    copied: list[Path] = []

    def add(src_name: str, dst_name: str) -> Path:
        dst = release_dir / dst_name
        copy_required(build_dir / src_name, dst)
        copied.append(dst)
        return dst

    add("sensor-system-node-factory.uf2", f"sensor-system-node-{release_name}-factory.uf2")
    add("sensor-system-node-factory.bin", f"sensor-system-node-{release_name}-factory.bin")
    add("sensor-system-node-direct.uf2", f"sensor-system-node-{release_name}-direct.uf2")
    slot_a = add("sensor-system-node-slot-a.bin", f"sensor-system-node-{release_name}-slot-a.bin")
    slot_b = add("sensor-system-node-slot-b.bin", f"sensor-system-node-{release_name}-slot-b.bin")

    update_package_src = build_dir / "sensor-system-node-update-package.json"
    if not update_package_src.exists():
        raise RuntimeError(f"missing build artifact: {update_package_src}")
    update_package = json.loads(update_package_src.read_text(encoding="utf-8"))
    expected_version = version_int(version)
    if int(update_package.get("version", -1)) != expected_version:
        raise RuntimeError(
            f"update package version {update_package.get('version')} does not match "
            f"{release_name} ({expected_version})"
        )
    update_package["version_name"] = release_name
    update_package["slot_a"]["path"] = slot_a.name
    update_package["slot_b"]["path"] = slot_b.name
    update_package_path = release_dir / f"sensor-system-node-{release_name}-update-package.json"
    update_package_path.write_text(json.dumps(update_package, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    copied.append(update_package_path)

    readme = release_dir / "README.md"
    write_release_readme(readme, release_name)
    copied.append(readme)

    manifest = {
        "product": "sensor-system-node",
        "version": release_name,
        "firmware_version": {
            "major": version[0],
            "minor": version[1],
            "patch": version[2],
            "packed": expected_version,
        },
        "host_release_included": False,
        "files": [
            {
                "name": path.name,
                "size": path.stat().st_size,
                "sha256": sha256_file(path),
            }
            for path in sorted(copied, key=lambda p: p.name)
        ],
    }
    manifest_path = release_dir / "release-manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    checksums_path = release_dir / "SHA256SUMS.txt"
    checksum_lines = [
        f"{entry['sha256']}  {entry['name']}"
        for entry in manifest["files"]
    ]
    checksum_lines.append(f"{sha256_file(manifest_path)}  {manifest_path.name}")
    checksums_path.write_text("\n".join(checksum_lines) + "\n", encoding="utf-8")

    archive_base = release_dir.parent / release_dir.name
    archive_path = Path(shutil.make_archive(
        str(archive_base),
        "zip",
        root_dir=release_dir.parent,
        base_dir=release_dir.name,
    ))
    archive_checksum_path = archive_path.with_suffix(archive_path.suffix + ".sha256")
    archive_checksum_path.write_text(
        f"{sha256_file(archive_path)}  {archive_path.name}\n",
        encoding="utf-8",
    )

    print(release_dir)
    print(archive_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
