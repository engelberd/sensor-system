#!/usr/bin/env python3
"""Validate complete Sensor System HDF5 files and report timing diagnostics."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys


PROJECT_ROOT = Path(__file__).resolve().parents[1]
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from host.recorder.verification import verify_hdf5


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="+", type=Path)
    parser.add_argument("--json", action="store_true", dest="as_json")
    parser.add_argument("--allow-partial", action="store_true")
    parser.add_argument("--skip-manifest", action="store_true")
    parser.add_argument("--odr-warning-ppm", type=float, default=1000.0)
    parser.add_argument("--resample-threshold-ppm", type=float, default=50.0)
    args = parser.parse_args()
    reports = []
    exit_code = 0
    for path in args.paths:
        try:
            report = verify_hdf5(
                path,
                allow_partial=args.allow_partial,
                verify_manifest=not args.skip_manifest,
                odr_warning_ppm=args.odr_warning_ppm,
                resample_threshold_ppm=args.resample_threshold_ppm,
            )
            reports.append(report.to_dict())
            if not report.valid:
                exit_code = 2
        except Exception as exc:
            reports.append({"path": str(path), "valid": False, "errors": [str(exc)]})
            exit_code = 2
    if args.as_json:
        print(json.dumps(reports, indent=2, sort_keys=True))
    else:
        for report in reports:
            status = "OK" if report.get("valid") else "INVALID"
            print(
                f"[{status}] {report['path']} product={report.get('product', '-')} "
                f"samples={report.get('sample_count', '-')}"
            )
            timing = report.get("diagnostics", {}).get("timing", {})
            if timing:
                print(
                    "  ODR: nominal={nominal_odr_hz} Hz observed={observed_odr_hz} Hz "
                    "shift={odr_shift_ppm} ppm intervals={interval_count}".format(**timing)
                )
            for warning in report.get("warnings", []):
                print(f"  warning: {warning}")
            for error in report.get("errors", []):
                print(f"  error: {error}")
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
