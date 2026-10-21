#!/usr/bin/env python3
"""Load and verify one Sensor System Analysis CSV v2 window."""

from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path
import sys

import numpy as np


EXPECTED_COLUMNS = ["x_m_s2", "y_m_s2", "z_m_s2"]
EXPECTED_EVENT_COLUMNS = [
    "row_index",
    "source_sample_number",
    "event",
    "count",
    "reason",
]
EXPECTED_TELEMETRY_COLUMNS = [
    "captured_device_time_us",
    "time_from_first_sample_s",
    "adxl355_temperature_c",
    "adxl355_temperature_raw",
]


def load_window(csv_path: str | Path) -> dict:
    """Load the data, metadata and events belonging to one CSV window."""
    csv_path = Path(csv_path)
    metadata_path = csv_path.with_suffix(".json")
    events_path = csv_path.with_name(f"{csv_path.stem}.events.csv")

    with metadata_path.open(encoding="utf-8") as file:
        metadata = json.load(file)

    if metadata.get("schema") != "sensor-system-analysis-csv-v2":
        raise ValueError("unsupported Analysis CSV schema")
    if metadata.get("data_file") != csv_path.name:
        raise ValueError("JSON refers to a different data file")

    with csv_path.open(encoding="utf-8", newline="") as file:
        columns = next(csv.reader(file), None)
    if columns != EXPECTED_COLUMNS:
        raise ValueError(f"expected columns: {EXPECTED_COLUMNS}")

    samples = np.loadtxt(
        csv_path,
        delimiter=",",
        skiprows=1,
        dtype=np.float64,
        ndmin=2,
    )
    if samples.shape[1] != len(EXPECTED_COLUMNS):
        raise ValueError("CSV rows do not contain exactly three acceleration values")
    if len(samples) != metadata.get("sample_count"):
        raise ValueError("CSV row count differs from JSON sample_count")
    if not np.isfinite(samples).all():
        raise ValueError("CSV contains NaN or infinite acceleration values")

    with events_path.open(encoding="utf-8", newline="") as file:
        reader = csv.DictReader(file)
        if reader.fieldnames != EXPECTED_EVENT_COLUMNS:
            raise ValueError(f"expected event columns: {EXPECTED_EVENT_COLUMNS}")
        raw_events = list(reader)

    try:
        events = [
            {
                "row_index": int(event["row_index"]),
                "source_sample_number": int(event["source_sample_number"]),
                "event": event["event"],
                "count": int(event["count"]),
                "reason": event["reason"],
            }
            for event in raw_events
        ]
        if any(event["event"] not in {"gap", "continuity_break"} for event in events):
            raise ValueError("unknown event type")
        if any(
            event["row_index"] < 0
            or event["row_index"] > len(samples)
            or event["count"] <= 0
            for event in events
        ):
            raise ValueError("invalid event location or count")
        gaps = [event for event in events if event["event"] == "gap"]
        continuity_breaks = [
            event for event in events if event["event"] == "continuity_break"
        ]
        missing_samples = sum(int(event["count"]) for event in gaps)
    except (KeyError, ValueError) as error:
        raise ValueError("events CSV contains an invalid row") from error

    if missing_samples != metadata.get("missing_sample_count"):
        raise ValueError("events and JSON disagree about missing samples")
    if len(continuity_breaks) != metadata.get("continuity_break_count"):
        raise ValueError("events and JSON disagree about continuity breaks")

    telemetry = np.empty((0, len(EXPECTED_TELEMETRY_COLUMNS)), dtype=np.float64)
    telemetry_file = metadata.get("telemetry_file")
    if telemetry_file is not None:
        telemetry_path = csv_path.with_name(telemetry_file)
        with telemetry_path.open(encoding="utf-8", newline="") as file:
            reader = csv.DictReader(file)
            if reader.fieldnames != EXPECTED_TELEMETRY_COLUMNS:
                raise ValueError(
                    f"expected telemetry columns: {EXPECTED_TELEMETRY_COLUMNS}"
                )
            raw_telemetry = list(reader)
        try:
            telemetry = np.asarray(
                [
                    [
                        int(row["captured_device_time_us"]),
                        float(row["time_from_first_sample_s"]),
                        float(row["adxl355_temperature_c"]),
                        int(row["adxl355_temperature_raw"]),
                    ]
                    for row in raw_telemetry
                ],
                dtype=np.float64,
            ).reshape((-1, len(EXPECTED_TELEMETRY_COLUMNS)))
        except (KeyError, ValueError) as error:
            raise ValueError("telemetry CSV contains an invalid row") from error
        if not np.isfinite(telemetry).all():
            raise ValueError("telemetry CSV contains NaN or infinite values")
        if len(telemetry) != metadata.get("telemetry_record_count"):
            raise ValueError("telemetry CSV and JSON disagree about record count")
        if len(telemetry):
            captured = telemetry[:, 0]
            offsets = telemetry[:, 1]
            temperatures = telemetry[:, 2]
            raw_codes = telemetry[:, 3]
            expected_offsets = (
                captured - float(metadata["first_device_time_us"])
            ) / 1_000_000.0
            if np.any(np.diff(captured) <= 0):
                raise ValueError("telemetry timestamps are not strictly increasing")
            if not np.allclose(offsets, expected_offsets, rtol=0.0, atol=0.5e-6):
                raise ValueError("telemetry offsets disagree with device timestamps")
            if np.any(offsets < 0):
                raise ValueError("telemetry precedes the analysis window")
            if np.any(captured > float(metadata["last_device_time_us"])):
                raise ValueError("telemetry follows the analysis window")
            if np.any((temperatures < -55.0) | (temperatures > 125.0)):
                raise ValueError("telemetry temperature is outside the supported range")
            if np.any((raw_codes < 0) | (raw_codes > 4095)):
                raise ValueError("telemetry raw code is outside the 12-bit range")

    odr_hz = metadata.get("estimated_odr_hz") or metadata.get("configured_odr_hz")
    if not odr_hz or odr_hz <= 0:
        raise ValueError("no usable ODR in JSON")

    sample_index = np.arange(len(samples), dtype=np.int64)
    for gap in gaps:
        sample_index[gap["row_index"] :] += gap["count"]
    time_s = sample_index.astype(np.float64) / float(odr_hz)
    return {
        "samples": samples,
        "time_s": time_s,
        "metadata": metadata,
        "events": events,
        "gaps": gaps,
        "odr_hz": float(odr_hz),
        "missing_samples": missing_samples,
        "continuity_breaks": len(continuity_breaks),
        "telemetry": telemetry,
        "telemetry_columns": EXPECTED_TELEMETRY_COLUMNS.copy(),
        "is_continuous": missing_samples == 0 and not continuity_breaks,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("csv", type=Path, help="path to the main Analysis CSV v2 file")
    args = parser.parse_args()

    try:
        window = load_window(args.csv)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1

    metadata = window["metadata"]
    duration_s = window["time_s"][-1] if len(window["time_s"]) > 0 else 0.0

    print(f"file: {args.csv}")
    print(f"first sample: {metadata['first_sample_time_local']}")
    print(
        f"samples: {len(window['samples'])}, "
        f"ODR: {window['odr_hz']:.6f} Hz, "
        f"duration: {duration_s:.2f} s"
    )
    print(
        f"missing samples: {window['missing_samples']}, "
        f"continuity breaks: {window['continuity_breaks']}"
    )
    print(f"telemetry snapshots: {len(window['telemetry'])}")
    print("continuous: YES" if window["is_continuous"] else "continuous: NO")
    return 0 if window["is_continuous"] else 2


if __name__ == "__main__":
    raise SystemExit(main())
