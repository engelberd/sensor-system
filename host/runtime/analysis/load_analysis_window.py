#!/usr/bin/env python3
"""Load and verify one Sensor System Analysis HDF5 v6 window."""

from __future__ import annotations

import argparse
import h5py
from pathlib import Path
import sys

import numpy as np


EXPECTED_COLUMNS = ["sample_sequence", "czas_us", "x", "y", "z"]
EXPECTED_EVENT_COLUMNS = [
    "row_index",
    "source_sample_number",
    "event",
    "count",
    "reason",
]
EXPECTED_TELEMETRY_COLUMNS = ["time_us", "temperature_c"]
EXPECTED_TIME_SYNC_COLUMNS = [
    "host_clock_id",
    "t1_host_us",
    "t2_node_us",
    "t3_node_us",
    "t4_host_us",
    "path_delay_us",
    "uncertainty_us",
]


def attribute_value(value):
    if isinstance(value, np.ndarray):
        return [attribute_value(item) for item in value.tolist()]
    if isinstance(value, np.generic):
        return attribute_value(value.item())
    if isinstance(value, bytes):
        return value.decode("utf-8")
    return value


def read_metadata(attributes):
    metadata = {key: attribute_value(value) for key, value in attributes.items()}
    configuration = {key.removeprefix("configuration_"): metadata.pop(key)
                     for key in list(metadata) if key.startswith("configuration_")}
    if configuration:
        metadata["configuration"] = configuration
    return metadata


