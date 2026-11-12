"""Embedded offline HDF5 writer. Input spool files are private JSON lines, never CSV.

The Rust exporter validates Capture and controls atomic publication. This writer
uses bounded batches and creates a new, unpublished file only.
"""
import itertools
import json
from pathlib import Path
import sys

import h5py
import numpy as np

BATCH_ROWS = 8192
SCHEMA = "sensor-system-analysis-hdf5-v6"
SAMPLE_DTYPE = np.dtype([("sample_sequence", "<u8"), ("czas_us", "<u8"),
                         ("x", "<i4"), ("y", "<i4"), ("z", "<i4")])

TELEMETRY_DTYPE = np.dtype([("time_us", "<u8"), ("temperature_c", "<f8")])
TIME_SYNC_DTYPE = np.dtype([
    ("host_clock_id", "S32"),
    ("t1_host_us", "<u8"),
    ("t2_node_us", "<u8"),
    ("t3_node_us", "<u8"),
    ("t4_host_us", "<u8"),
    ("path_delay_us", "<u8"),
    ("uncertainty_us", "<u8"),
])
EVENT_DTYPE = np.dtype([
    ("row_index", "<u8"),
    ("source_sample_number", "<u8"),
    ("event", h5py.string_dtype("utf-8")),
    ("count", "<u8"),
    ("reason", h5py.string_dtype("utf-8")),
])


def write_table(file, name, path, dtype, attributes, *, fletcher32=True):
    dataset = file.create_dataset(name, (0,), maxshape=(None,), dtype=dtype,
                                  chunks=(BATCH_ROWS,), compression="gzip",
                                  compression_opts=4, shuffle=True,
                                  fletcher32=fletcher32)
    dataset.attrs.update(attributes)
    count = 0
    with open(path, encoding="utf-8") as source:
        while batch := list(itertools.islice(source, BATCH_ROWS)):
            rows = [tuple(json.loads(line)) for line in batch]
            if any(len(row) != len(dtype.names) for row in rows):
                raise ValueError(f"invalid {name} row width")
            end = count + len(rows)
            dataset.resize((end,))
            dataset[count:end] = np.asarray(rows, dtype=dtype)
            count = end
    return count


def write_attributes(attributes, metadata, prefix=""):
    for key, value in metadata.items():
        name = prefix + key
        if value is None:
            # An absent optional attribute means unavailable, never zero.
            continue
        if isinstance(value, dict):
            write_attributes(attributes, value, name + "_")
        elif isinstance(value, bool):
            attributes[name] = np.bool_(value)
        elif isinstance(value, int):
            attributes[name] = np.uint64(value) if value >= 0 else np.int64(value)
        elif isinstance(value, list):
            attributes.create(name, np.asarray(value, dtype=h5py.string_dtype("utf-8")))
        else:
            attributes[name] = value


def write_window(samples, events, telemetry, time_sync, metadata, output):
    with open(metadata, encoding="utf-8") as source:
        meta = json.load(source)
    if meta["schema"] != SCHEMA:
        raise ValueError("unsupported analysis schema")
    with h5py.File(output, "x") as file:
        write_attributes(file.attrs, meta)
        count = write_table(file, "samples", samples, SAMPLE_DTYPE,
                            {"axis_unit": "raw", "time_unit": "us"})
        if count != meta["sample_count"]:
            raise ValueError("sample count mismatch")
        event_count = write_table(file, "events", events, EVENT_DTYPE, {},
                                  fletcher32=False)
        if event_count != meta["event_record_count"]:
            raise ValueError("event count mismatch")
        count = write_table(file, "telemetry", telemetry, TELEMETRY_DTYPE,
                            {"time_unit": "us", "temperature_unit": "degC"})
        if count != meta["telemetry_record_count"]:
            raise ValueError("telemetry count mismatch")
        count = write_table(file, "time_sync", time_sync, TIME_SYNC_DTYPE,
                            {"time_unit": "us",
                             "host_time_domain": "CLOCK_BOOTTIME per host_clock_id",
                             "node_time_domain": "RP2350 monotonic per boot_epoch"})
        if count != meta["time_sync_record_count"]:
            raise ValueError("time sync count mismatch")
        file.flush()


def product_identity(path):
    with h5py.File(path, "r") as file:
        if file.attrs.get("schema") not in {SCHEMA, "sensor-system-analysis-hdf5-v1", "sensor-system-analysis-hdf5-v2", "sensor-system-analysis-hdf5-v3", "sensor-system-analysis-hdf5-v4", "sensor-system-analysis-hdf5-v5"}:
            raise ValueError(f"{path}: unsupported analysis schema")
        return {"schema": str(file.attrs["schema"]), "hardware_id": str(file.attrs["hardware_id"]),
                "boot_epoch": int(file.attrs["boot_epoch"]),
                "window_ordinal": int(file.attrs["window_ordinal"])}


def product_index(root):
    products = []
    for path in sorted(Path(root).rglob("*.h5")):
        identity = product_identity(path)
        if identity["schema"] == SCHEMA:
            products.append(dict(path=str(path), **identity))
    return products


def select_product(root, stem, hardware_id, boot_epoch, ordinal):
    """Called under the Rust directory lock. Never replace another window."""
    expected = dict(hardware_id=hardware_id, boot_epoch=int(boot_epoch),
                    window_ordinal=int(ordinal))
    root = Path(root)
    candidates = [root / (stem + ".h5")]
    candidates.extend(sorted(root.glob(stem + "_*.h5")))
    for path in candidates:
        if path.is_file():
            identity = product_identity(path)
            if all(identity[key] == value for key, value in expected.items()):
                return dict(path=str(path), already_exists=identity["schema"] == SCHEMA)
    suffix = 1
    while True:
        path = root / (stem + (f"_{suffix}" if suffix > 1 else "") + ".h5")
        if not path.exists():
            return dict(path=str(path), already_exists=False)
        suffix += 1


if __name__ == "__main__":
    if sys.argv[1] == "--index":
        print(json.dumps(product_index(sys.argv[2])))
    elif sys.argv[1] == "--select":
        print(json.dumps(select_product(*sys.argv[2:])))
    else:
        write_window(*sys.argv[1:])
