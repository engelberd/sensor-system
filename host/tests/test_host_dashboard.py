from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from host.dashboard.app import DashboardRepository


class DashboardRepositoryTests(unittest.TestCase):
    def test_dashboard_merges_config_runtime_and_events(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            status_path = temp_dir / "supervisor.status.json"
            event_path = temp_dir / "supervisor.events.jsonl"
            config_path = temp_dir / "system_config.json"

            config_path.write_text(
                json.dumps(
                    {
                        "system": {
                            "name": "sensor-system-prod",
                            "site": "hala-a",
                            "timezone": "Europe/Warsaw",
                        },
                        "storage": {
                            "root_dir": "runs/sensor-system",
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
                                "nodes": [
                                    {
                                        "node_id": 1,
                                        "name": "Czujnik 1",
                                        "online": True,
                                        "sensor_odr_hz": 125,
                                        "output_odr_hz": 62.5,
                                        "samples_written": 1200,
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
            self.assertEqual(payload["channels"][0]["nodes"][0]["name"], "Czujnik 1")
            self.assertEqual(payload["channels"][0]["nodes"][0]["last_temperature_c"], 24.5)
            self.assertEqual(payload["channels"][0]["nodes"][0]["alerts"], [])
            self.assertEqual(len(payload["events"]), 2)

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


if __name__ == "__main__":
    unittest.main()
