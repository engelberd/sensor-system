from __future__ import annotations

import http.client
import json
import tempfile
import threading
import unittest
from datetime import datetime, timezone
from pathlib import Path
from unittest.mock import patch

from host.common.host_metrics import HostMetrics
from host.dashboard.app import DashboardServer
from host.dashboard.v2 import V2DashboardRepository, is_v2_profile


class V2DashboardRepositoryTests(unittest.TestCase):
    def _fixture(self, temp_dir: Path, *, updated_unix_ms: int | None = None) -> tuple[Path, Path]:
        common = temp_dir / "common.json"
        config = temp_dir / "system.json"
        status = temp_dir / "status.json"
        common.write_text(
            json.dumps(
                {
                    "storage": {"root_dir": "var/recordings-v2"},
                    "channels": [],
                }
            ),
            encoding="utf-8",
        )
        config.write_text(
            json.dumps(
                {
                    "extends": "common.json",
                    "system": {"name": "test-v2", "site": "lab"},
                    "channels": [
                        {
                            "name": "line-b",
                            "channel_id": 2,
                            "label": "Linia B",
                            "enabled": True,
                            "transport": {
                                "kind": "serial-rs485",
                                "endpoint": "/dev/line-b",
                                "baud_rate": 115200,
                            },
                            "nodes": [
                                {
                                    "id": 1,
                                    "hardware_id": "0102030405060708",
                                    "enabled": True,
                                    "output_rate_hz": 125,
                                }
                            ],
                        },
                        {
                            "name": "line-a",
                            "channel_id": 1,
                            "label": "Linia A",
                            "enabled": False,
                            "transport": {
                                "kind": "serial-rs485",
                                "endpoint": "/dev/line-a",
                                "baud_rate": 115200,
                            },
                            "nodes": [],
                        },
                    ],
                }
            ),
            encoding="utf-8",
        )
        now_ms = updated_unix_ms
        if now_ms is None:
            now_ms = int(datetime.now(timezone.utc).timestamp() * 1000)
        status.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "sequence": 7,
                    "system_name": "test-v2",
                    "software": {"host_version": "0.5.0-dev"},
                    "process": {
                        "phase": "recording",
                        "pid": 1234,
                        "instance_id": "test-instance",
                        "uptime_ms": 10_000,
                        "started_unix_ms": now_ms - 10_000,
                        "updated_unix_ms": now_ms,
                    },
                    "health": {"level": "healthy", "reasons": []},
                    "storage": {
                        "root": str(temp_dir / "recordings"),
                        "available_bytes": 123456,
                        "state": "healthy",
                    },
                    "channels": [
                        {
                            "channel_id": 2,
                            "channel_name": "line-b",
                            "phase": "running",
                            "metrics": {
                                "decode_errors": 0,
                                "measurement_no_data": 2,
                                "transaction_timeouts": 0,
                            },
                        }
                    ],
                    "nodes": [
                        {
                            "channel_id": 2,
                            "channel_name": "line-b",
                            "node_address": 1,
                            "hardware_id": "0102030405060708",
                            "firmware": "0.5.7",
                            "board_revision": 2,
                            "boot_epoch": 99,
                            "application_protocol": 5,
                            "framing_protocol": 3,
                            "output_rate_hz": "125",
                            "last_measurement_age_ms": 40,
                            "last_time_sync_age_ms": 900,
                            "cursors": {
                                "received": 1040,
                                "durable": 1040,
                                "released": 1000,
                                "produced": 1040,
                            },
                        }
                    ],
                }
            ),
            encoding="utf-8",
        )
        return config, status

    @patch.object(V2DashboardRepository, "_journal_entries", return_value=[])
    def test_maps_runtime_status_without_opening_runtime_io(self, _journal: object) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            config, status = self._fixture(Path(tmp))
            self.assertTrue(is_v2_profile(config))
            repository = V2DashboardRepository(config, status_path=status)
            with (
                patch.object(
                    repository.host_metrics_sampler,
                    "sample",
                    return_value=HostMetrics(cpu_used_percent=18.0, memory_used_percent=42.0),
                ),
                patch.object(
                    repository,
                    "_operations_status",
                    return_value={"ok": True, "mode": "recording", "active_operation_id": None},
                ),
            ):
                payload = repository.dashboard_payload()

            self.assertTrue(payload["read_only"])
            self.assertFalse(payload["data_available"])
            self.assertFalse(payload["logs_available"])
            self.assertFalse(payload["events_available"])
            self.assertEqual(payload["overview"]["channels_running"], 1)
            self.assertEqual(payload["overview"]["nodes_online"], 1)
            self.assertEqual(payload["supervisor"]["cpu_used_percent"], 18.0)
            self.assertEqual(payload["supervisor"]["memory_used_percent"], 42.0)
            channel = payload["channels"][0]
            self.assertTrue(channel["controls_enabled"])
            self.assertEqual(channel["phase"], "running")
            self.assertEqual(channel["operating_mode"], "recording")
            self.assertTrue(channel["runtime_v2"])
            self.assertNotIn("active_file", channel)
            self.assertNotIn("restart_count", channel)
            node = channel["nodes"][0]
            self.assertEqual(node["firmware_version"], "0.5.7")
            self.assertEqual(node["durable_cursor"], 1040)
            self.assertEqual(node["released_cursor"], 1000)
            self.assertEqual(node["last_measurement_age_ms"], 40)
            self.assertEqual(node["release_backlog"], 40)
            self.assertEqual(node["board_revision"], 2)
            disabled = payload["channels"][1]
            self.assertFalse(disabled["enabled"])
            self.assertFalse(disabled["controls_enabled"])
            self.assertIsNone(disabled["operating_mode"])

    @patch.object(V2DashboardRepository, "_journal_entries", return_value=[])
    def test_stale_status_marks_channel_offline(self, _journal: object) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            old_ms = int(datetime.now(timezone.utc).timestamp() * 1000) - 120_000
            config, status = self._fixture(Path(tmp), updated_unix_ms=old_ms)
            repository = V2DashboardRepository(config, status_path=status)

            payload = repository.dashboard_payload()

            self.assertTrue(payload["overview"]["status_stale"])
            self.assertEqual(payload["overview"]["channels_running"], 0)
            self.assertEqual(payload["overview"]["nodes_online"], 0)

    @patch.object(V2DashboardRepository, "_journal_entries", return_value=[])
    def test_http_server_rejects_post_in_v2_mode(self, _journal: object) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            config, status = self._fixture(Path(tmp))
            repository = V2DashboardRepository(config, status_path=status)
            server = DashboardServer(("127.0.0.1", 0), repository)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                connection = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=2)
                connection.request("POST", "/api/supervisor/restart_all", body=b"{}")
                response = connection.getresponse()
                body = json.loads(response.read())
                connection.close()
                self.assertEqual(response.status, 405)
                self.assertEqual(body["error"], "host v2 dashboard is read-only")
                connection = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=2)
                connection.request("GET", "/api/data")
                response = connection.getresponse()
                body = json.loads(response.read())
                connection.close()
                self.assertEqual(response.status, 404)
                self.assertEqual(body["error"], "data files are not exposed by the host v2 dashboard")
            finally:
                server.shutdown()
                server.server_close()
                thread.join(timeout=2)


if __name__ == "__main__":
    unittest.main()
