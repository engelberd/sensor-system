#!/usr/bin/env python3
"""Offline Capture v1 to Archive v1 command."""

from __future__ import annotations

import argparse
from datetime import date
from pathlib import Path
import sys


PROJECT_ROOT = Path(__file__).resolve().parents[1]
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from host.recorder.archive_v1 import ArchiveV1Compactor


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--day", required=True, type=date.fromisoformat)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--compression", choices=("gzip", "none"), default="gzip")
    parser.add_argument("--max-timing-residual-ns", type=int, default=1_000_000)
    parser.add_argument("captures", nargs="+", type=Path)
    args = parser.parse_args()
    report = ArchiveV1Compactor(
        max_timing_residual_ns=args.max_timing_residual_ns
    ).build(
        args.captures, args.output,
        archive_day=args.day, compression=args.compression,
    )
    print(
        f"[ARCHIVE] {report.output} samples={report.sample_count} "
        f"sources={report.source_count} controls={report.control_point_count} "
        f"sha256={report.sha256}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