def load_window(hdf5_path: str | Path) -> dict:
    """Load a self-contained HDF5 window, preserving integer timestamps."""
    hdf5_path = Path(hdf5_path)
    with h5py.File(hdf5_path, "r") as file:
        metadata = read_metadata(file.attrs)
        if file.attrs.get("schema") != "sensor-system-analysis-hdf5-v6" or metadata.get("schema") != "sensor-system-analysis-hdf5-v6":
            raise ValueError("unsupported Analysis HDF5 schema")
        table = file["samples"][:]
        expected_dtype = np.dtype([("sample_sequence", "<u8"), ("czas_us", "<u8"),
                                   ("x", "<i4"), ("y", "<i4"), ("z", "<i4")])
        if table.dtype != expected_dtype or table.ndim != 1:
            raise ValueError("samples must be one raw table with columns sample_sequence, czas_us, x, y, z")
        sequence = table["sample_sequence"]
        device_time_us = table["czas_us"]
        samples = np.column_stack([table[axis] for axis in "xyz"])
        event_table = file["events"][:]
        event_dtype = np.dtype([
            ("row_index", "<u8"),
            ("source_sample_number", "<u8"),
            ("event", h5py.string_dtype("utf-8")),
            ("count", "<u8"),
            ("reason", h5py.string_dtype("utf-8")),
        ])
        if event_table.dtype != event_dtype or event_table.ndim != 1:
            raise ValueError("events must be one table with row_index, source_sample_number, event, count, reason")
        telemetry = file["telemetry"][:]
        telemetry_dtype = np.dtype([("time_us", "<u8"), ("temperature_c", "<f8")])
        if telemetry.dtype != telemetry_dtype or telemetry.ndim != 1:
            raise ValueError("telemetry must be one table with time_us uint64 and temperature_c float64")
        telemetry_times = telemetry["time_us"]
        time_sync = file["time_sync"][:]
        time_sync_dtype = np.dtype([
            ("host_clock_id", "S32"),
            ("t1_host_us", "<u8"),
            ("t2_node_us", "<u8"),
            ("t3_node_us", "<u8"),
            ("t4_host_us", "<u8"),
            ("path_delay_us", "<u8"),
            ("uncertainty_us", "<u8"),
        ])
        if time_sync.dtype != time_sync_dtype or time_sync.ndim != 1:
            raise ValueError("time_sync must be one table with host clock identity and t1..t4")
    if len(sequence) != len(device_time_us) or len(samples) != len(sequence):
        raise ValueError("sample dataset lengths disagree")
    time_s = np.empty(0, dtype=np.float64)
    if len(device_time_us):
        first_time = int(metadata["first_device_time_us"])
        time_s = np.array([(int(value) - first_time) / 1_000_000.0
                                    for value in device_time_us])
        if np.any(device_time_us[1:] <= device_time_us[:-1]):
            raise ValueError("sample timestamps are not strictly increasing")
        if np.any(sequence[1:] <= sequence[:-1]):
            raise ValueError("sample sequences are not strictly increasing")
        if int(device_time_us[0]) != first_time or int(device_time_us[-1]) != int(metadata["last_device_time_us"]):
            raise ValueError("sample bounds disagree with metadata")
    if len(samples) != metadata.get("sample_count"):
        raise ValueError("HDF5 row count differs from metadata sample_count")
    if not np.isfinite(samples).all():
        raise ValueError("HDF5 contains NaN or infinite raw sample values")

    try:
        events = [
            {
                "row_index": int(event["row_index"]),
                "source_sample_number": int(event["source_sample_number"]),
                "event": attribute_value(event["event"]),
                "count": int(event["count"]),
                "reason": attribute_value(event["reason"]),
            }
            for event in event_table
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
        raise ValueError("events HDF5 contains an invalid row") from error

    if missing_samples != metadata.get("missing_sample_count"):
        raise ValueError("events and metadata disagree about missing samples")
    if len(continuity_breaks) != metadata.get("continuity_break_count"):
        raise ValueError("events and metadata disagree about continuity breaks")
    if len(events) != metadata.get("event_record_count"):
        raise ValueError("events and metadata disagree about record count")

    if telemetry_times.dtype != np.dtype("uint64"):
        raise ValueError("telemetry times must be uint64")
    if not np.isfinite(telemetry["temperature_c"]).all():
        raise ValueError("telemetry HDF5 contains NaN or infinite values")
    if len(telemetry) != metadata.get("telemetry_record_count"):
        raise ValueError("telemetry HDF5 and metadata disagree about record count")
    if len(telemetry):
        captured = telemetry_times
        temperatures = telemetry["temperature_c"]
        if np.any(captured[1:] <= captured[:-1]):
            raise ValueError("telemetry timestamps are not strictly increasing")
        if np.any(captured < int(metadata["first_device_time_us"])):
            raise ValueError("telemetry precedes the analysis window")
        if np.any(captured > int(metadata["last_device_time_us"])):
            raise ValueError("telemetry follows the analysis window")
        if np.any((temperatures < -55.0) | (temperatures > 125.0)):
            raise ValueError("telemetry temperature is outside the supported range")

    if len(time_sync) != metadata.get("time_sync_record_count"):
        raise ValueError("time_sync HDF5 and metadata disagree about record count")
    expected_time_sync_status = "present" if len(time_sync) else "no_observations_available"
    if metadata.get("time_sync_status") != expected_time_sync_status:
        raise ValueError("time_sync HDF5 and metadata disagree about status")
    observations_by_clock = {}
    for observation in time_sync:
        host_clock_id = bytes(observation["host_clock_id"]).decode("ascii")
        if len(host_clock_id) != 32 or any(character not in "0123456789abcdef" for character in host_clock_id):
            raise ValueError("time_sync contains an invalid host_clock_id")
        t1 = int(observation["t1_host_us"])
        t2 = int(observation["t2_node_us"])
        t3 = int(observation["t3_node_us"])
        t4 = int(observation["t4_host_us"])
        if t4 < t1 or t3 < t2 or t3 - t2 > t4 - t1:
            raise ValueError("time_sync contains invalid four-timestamp ordering")
        path_delay = (t4 - t1) - (t3 - t2)
        if int(observation["path_delay_us"]) != path_delay:
            raise ValueError("time_sync path delay disagrees with t1..t4")
        if int(observation["uncertainty_us"]) != (path_delay + 1) // 2:
            raise ValueError("time_sync uncertainty disagrees with path delay")
        previous = observations_by_clock.get(host_clock_id)
        if previous is not None and any(current <= prior for current, prior in zip((t1, t2, t3, t4), previous)):
            raise ValueError("time_sync observations are not strictly increasing")
        observations_by_clock[host_clock_id] = (t1, t2, t3, t4)
    if len(observations_by_clock) != metadata.get("time_sync_host_clock_count"):
        raise ValueError("time_sync HDF5 and metadata disagree about host clock count")

    odr_hz = metadata.get("estimated_odr_hz") or metadata.get("configured_odr_hz")
    if not odr_hz or odr_hz <= 0:
        raise ValueError("no usable ODR in metadata")

    return {
        "samples": samples,
        "sample_table": table,
        "telemetry_device_time_us": telemetry_times,
        "sample_sequence": sequence,
        "device_time_us": device_time_us,
        "time_s": time_s,
        "metadata": metadata,
        "events": events,
        "gaps": gaps,
        "odr_hz": float(odr_hz),
        "missing_samples": missing_samples,
        "continuity_breaks": len(continuity_breaks),
        "telemetry": telemetry,
        "telemetry_columns": EXPECTED_TELEMETRY_COLUMNS.copy(),
        "time_sync": time_sync,
        "time_sync_columns": EXPECTED_TIME_SYNC_COLUMNS.copy(),
        "is_continuous": missing_samples == 0 and not continuity_breaks,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("hdf5", type=Path, help="path to the main Analysis HDF5 v6 file")
    args = parser.parse_args()

    try:
        window = load_window(args.hdf5)
    except (OSError, ValueError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1

    metadata = window["metadata"]
    duration_s = window["time_s"][-1] if len(window["time_s"]) > 0 else 0.0

    print(f"file: {args.hdf5}")
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
    print(f"time sync observations: {len(window['time_sync'])}")
    print("continuous: YES" if window["is_continuous"] else "continuous: NO")
    return 0 if window["is_continuous"] else 2


if __name__ == "__main__":
    raise SystemExit(main())
