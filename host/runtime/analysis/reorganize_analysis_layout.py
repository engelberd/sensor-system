#!/usr/bin/env python3
"""Move legacy flat Analysis HDF5 files into date/channel directories."""

from __future__ import annotations

import argparse
import datetime as dt
import os
from pathlib import Path
import re

import h5py


CHANNEL_RE = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*\Z")


def product_destination(root: Path, path: Path) -> Path:
    with h5py.File(path, "r") as file:
        channel = str(file.attrs["channel_name"])
        first_local = str(file.attrs["first_sample_time_local"])
    if not CHANNEL_RE.fullmatch(channel):
        raise ValueError(f"{path}: invalid channel_name {channel!r}")
    try:
        date = dt.date.fromisoformat(first_local[:10]).isoformat()
    except ValueError as error:
        raise ValueError(f"{path}: invalid first_sample_time_local {first_local!r}") from error
    return root / date / channel / path.name


def plan_moves(root: Path) -> list[tuple[Path, Path]]:
    return [(path, product_destination(root, path)) for path in sorted(root.glob("*.h5"))]


def apply_moves(root: Path, moves: list[tuple[Path, Path]]) -> None:
    for _, destination in moves:
        if destination.exists():
            raise FileExistsError(f"refusing to replace {destination}")
    for source, destination in moves:
        destination.parent.mkdir(parents=True, exist_ok=True)
        source.rename(destination)
        _sync_directory(destination.parent)
    _sync_directory(root)


def _sync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--apply", action="store_true")
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    moves = plan_moves(root)
    for source, destination in moves:
        print(f"{source.relative_to(root)} -> {destination.relative_to(root)}")
    if arguments.apply:
        apply_moves(root, moves)
    print(f"products={len(moves)} applied={str(arguments.apply).lower()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
