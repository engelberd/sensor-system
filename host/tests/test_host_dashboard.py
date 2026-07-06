from __future__ import annotations

import http.client
import json
import tempfile
import threading
import unittest
import zipfile
from io import BytesIO
from pathlib import Path

from host.dashboard.app import DashboardRepository, DashboardServer


class DashboardRepositoryTests(unittest.TestCase):
    def create_live_hdf5(self, path: Path) -> None:
        try:
            import h5py  # type: ignore
        except ImportError as exc:
            self.skipTest(f"h5py is required for live preview tests: {exc}")

        with h5py.File(path, "w") as handle:
            handle.attrs["created_utc"] = "2026-05-12T12:00:00+00:00"
            handle.attrs["window_start_utc"] = "2026-05-12T12:00:00+00:00"
            handle.attrs["window_end_utc"] = "2026-05-12T12:00:10+00:00"
            nodes = handle.require_group("nodes")
            group = nodes.require_group("1")
            group.attrs["node_id"] = 1
            group.attrs["sensor_odr_hz"] = 125
            group.attrs["output_odr_hz"] = 62.5
            group.attrs["range_g"] = 2
            group.attrs["accel_unit"] = "m/s^2"
            dataset = group.create_dataset(
                "samples",
                shape=(5,),
                dtype=[
                    ("sample_seq", "<u8"),
                    ("x", "<f4"),
                    ("y", "<f4"),
                    ("z", "<f4"),
                    ("packet_seq", "<u4"),
                ],
            )
            dataset[...] = [
                (101, 0.1, 0.2, 0.3, 1),
                (102, 0.4, 0.5, 0.6, 2),
                (103, 0.7, 0.8, 0.9, 3),
                (104, 1.0, 1.1, 1.2, 4),
                (105, 1.3, 1.4, 1.5, 5),
            ]

    def create_live_hdf5_with_window(self, path: Path, start_utc: str, end_utc: str) -> None:
        try:
            import h5py  # type: ignore
        except ImportError as exc:
            self.skipTest(f"h5py is required for live preview tests: {exc}")

        with h5py.File(path, "w") as handle:
            handle.attrs["created_utc"] = start_utc
            handle.attrs["window_start_utc"] = start_utc
            handle.attrs["window_end_utc"] = end_utc
            nodes = handle.require_group("nodes")
            group = nodes.require_group("1")
            group.attrs["node_id"] = 1
            group.attrs["sensor_odr_hz"] = 125
            group.attrs["output_odr_hz"] = 62.5
            group.attrs["range_g"] = 2
            group.attrs["accel_unit"] = "m/s^2"
            dataset = group.create_dataset(
                "samples",
                shape=(2,),
                dtype=[
                    ("sample_seq", "<u8"),
                    ("x", "<f4"),
                    ("y", "<f4"),
                    ("z", "<f4"),
                    ("packet_seq", "<u4"),
                ],
            )
            dataset[...] = [
                (1, 0.1, 0.2, 0.3, 1),
                (2, 0.4, 0.5, 0.6, 2),
            ]

    def test_dashboard_merges_config_runtime_and_events(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            status_path = temp_dir / "supervisor.status.json"
            event_path = temp_dir / "supervisor.events.jsonl"
            config_path = temp_dir / "system_config.json"
            process_log_path = temp_dir / "channels" / "line-a.process.log"
            runs_root = temp_dir / "runs" / "sensor-system"
            runs_root.mkdir(parents=True)
            process_log_path.parent.mkdir(parents=True)
            process_log_path.write_text(
                "[REC] t=    8.0s samples=1000 rate=  125.0 samples/s\n",
                encoding="utf-8",
            )

            config_path.write_text(
                json.dumps(
                    {
                        "system": {
                            "name": "sensor-system-prod",
                            "site": "hala-a",
                            "timezone": "Europe/Warsaw",
                        },
                        "storage": {
                            "root_dir": str(runs_root),
                            "format": "hdf5",
                            "compression": "gzip",
                            "window_seconds": 600,
                        },
                        "supervisor": {
                            "status_file": str(status_path),
                            "event_log": str(event_path),
                            "channel_runtime_dir": str(temp_dir / "channels"),
                        },
                        "channels": [
                            {
                                "name": "line-a",
                                "label": "Linia A",
                                "port": "/dev/ttyUSB0",
                                "baud": 115200,
                                "nodes": [
                                    {
                                        "id": 1,
                                        "name": "Czujnik 1",
                                        "expected_odr_hz": 62.5,
                                    }
                                ],
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )

            status_path.write_text(
                json.dumps(
                    {
                        "updated_utc": "2026-04-29T10:00:00+00:00",
                        "started_utc": "2026-04-29T09:00:00+00:00",
                        "supervisor_version": "0.3.0",
                        "storage_root": "runs/sensor-system",
                        "channels": [
                            {
                                "name": "line-a",
                                "label": "Linia A",
                                "enabled": True,
                                "port": "/dev/ttyUSB0",
                                "baud": 115200,
                                "process_id": 1234,
                                "running": True,
                                "restart_count": 2,
                                "last_exit_code": None,
                                "updated_utc": "2026-04-29T10:00:00+00:00",
                                "destination": "runs/sensor-system/day-1.h5",
                                "active_file": "runs/sensor-system/day-1.h5",
                                "status_file": "/tmp/channels/line-a.status.json",
                                "event_log": "/tmp/channels/line-a.events.jsonl",
                                "process_log": str(process_log_path),
                                "nodes": [
                                    {
                                        "node_id": 1,
                                        "name": "Czujnik 1",
                                        "online": True,
                                        "sensor_odr_hz": 125,
                                        "output_odr_hz": 62.5,
                                        "samples_written": 1200,
                                        "instant_samples_per_second_5s": 61.8,
                                        "rate_stability_percent_5s": 97.4,
                                        "expected_sample_seq": 1201,
                                        "last_written_seq": 1200,
                                        "bursts_ok": 100,
                                        "bursts_no_data": 0,
                                        "bursts_failed": 0,
                                        "gaps_detected": 0,
                                        "empty_polls": 0,
                                        "sensor_loss_total": 0,
                                        "sensor_loss_session": 0,
                                        "rx_overflow_total": 0,
                                        "rx_overflow_session": 0,
                                        "packet_overwrite_total": 0,
                                        "packet_overwrite_session": 0,
                                        "last_temperature_c": 24.5,
                                        "last_temperature_unix_ns": 1_714_382_800_000_000_000,
                                    }
                                ],
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )

            event_path.write_text(
                "\n".join(
                    [
                        json.dumps({"utc": "2026-04-29T10:00:01+00:00", "severity": "info", "event": "channel_started", "channel_name": "line-a"}),
                        json.dumps({"utc": "2026-04-29T10:00:02+00:00", "severity": "warning", "event": "temperature_delayed", "channel_name": "line-a", "node_id": 1}),
                    ]
                )
                + "\n",
                encoding="utf-8",
            )

            repository = DashboardRepository(config_path)
            payload = repository.dashboard_payload(limit=10)

            self.assertEqual(payload["config"]["system"]["site"], "hala-a")
            self.assertTrue(payload["supervisor"]["has_status"])
            self.assertEqual(payload["overview"]["channels_running"], 1)
            self.assertEqual(payload["overview"]["nodes_online"], 1)
            self.assertEqual(payload["overview"]["samples_written_total"], 1200)
            self.assertEqual(payload["overview"]["events_by_severity"]["warning"], 1)
            self.assertEqual(len(payload["channels"]), 1)
            self.assertEqual(payload["channels"][0]["health"], "healthy")
            self.assertEqual(payload["channels"][0]["last_samples_per_second"], 125.0)
            self.assertEqual(payload["channels"][0]["instant_samples_per_second_5s"], 61.8)
            self.assertEqual(payload["channels"][0]["rate_stability_percent_5s"], 97.4)
            self.assertEqual(payload["channels"][0]["nodes"][0]["name"], "Czujnik 1")
            self.assertEqual(payload["channels"][0]["nodes"][0]["last_temperature_c"], 24.5)
            self.assertEqual(payload["channels"][0]["nodes"][0]["instant_samples_per_second_5s"], 61.8)
            self.assertEqual(payload["channels"][0]["nodes"][0]["alerts"], [])
            self.assertEqual(len(payload["events"]), 2)
            data_payload = repository.data_payload(".")
            self.assertTrue(data_payload["exists"])
            self.assertEqual(data_payload["relative_path"], ".")

    def test_supervisor_action_writes_global_command_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            config_path = temp_dir / "system_config.json"
            runtime_dir = temp_dir / "channels"

            config_path.write_text(
                json.dumps(
                    {
                        "system": {"name": "sensor-system-prod"},
                        "supervisor": {
                            "status_file": str(temp_dir / "supervisor.status.json"),
                            "event_log": str(temp_dir / "supervisor.events.jsonl"),
                            "channel_runtime_dir": str(runtime_dir),
                        },
                        "channels": [
                            {"name": "line-a", "port": "/dev/ttyUSB0", "nodes": [{"id": 1}]}
                        ],
                    }
                ),
                encoding="utf-8",
            )

            repository = DashboardRepository(config_path)
            payload = repository.supervisor_action("restart_all")

            command_file = runtime_dir / "supervisor.command.json"
            self.assertTrue(command_file.exists())
            self.assertEqual(payload["action"], "restart_all")
            self.assertEqual(json.loads(command_file.read_text(encoding="utf-8"))["action"], "restart_all")

            payload = repository.supervisor_action("purge_all")

            self.assertTrue(command_file.exists())
            self.assertEqual(payload["action"], "purge_all")
            self.assertEqual(json.loads(command_file.read_text(encoding="utf-8"))["action"], "purge_all")

    def test_dashboard_handles_missing_runtime_files(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            config_path = temp_dir / "system_config.json"

            config_path.write_text(
                json.dumps(
                    {
                        "system": {"name": "sensor-system-prod"},
                        "supervisor": {
                            "status_file": str(temp_dir / "missing.status.json"),
                            "event_log": str(temp_dir / "missing.events.jsonl"),
                            "channel_runtime_dir": str(temp_dir / "channels"),
                        },
                        "channels": [
                            {
                                "name": "line-a",
                                "label": "Linia A",
                                "port": "/dev/ttyUSB0",
                                "nodes": [1],
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )

            repository = DashboardRepository(config_path)
            payload = repository.dashboard_payload(limit=5)

            self.assertFalse(payload["supervisor"]["has_status"])
            self.assertEqual(payload["overview"]["channels_total"], 1)
            self.assertEqual(payload["overview"]["nodes_total"], 1)
            self.assertEqual(payload["channels"][0]["health"], "waiting")
            self.assertEqual(payload["channels"][0]["nodes"][0]["alerts"], ["brak runtime"])

    def test_dashboard_data_search_and_bundle(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            status_path = temp_dir / "supervisor.status.json"
            event_path = temp_dir / "supervisor.events.jsonl"
            runs_root = temp_dir / "runs"
            (runs_root / "2026-05-12").mkdir(parents=True)
            (runs_root / "2026-05-12" / "line-a_13-00.h5").write_bytes(b"a")
            (runs_root / "2026-05-12" / "line-c_14-00.h5").write_bytes(b"c")
            config_path = temp_dir / "system_config.json"

            config_path.write_text(
                json.dumps(
                    {
                        "system": {"name": "sensor-system-prod"},
                        "storage": {"root_dir": str(runs_root)},
                        "supervisor": {
                            "status_file": str(status_path),
                            "event_log": str(event_path),
                            "channel_runtime_dir": str(temp_dir / "channels"),
                        },
                        "channels": [{"name": "line-a", "label": "Linia A", "port": "/dev/ttyUSB0", "nodes": [1]}],
                    }
                ),
                encoding="utf-8",
            )
            repository = DashboardRepository(config_path)

            payload = repository.data_search_payload("line-c 14")
            self.assertEqual([item["relative_path"] for item in payload["items"]], ["2026-05-12/line-c_14-00.h5"])

            download = repository.data_download_bundle(["2026-05-12/line-a_13-00.h5", "2026-05-12/line-c_14-00.h5"])
            try:
                with zipfile.ZipFile(download.path, mode="r") as archive:
                    self.assertEqual(
                        sorted(archive.namelist()),
                        ["2026-05-12/line-a_13-00.h5", "2026-05-12/line-c_14-00.h5"],
                    )
            finally:
                if download.cleanup_path is not None:
                    download.cleanup_path.unlink(missing_ok=True)

    def test_channel_action_writes_command_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            runtime_dir = temp_dir / "channels"
            config_path = temp_dir / "system_config.json"
            config_path.write_text(
                json.dumps(
                    {
                        "system": {"name": "sensor-system-prod"},
                        "supervisor": {
                            "status_file": str(temp_dir / "supervisor.status.json"),
                            "event_log": str(temp_dir / "supervisor.events.jsonl"),
                            "channel_runtime_dir": str(runtime_dir),
                        },
                        "channels": [{"name": "line-a", "label": "Linia A", "port": "/dev/ttyUSB0", "nodes": [1]}],
                    }
                ),
                encoding="utf-8",
            )

            repository = DashboardRepository(config_path)
            payload = repository.channel_action("line-a", "restart")
            command_file = runtime_dir / "line-a.command.json"

            self.assertTrue(payload["ok"])
            self.assertTrue(command_file.exists())
            self.assertEqual(json.loads(command_file.read_text(encoding="utf-8"))["action"], "restart")

    def test_purge_action_writes_command_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            runtime_dir = temp_dir / "channels"
            config_path = temp_dir / "system_config.json"
            config_path.write_text(
                json.dumps(
                    {
                        "system": {"name": "sensor-system-prod"},
                        "supervisor": {
                            "status_file": str(temp_dir / "supervisor.status.json"),
                            "event_log": str(temp_dir / "supervisor.events.jsonl"),
                            "channel_runtime_dir": str(runtime_dir),
                        },
                        "channels": [{"name": "line-a", "label": "Linia A", "port": "/dev/ttyUSB0", "nodes": [1]}],
                    }
                ),
                encoding="utf-8",
            )

            repository = DashboardRepository(config_path)
            payload = repository.channel_action("line-a", "purge")
            command_file = runtime_dir / "line-a.command.json"

            self.assertTrue(payload["ok"])
            self.assertTrue(command_file.exists())
            self.assertEqual(json.loads(command_file.read_text(encoding="utf-8"))["action"], "purge")

    def test_logs_payload_includes_channel_alerts_and_process_tail(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            runtime_dir = temp_dir / "channels"
            runtime_dir.mkdir(parents=True)
            config_path = temp_dir / "system_config.json"
            supervisor_status_path = temp_dir / "supervisor.status.json"
            supervisor_event_path = temp_dir / "supervisor.events.jsonl"
            channel_event_path = runtime_dir / "line-a.events.jsonl"
            channel_process_path = runtime_dir / "line-a.process.log"

            config_path.write_text(
                json.dumps(
                    {
                        "system": {"name": "sensor-system-prod"},
                        "supervisor": {
                            "status_file": str(supervisor_status_path),
                            "event_log": str(supervisor_event_path),
                            "channel_runtime_dir": str(runtime_dir),
                        },
                        "channels": [{"name": "line-a", "label": "Linia A", "port": "/dev/ttyUSB0", "nodes": [1]}],
                    }
                ),
                encoding="utf-8",
            )
            supervisor_status_path.write_text(
                json.dumps(
                    {
                        "updated_utc": "2026-06-18T11:00:00+00:00",
                        "channels": [
                            {
                                "name": "line-a",
                                "label": "Linia A",
                                "enabled": True,
                                "running": True,
                                "port": "/dev/ttyUSB0",
                                "baud": 115200,
                                "event_log": str(channel_event_path),
                                "process_log": str(channel_process_path),
                                "nodes": [{"node_id": 1, "online": True, "output_odr_hz": 62.5}],
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )
            supervisor_event_path.write_text(
                "\n".join(
                    [
                        json.dumps({"utc": "2026-06-18T11:00:01+00:00", "severity": "info", "event": "channel_started", "channel_name": "line-a"}),
                        json.dumps({"utc": "2026-06-18T11:00:02+00:00", "severity": "warning", "event": "channel_exited", "channel_name": "line-a"}),
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            channel_event_path.write_text(
                "\n".join(
                    [
                        json.dumps({"utc": "2026-06-18T11:00:03+00:00", "severity": "info", "event": "node_initialized", "node_id": 1}),
                        json.dumps({"utc": "2026-06-18T11:00:04+00:00", "severity": "warning", "event": "gap_detected", "node_id": 1, "expected_sample_seq": 12, "received_sample_seq": 16}),
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            channel_process_path.write_text(
                "\n".join(
                    [
                        "[REC] t=    8.0s samples=1000 rate=  125.0 samples/s",
                        "  node=1 written=1000 next=1001 bursts_ok=100 no_data=3 failed=0 gaps=2 sensor_loss=0 rx_ovf=0 pkt_ovf=1 totals=(0/0/1)",
                        "[WARN] packet overwrite noticed",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )

            repository = DashboardRepository(config_path)
            payload = repository.logs_payload(limit=10)

            self.assertEqual(payload["channel_filter"], None)
            self.assertEqual(len(payload["channels"]), 1)
            self.assertEqual(payload["channels"][0]["name"], "line-a")
            self.assertEqual(payload["channels"][0]["events"][0]["channel_name"], "line-a")
            self.assertEqual(payload["channels"][0]["process_lines"][-1], "[WARN] packet overwrite noticed")
            alert_names = [item["event"] for item in payload["alerts"]]
            self.assertIn("gap_detected", alert_names)
            self.assertIn("process_warning", alert_names)
            self.assertIn("process_counters_attention", alert_names)

    def test_dashboard_hides_temperature_sampled_events(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            status_path = temp_dir / "supervisor.status.json"
            event_path = temp_dir / "supervisor.events.jsonl"
            config_path = temp_dir / "system_config.json"
            config_path.write_text(
                json.dumps(
                    {
                        "system": {"name": "sensor-system-prod"},
                        "supervisor": {
                            "status_file": str(status_path),
                            "event_log": str(event_path),
                            "channel_runtime_dir": str(temp_dir / "channels"),
                        },
                        "channels": [{"name": "line-a", "label": "Linia A", "port": "/dev/ttyUSB0", "nodes": [1]}],
                    }
                ),
                encoding="utf-8",
            )
            event_path.write_text(
                "\n".join(
                    [
                        json.dumps({"utc": "2026-06-18T20:00:00+00:00", "severity": "info", "event": "temperature_sampled", "channel_name": "line-a"}),
                        json.dumps({"utc": "2026-06-18T20:00:01+00:00", "severity": "warning", "event": "gap_detected", "channel_name": "line-a"}),
                    ]
                )
                + "\n",
                encoding="utf-8",
            )

            repository = DashboardRepository(config_path)
            events_payload = repository.events_payload(limit=10)
            dashboard_payload = repository.dashboard_payload(limit=10)
            logs_payload = repository.logs_payload(limit=10)

            self.assertEqual([item["event"] for item in events_payload], ["gap_detected"])
            self.assertEqual([item["event"] for item in dashboard_payload["events"]], ["gap_detected"])
            self.assertEqual([item["event"] for item in logs_payload["supervisor_events"]], ["gap_detected"])

    def test_live_preview_reads_active_hdf5_tail(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            status_path = temp_dir / "supervisor.status.json"
            event_path = temp_dir / "supervisor.events.jsonl"
            runs_root = temp_dir / "runs"
            runs_root.mkdir(parents=True)
            hdf5_path = runs_root / "line-a_active.h5"
            self.create_live_hdf5(hdf5_path)
            config_path = temp_dir / "system_config.json"

            config_path.write_text(
                json.dumps(
                    {
                        "system": {"name": "sensor-system-prod"},
                        "storage": {"root_dir": str(runs_root)},
                        "supervisor": {
                            "status_file": str(status_path),
                            "event_log": str(event_path),
                            "channel_runtime_dir": str(temp_dir / "channels"),
                        },
                        "channels": [
                            {
                                "name": "line-a",
                                "label": "Linia A",
                                "port": "/dev/ttyUSB0",
                                "nodes": [{"id": 1, "name": "Czujnik 1", "range_g": 2}],
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )
            status_path.write_text(
                json.dumps(
                    {
                        "updated_utc": "2026-05-12T13:00:00+00:00",
                        "channels": [
                            {
                                "name": "line-a",
                                "label": "Linia A",
                                "enabled": True,
                                "running": True,
                                "port": "/dev/ttyUSB0",
                                "baud": 115200,
                                "active_file": str(hdf5_path),
                                "nodes": [
                                    {
                                        "node_id": 1,
                                        "name": "Czujnik 1",
                                        "online": True,
                                        "sensor_odr_hz": 125,
                                        "output_odr_hz": 62.5,
                                    }
                                ],
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )
            event_path.write_text("", encoding="utf-8")

            repository = DashboardRepository(config_path)
            acquired = repository.live_preview_acquire("line-a", 1, "client-a", limit=3)
            self.assertIn("token", acquired)
            snapshot = acquired["snapshot"]
            self.assertEqual(snapshot["sample_count"], 3)
            self.assertEqual(snapshot["total_samples"], 5)
            self.assertEqual(snapshot["window_start_index"], 2)
            self.assertEqual(snapshot["window_end_index"], 5)
            self.assertTrue(snapshot["is_live_tail"])
            self.assertEqual(snapshot["capture_start_utc"], "2026-05-12T12:00:00+00:00")
            self.assertEqual(snapshot["window_start_utc_estimated"], "2026-05-12T12:00:00.032000+00:00")
            self.assertEqual(snapshot["window_end_utc_estimated"], "2026-05-12T12:00:00.080000+00:00")
            self.assertEqual(snapshot["first_sample_seq"], 103)
            self.assertEqual(snapshot["last_sample_seq"], 105)
            self.assertEqual(snapshot["accel_unit"], "m/s^2")
            self.assertEqual(snapshot["x"], [0.7, 1.0, 1.3])
            self.assertGreater(snapshot["scale_abs_m_s2"], 19.0)

            history = repository.live_preview_data("line-a", 1, token=acquired["token"], limit=2, end_index=3)["snapshot"]
            self.assertEqual(history["sample_count"], 2)
            self.assertEqual(history["window_start_index"], 1)
            self.assertEqual(history["window_end_index"], 3)
            self.assertFalse(history["is_live_tail"])
            self.assertEqual(history["window_end_utc_estimated"], "2026-05-12T12:00:00.048000+00:00")
            self.assertEqual(history["x"], [0.4, 0.7])

    def test_live_preview_resolves_irregular_file_windows_by_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            status_path = temp_dir / "supervisor.status.json"
            event_path = temp_dir / "supervisor.events.jsonl"
            runs_root = temp_dir / "runs" / "line-a" / "2026-05-12"
            runs_root.mkdir(parents=True)
            early_file = runs_root / "line-a_2026-05-12_12-37-00.h5"
            later_file = runs_root / "line-a_2026-05-12_13-00-00.h5"
            self.create_live_hdf5_with_window(
                early_file,
                "2026-05-12T10:37:00+00:00",
                "2026-05-12T11:00:00+00:00",
            )
            self.create_live_hdf5_with_window(
                later_file,
                "2026-05-12T11:00:00+00:00",
                "2026-05-12T12:00:00+00:00",
            )
            config_path = temp_dir / "system_config.json"
            config_path.write_text(
                json.dumps(
                    {
                        "system": {"name": "sensor-system-prod", "timezone": "Europe/Warsaw"},
                        "storage": {"root_dir": str(temp_dir / "runs"), "window_seconds": 600},
                        "supervisor": {
                            "status_file": str(status_path),
                            "event_log": str(event_path),
                            "channel_runtime_dir": str(temp_dir / "channels"),
                        },
                        "channels": [{"name": "line-a", "label": "Linia A", "port": "/dev/ttyUSB0", "nodes": [{"id": 1, "name": "Czujnik 1"}]}],
                    }
                ),
                encoding="utf-8",
            )
            repository = DashboardRepository(config_path)

            resolved = repository._resolve_live_file_from_target_time(  # type: ignore[attr-defined]
                repository._system_config(),  # type: ignore[attr-defined]
                "line-a",
                "2026-05-12T10:50:00+00:00",
                [early_file, later_file],
            )
            self.assertEqual(resolved, early_file)

            resolved = repository._resolve_live_file_from_target_time(  # type: ignore[attr-defined]
                repository._system_config(),  # type: ignore[attr-defined]
                "line-a",
                "2026-05-12T11:20:00+00:00",
                [early_file, later_file],
            )
            self.assertEqual(resolved, later_file)


class DashboardServerRoutesTests(unittest.TestCase):
    def create_live_hdf5(self, path: Path) -> None:
        try:
            import h5py  # type: ignore
        except ImportError as exc:
            self.skipTest(f"h5py is required for live preview tests: {exc}")

        with h5py.File(path, "w") as handle:
            handle.attrs["created_utc"] = "2026-05-12T12:00:00+00:00"
            handle.attrs["window_start_utc"] = "2026-05-12T12:00:00+00:00"
            handle.attrs["window_end_utc"] = "2026-05-12T12:00:10+00:00"
            nodes = handle.require_group("nodes")
            group = nodes.require_group("1")
            group.attrs["node_id"] = 1
            group.attrs["sensor_odr_hz"] = 125
            group.attrs["output_odr_hz"] = 62.5
            group.attrs["range_g"] = 2
            group.attrs["accel_unit"] = "m/s^2"
            dataset = group.create_dataset(
                "samples",
                shape=(4,),
                dtype=[
                    ("sample_seq", "<u8"),
                    ("x", "<f4"),
                    ("y", "<f4"),
                    ("z", "<f4"),
                    ("packet_seq", "<u4"),
                ],
            )
            dataset[...] = [
                (201, 0.0, 0.1, 0.2, 1),
                (202, 0.3, 0.4, 0.5, 2),
                (203, 0.6, 0.7, 0.8, 3),
                (204, 0.9, 1.0, 1.1, 4),
            ]

    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        temp_root = Path(self.temp_dir.name)
        self.status_path = temp_root / "supervisor.status.json"
        self.event_path = temp_root / "supervisor.events.jsonl"
        self.runs_root = temp_root / "runs"
        (self.runs_root / "2026-05-12").mkdir(parents=True)
        (self.runs_root / "2026-05-12" / "line-a_13-00.h5").write_bytes(b"a")
        self.live_hdf5_path = self.runs_root / "line-a_live.h5"
        self.create_live_hdf5(self.live_hdf5_path)
        self.runtime_dir = temp_root / "channels"
        self.runtime_dir.mkdir(parents=True)
        self.channel_event_path = self.runtime_dir / "line-a.events.jsonl"
        self.channel_process_path = self.runtime_dir / "line-a.process.log"
        self.config_path = temp_root / "system_config.json"
        self.config_path.write_text(
            json.dumps(
                {
                    "system": {"name": "sensor-system-prod"},
                    "storage": {"root_dir": str(self.runs_root)},
                    "supervisor": {
                            "status_file": str(self.status_path),
                            "event_log": str(self.event_path),
                            "channel_runtime_dir": str(self.runtime_dir),
                        },
                    "channels": [{"name": "line-a", "label": "Linia A", "port": "/dev/ttyUSB0", "nodes": [{"id": 1, "name": "Czujnik 1", "range_g": 2}]}],
                }
            ),
            encoding="utf-8",
        )
        self.status_path.write_text(
            json.dumps(
                {
                    "updated_utc": "2026-05-12T14:00:00+00:00",
                    "channels": [
                        {
                            "name": "line-a",
                            "label": "Linia A",
                            "enabled": True,
                            "running": True,
                            "port": "/dev/ttyUSB0",
                            "baud": 115200,
                            "active_file": str(self.live_hdf5_path),
                            "destination": str(self.live_hdf5_path),
                            "event_log": str(self.channel_event_path),
                            "process_log": str(self.channel_process_path),
                            "nodes": [
                                {
                                    "node_id": 1,
                                    "name": "Czujnik 1",
                                    "online": True,
                                    "sensor_odr_hz": 125,
                                    "output_odr_hz": 62.5,
                                }
                            ],
                        }
                    ],
                }
            ),
            encoding="utf-8",
        )
        self.event_path.write_text(
            json.dumps({"utc": "2026-05-12T14:00:02+00:00", "severity": "warning", "event": "channel_exited", "channel_name": "line-a"}) + "\n",
            encoding="utf-8",
        )
        self.channel_event_path.write_text(
            json.dumps({"utc": "2026-05-12T14:00:03+00:00", "severity": "warning", "event": "gap_detected", "channel_name": "line-a", "node_id": 1}) + "\n",
            encoding="utf-8",
        )
        self.channel_process_path.write_text("[WARN] sample stall seen\n", encoding="utf-8")
        repository = DashboardRepository(self.config_path)
        self.server = DashboardServer(("127.0.0.1", 0), repository)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
        self.temp_dir.cleanup()

    def request(self, method: str, path: str, body: dict[str, object] | None = None) -> tuple[int, bytes, dict[str, str]]:
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=5)
        payload = None
        headers: dict[str, str] = {}
        if body is not None:
            payload = json.dumps(body)
            headers["Content-Type"] = "application/json"
        connection.request(method, path, body=payload, headers=headers)
        response = connection.getresponse()
        raw = response.read()
        response_headers = {key: value for key, value in response.getheaders()}
        connection.close()
        return response.status, raw, response_headers

    def test_dashboard_serves_data_routes(self) -> None:
        status, body, _headers = self.request("GET", "/")
        self.assertEqual(status, 200)
        self.assertIn("Sekcja data", body.decode("utf-8"))
        self.assertIn("Logi kanałów", body.decode("utf-8"))

        status, body, headers = self.request("GET", "/api/data")
        self.assertEqual(status, 200)
        self.assertEqual(headers["Content-Type"], "application/json; charset=utf-8")
        payload = json.loads(body)
        self.assertEqual(payload["items"][0]["relative_path"], "2026-05-12")

        status, body, _headers = self.request("GET", "/api/data/search?q=line-a")
        self.assertEqual(status, 200)
        payload = json.loads(body)
        self.assertEqual(payload["items"][0]["relative_path"], "2026-05-12/line-a_13-00.h5")

        status, body, headers = self.request("GET", "/api/logs?limit=10&channel=line-a")
        self.assertEqual(status, 200)
        self.assertEqual(headers["Content-Type"], "application/json; charset=utf-8")
        payload = json.loads(body)
        self.assertEqual(payload["channel_filter"], "line-a")
        self.assertEqual(payload["channels"][0]["name"], "line-a")
        self.assertIn("gap_detected", [item["event"] for item in payload["alerts"]])

        status, body, headers = self.request("GET", "/api/data/download?path=2026-05-12")
        self.assertEqual(status, 200)
        self.assertEqual(headers["Content-Type"], "application/zip")
        self.assertIn("2026-05-12.zip", headers["Content-Disposition"])
        with zipfile.ZipFile(BytesIO(body), mode="r") as archive:
            self.assertIn("2026-05-12/line-a_13-00.h5", archive.namelist())

        status, body, headers = self.request(
            "POST",
            "/api/data/download-bundle",
            {"paths": ["2026-05-12/line-a_13-00.h5"]},
        )
        self.assertEqual(status, 200)
        self.assertEqual(headers["Content-Type"], "application/x-hdf5")
        self.assertEqual(body, b"a")

    def test_dashboard_serves_live_preview_routes(self) -> None:
        status, body, _headers = self.request("GET", "/live?channel=line-a&node=1")
        self.assertEqual(status, 200)
        self.assertIn("Podgląd osi na żywo", body.decode("utf-8"))
        self.assertIn("FFT próbek", body.decode("utf-8"))
        self.assertIn("fft-chart", body.decode("utf-8"))

        status, body, _headers = self.request(
            "POST",
            "/api/live/acquire",
            {"channel_name": "line-a", "node_id": 1, "client_id": "session-a", "limit": 3},
        )
        self.assertEqual(status, 200)
        payload = json.loads(body)
        token = payload["token"]
        self.assertEqual(payload["snapshot"]["sample_count"], 3)
        self.assertEqual(payload["snapshot"]["total_samples"], 4)
        self.assertEqual(payload["snapshot"]["window_start_index"], 1)
        self.assertEqual(payload["snapshot"]["window_end_index"], 4)
        self.assertTrue(payload["snapshot"]["is_live_tail"])
        self.assertEqual(payload["snapshot"]["capture_start_utc"], "2026-05-12T12:00:00+00:00")
        self.assertEqual(payload["snapshot"]["last_sample_seq"], 204)

        status, body, _headers = self.request(
            "POST",
            "/api/live/acquire",
            {"channel_name": "line-a", "node_id": 1, "client_id": "session-b", "limit": 3},
        )
        self.assertEqual(status, 409)
        self.assertIn("aktywny podgląd", json.loads(body)["error"])

        status, body, headers = self.request(
            "GET",
            f"/api/live/data?channel=line-a&node=1&limit=3&token={token}",
        )
        self.assertEqual(status, 200)
        self.assertEqual(headers["Content-Type"], "application/json; charset=utf-8")
        payload = json.loads(body)
        self.assertEqual(payload["snapshot"]["x"], [0.3, 0.6, 0.9])

        status, body, headers = self.request(
            "GET",
            f"/api/live/data?channel=line-a&node=1&limit=2&end_index=2&token={token}",
        )
        self.assertEqual(status, 200)
        self.assertEqual(headers["Content-Type"], "application/json; charset=utf-8")
        payload = json.loads(body)
        self.assertEqual(payload["snapshot"]["x"], [0.0, 0.3])
        self.assertEqual(payload["snapshot"]["window_start_index"], 0)
        self.assertEqual(payload["snapshot"]["window_end_index"], 2)
        self.assertFalse(payload["snapshot"]["is_live_tail"])
        self.assertEqual(payload["snapshot"]["window_end_utc_estimated"], "2026-05-12T12:00:00.032000+00:00")

        status, _body, _headers = self.request("POST", "/api/live/release", {"token": token})
        self.assertEqual(status, 200)

        status, body, headers = self.request("POST", "/api/channels/line-a/restart")
        self.assertEqual(status, 200)
        self.assertEqual(headers["Content-Type"], "application/json; charset=utf-8")
        payload = json.loads(body)
        self.assertEqual(payload["action"], "restart")
        command_file = self.config_path.parent / "channels" / "line-a.command.json"
        self.assertTrue(command_file.exists())

        status, body, headers = self.request("POST", "/api/channels/line-a/purge")
        self.assertEqual(status, 200)
        self.assertEqual(headers["Content-Type"], "application/json; charset=utf-8")
        payload = json.loads(body)
        self.assertEqual(payload["action"], "purge")
        command_file = self.config_path.parent / "channels" / "line-a.command.json"
        self.assertTrue(command_file.exists())

        status, body, headers = self.request("POST", "/api/supervisor/restart_all")
        self.assertEqual(status, 200)
        self.assertEqual(headers["Content-Type"], "application/json; charset=utf-8")
        payload = json.loads(body)
        self.assertEqual(payload["action"], "restart_all")
        command_file = self.config_path.parent / "channels" / "supervisor.command.json"
        self.assertTrue(command_file.exists())

        status, body, headers = self.request("POST", "/api/supervisor/purge_all")
        self.assertEqual(status, 200)
        self.assertEqual(headers["Content-Type"], "application/json; charset=utf-8")
        payload = json.loads(body)
        self.assertEqual(payload["action"], "purge_all")
        command_file = self.config_path.parent / "channels" / "supervisor.command.json"
        self.assertTrue(command_file.exists())


if __name__ == "__main__":
    unittest.main()
