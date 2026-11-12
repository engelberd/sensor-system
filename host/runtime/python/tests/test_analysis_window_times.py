import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

import h5py
import numpy as np

ANALYSIS = Path(__file__).resolve().parents[2] / "analysis"


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


module = load_module("analysis_window_times", ANALYSIS / "load_analysis_window.py")
writer = load_module("analysis_hdf5_writer", ANALYSIS / "write_analysis_hdf5.py")


class AnalysisWindowTimesTests(unittest.TestCase):
    def write_window(self, root, with_events=False, with_telemetry=True, with_time_sync=True):
        path = root / "window.h5"
        first = (1 << 53) + 13
        times = [first, first + 128001, first + 256005]
        metadata = dict(schema=writer.SCHEMA, data_file=path.name,
                        hardware_id="0102030405060708", boot_epoch=9,
                        window_ordinal=0, channel_name="line-b", node_address=1,
                        sample_count=3, missing_sample_count=2 if with_events else 0,
                        continuity_break_count=1 if with_events else 0,
                        event_record_count=2 if with_events else 0,
                        telemetry_record_count=int(with_telemetry),
                        telemetry_status="present" if with_telemetry else "no_observations_in_window",
                        time_sync_record_count=2 if with_time_sync else 0,
                        time_sync_host_clock_count=1 if with_time_sync else 0,
                        time_sync_status="present" if with_time_sync else "no_observations_available",
                        first_device_time_us=first, last_device_time_us=times[-1],
                        configured_odr_hz=7.8125,
                        first_sample_time_uncertainty_us=None,
                        configuration=dict(revision=13, offset_x=-8, range_g=2))
        spools = [root / name for name in ("samples.tmp", "events.tmp", "telemetry.tmp", "time-sync.tmp", "metadata.tmp")]
        rows = [[index, time, -1, 2, 3] for index, time in enumerate(times)]
        spools[0].write_text("".join(json.dumps(row) + "\n" for row in rows))
        events = [[2, 2, "gap", 2, "JournalOverflow"], [3, 5, "continuity_break", 1, "ProcessingReset"]] if with_events else []
        spools[1].write_text("".join(json.dumps(row) + "\n" for row in events))
        spools[2].write_text(json.dumps([times[1], 25.]) + "\n" if with_telemetry else "")
        sync_rows = [
            ["01010101010101010101010101010101", 1000, first - 100, first - 90, 1020, 10, 5],
            ["01010101010101010101010101010101", 2000, times[-1] + 90, times[-1] + 100, 2022, 12, 6],
        ] if with_time_sync else []
        spools[3].write_text("".join(json.dumps(row) + "\n" for row in sync_rows))
        spools[4].write_text(json.dumps(metadata))
        writer.write_window(*spools, path)
        for spool in spools:
            spool.unlink()
        return path, times

    def test_preserves_uint64_times_raw_axes_and_telemetry_in_one_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path, times = self.write_window(root)
            result = module.load_window(path)
            self.assertEqual(result["device_time_us"].tolist(), times)
            self.assertEqual(result["time_s"].tolist(), [0, 0.128001, 0.256005])
            self.assertEqual(result["telemetry_device_time_us"].tolist(), [times[1]])
            self.assertEqual(result["samples"].tolist(), [[-1, 2, 3]] * 3)
            self.assertEqual(result["samples"].shape, (3, 3))
            with h5py.File(path) as file:
                self.assertNotIn("metadata_json", file)
                self.assertEqual(set(file), {"samples", "events", "telemetry", "time_sync"})
                self.assertEqual(file.attrs["sample_count"], 3)
                self.assertEqual(file.attrs["first_device_time_us"], times[0])
                self.assertEqual(file.attrs["configuration_offset_x"], -8)
                self.assertEqual(file.attrs["configuration_revision"], 13)
                self.assertNotIn("first_sample_time_uncertainty_us", file.attrs)
                self.assertEqual(result["metadata"]["configuration"]["offset_x"], -8)
                self.assertIsInstance(file["telemetry"], h5py.Dataset)
                self.assertEqual(file["telemetry"].dtype.names, ("time_us", "temperature_c"))
                self.assertEqual(file["telemetry"].dtype["time_us"], np.dtype("uint64"))
                self.assertEqual(file["telemetry"].dtype["temperature_c"], np.dtype("float64"))
                self.assertEqual(file["telemetry"]["temperature_c"].tolist(), [25.])
                self.assertIsInstance(file["samples"], h5py.Dataset)
                self.assertEqual(file["samples"].dtype.names, ("sample_sequence", "czas_us", "x", "y", "z"))
                self.assertEqual(file["samples"].dtype["czas_us"], np.dtype("uint64"))
                self.assertEqual(file["samples"].dtype["x"], np.dtype("int32"))
                self.assertIsInstance(file["events"], h5py.Dataset)
                self.assertEqual(file["events"].dtype.names, tuple(module.EXPECTED_EVENT_COLUMNS))
                self.assertIsInstance(file["time_sync"], h5py.Dataset)
                self.assertEqual(file["time_sync"].dtype.names, tuple(module.EXPECTED_TIME_SYNC_COLUMNS))
                self.assertEqual(file["time_sync"]["host_clock_id"].tolist(), [b"01010101010101010101010101010101"] * 2)
            self.assertEqual(list(root.iterdir()), [path])

    def test_rejects_non_finite_or_out_of_window_telemetry(self):
        for field, bad_value, message in [("temperature_c", float("nan"), "NaN"),
                                          ("temperature_c", 150., "supported range"),
                                          ("time_us", 0, "precedes")]:
            with self.subTest(field=field, bad_value=bad_value):
                with tempfile.TemporaryDirectory() as temporary:
                    path, _ = self.write_window(Path(temporary))
                    with h5py.File(path, "r+") as file:
                        table = file["telemetry"][:]
                        table[field][0] = bad_value
                        file["telemetry"][:] = table
                    with self.assertRaisesRegex(ValueError, message):
                        module.load_window(path)

    def test_native_attributes_preserve_full_uint64_and_string_arrays(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "attrs.h5"
            with h5py.File(path, "w") as file:
                writer.write_attributes(file.attrs, dict(boot_epoch=(1 << 64) - 1,
                    columns=["sample_sequence", "czas_us", "x", "y", "z"],
                    configuration=dict(offset_x=-32768), unavailable=None))
            with h5py.File(path) as file:
                meta = module.read_metadata(file.attrs)
                self.assertEqual(meta["boot_epoch"], (1 << 64) - 1)
                self.assertEqual(meta["columns"], ["sample_sequence", "czas_us", "x", "y", "z"])
                self.assertEqual(meta["configuration"]["offset_x"], -32768)
                self.assertNotIn("unavailable", meta)

    def test_short_window_can_explicitly_have_no_telemetry(self):
        with tempfile.TemporaryDirectory() as temporary:
            path, _ = self.write_window(Path(temporary), with_telemetry=False)
            result = module.load_window(path)
            self.assertEqual(len(result["telemetry"]), 0)
            self.assertEqual(result["metadata"]["telemetry_status"], "no_observations_in_window")
            self.assertEqual(result["samples"].shape, (3, 3))

    def test_window_can_explicitly_have_no_time_sync(self):
        with tempfile.TemporaryDirectory() as temporary:
            path, _ = self.write_window(Path(temporary), with_time_sync=False)
            result = module.load_window(path)
            self.assertEqual(len(result["time_sync"]), 0)
            self.assertEqual(result["metadata"]["time_sync_status"], "no_observations_available")

    def test_rejects_time_sync_derived_values_inconsistent_with_timestamps(self):
        with tempfile.TemporaryDirectory() as temporary:
            path, _ = self.write_window(Path(temporary))
            with h5py.File(path, "r+") as file:
                table = file["time_sync"][:]
                table["path_delay_us"][0] += 1
                file["time_sync"][:] = table
            with self.assertRaisesRegex(ValueError, "path delay disagrees"):
                module.load_window(path)

    def test_rejects_inconsistent_time_sync_metadata(self):
        for attribute, value, message in [
            ("time_sync_status", "no_observations_available", "status"),
            ("time_sync_host_clock_count", 2, "host clock count"),
        ]:
            with self.subTest(attribute=attribute):
                with tempfile.TemporaryDirectory() as temporary:
                    path, _ = self.write_window(Path(temporary))
                    with h5py.File(path, "r+") as file:
                        file.attrs.modify(attribute, value)
                    with self.assertRaisesRegex(ValueError, message):
                        module.load_window(path)

    def test_previous_schema_is_rebuilt_atomically_under_the_same_name(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path, _ = self.write_window(root)
            stem = "line-b_2026-11-11_16-38"
            named = root / (stem + ".h5")
            path.rename(named)
            with h5py.File(named, "r+") as file:
                file.attrs["schema"] = "sensor-system-analysis-hdf5-v1"
            self.assertEqual(writer.product_index(root), [])
            result = writer.select_product(root, stem, "0102030405060708", "9", "0")
            self.assertFalse(result["already_exists"])
            self.assertEqual(Path(result["path"]), named)
            self.assertTrue(named.exists())

    def test_readable_names_keep_identity_in_hdf5_and_never_overwrite_a_collision(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path, _ = self.write_window(root)
            stem = "line-b_2026-11-11_16-38"
            named = root / (stem + ".h5")
            path.rename(named)
            existing = writer.select_product(root, stem, "0102030405060708", "9", "0")
            self.assertTrue(existing["already_exists"])
            self.assertEqual(Path(existing["path"]), named)
            collision = writer.select_product(root, stem, "0102030405060708", "9", "1")
            self.assertFalse(collision["already_exists"])
            self.assertEqual(Path(collision["path"]).name, stem + "_2.h5")
            self.assertEqual(writer.product_index(root)[0]["window_ordinal"], 0)

    def test_rejects_inconsistent_time_metadata(self):
        with tempfile.TemporaryDirectory() as temporary:
            path, _ = self.write_window(Path(temporary))
            with h5py.File(path, "r+") as file:
                file.attrs.modify("first_device_time_us", int(file.attrs["first_device_time_us"]) + 1)
            with self.assertRaisesRegex(ValueError, "bounds disagree"):
                module.load_window(path)

    def test_rejects_previous_schema(self):
        with tempfile.TemporaryDirectory() as temporary:
            path, _ = self.write_window(Path(temporary))
            with h5py.File(path, "r+") as file:
                file.attrs["schema"] = "sensor-system-analysis-csv-v3"
            with self.assertRaisesRegex(ValueError, "unsupported"):
                module.load_window(path)

    def test_preserves_gap_and_reset_reasons(self):
        with tempfile.TemporaryDirectory() as temporary:
            path, _ = self.write_window(Path(temporary), with_events=True)
            result = module.load_window(path)
            self.assertEqual(result["missing_samples"], 2)
            self.assertEqual(result["continuity_breaks"], 1)
            self.assertEqual(result["gaps"][0]["reason"], "JournalOverflow")
            self.assertFalse(result["is_continuous"])

    def test_rejects_event_table_with_invalid_shape(self):
        with tempfile.TemporaryDirectory() as temporary:
            path, _ = self.write_window(Path(temporary))
            with h5py.File(path, "r+") as file:
                del file["events"]
                file.create_dataset("events", (0,), dtype=np.dtype([("count", "<u8")]))
            with self.assertRaisesRegex(ValueError, "events must be one table"):
                module.load_window(path)

    def test_writer_uses_bounded_batches_and_lossless_compression(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            spool = root / "rows.tmp"
            spool.write_text("[9007199254741005,25.0]\n" * (writer.BATCH_ROWS + 1))
            with h5py.File(root / "batch.h5", "x") as file:
                count = writer.write_table(file, "telemetry", spool, writer.TELEMETRY_DTYPE,
                                           {"time_unit": "us", "temperature_unit": "degC"})
                self.assertEqual(count, writer.BATCH_ROWS + 1)
                self.assertEqual(int(file["telemetry"][-1]["time_us"]), 9007199254741005)
                self.assertEqual(file["telemetry"].compression, "gzip")
                self.assertTrue(file["telemetry"].fletcher32)
