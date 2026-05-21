from __future__ import annotations

import http.client
import json
import tarfile
import tempfile
import threading
import unittest
from io import BytesIO
from pathlib import Path

from host.operator_panel.app import (
    OperatorApplication,
    OperatorServer,
    RunsRepository,
    SupervisorControlState,
    SystemConfigStore,
)


class FakeSupervisorController:
    def __init__(self) -> None:
        self.actions: list[str] = []
        self.state = SupervisorControlState(
            available=True,
            controller="fake",
            service_name="sensor-system-supervisor.service",
            active=True,
            substate="running",
            unit_file_state="enabled",
            description="fake",
            pid=4321,
        )

    def status(self) -> SupervisorControlState:
        return self.state

    def perform(self, action: str) -> SupervisorControlState:
        self.actions.append(action)
        if action == "stop":
            self.state = SupervisorControlState(**{**self.state.__dict__, "active": False, "substate": "dead"})
        else:
            self.state = SupervisorControlState(**{**self.state.__dict__, "active": True, "substate": "running"})
        return self.state


class FakePreviewReader:
    def read(self, channel_name: str, node_id: int, limit: int) -> dict[str, object]:
        return {
            "channel_name": channel_name,
            "node_id": node_id,
            "sample_count": limit,
            "samples_total": limit,
            "source_file": "/tmp/fake.h5",
            "output_odr_hz": 62.5,
            "range_g": 2,
            "samples": [
                {"sample_seq": 1, "x": 0.1, "y": 0.2, "z": 0.3, "packet_seq": 1},
                {"sample_seq": 2, "x": 0.4, "y": 0.5, "z": 0.6, "packet_seq": 1},
            ],
        }


class FakeNodeService:
    def __init__(self) -> None:
        self.calls: list[tuple[str, str, int]] = []

    def read_config(self, channel_name: str, node_id: int) -> dict[str, object]:
        self.calls.append(("read", channel_name, node_id))
        return {
            "channel_name": channel_name,
            "node_id": node_id,
            "sensor_odr_hz": 125,
            "output_odr_hz": 62.5,
            "range_g": 2,
            "high_pass_corner": 0,
            "fifo_watermark": 30,
            "offset_x": 0,
            "offset_y": 0,
            "offset_z": 0,
            "baudrate": 115200,
            "act_threshold": 0,
            "act_count": 0,
        }

    def apply_config(self, channel_name: str, node_id: int, payload: dict[str, object]) -> dict[str, object]:
        self.calls.append(("apply", channel_name, node_id))
        return {
            "persisted": True,
            "config": {
                **self.read_config(channel_name, node_id),
                **payload,
            },
        }

    def load_config(self, channel_name: str, node_id: int) -> dict[str, object]:
        self.calls.append(("load", channel_name, node_id))
        return self.read_config(channel_name, node_id)

    def reset_defaults(self, channel_name: str, node_id: int) -> dict[str, object]:
        self.calls.append(("reset", channel_name, node_id))
        payload = self.read_config(channel_name, node_id)
        payload["high_pass_corner"] = 0
        return payload

    def save_config(self, channel_name: str, node_id: int) -> dict[str, object]:
        self.calls.append(("save", channel_name, node_id))
        return self.read_config(channel_name, node_id)


class OperatorPanelRoutesTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        temp_root = Path(self.temp_dir.name)
        self.status_path = temp_root / "supervisor.status.json"
        self.event_path = temp_root / "supervisor.events.jsonl"
        self.process_log_path = temp_root / "channels" / "line-a.process.log"
        self.process_log_path.parent.mkdir()
        self.process_log_path.write_text("line-a started\ncapture ok\n", encoding="utf-8")
        self.runs_root = temp_root / "runs"
        self.runs_root.mkdir()
        (self.runs_root / "2026-05-12").mkdir()
        (self.runs_root / "2026-05-12" / "capture.h5").write_bytes(b"test-data")
        self.config_path = temp_root / "system_config.json"
        self.config_path.write_text(
            json.dumps(
                {
                    "system": {
                        "name": "sensor-system-prod",
                        "site": "hala-a",
                        "timezone": "Europe/Warsaw",
                    },
                    "storage": {
                        "root_dir": str(self.runs_root),
                        "format": "hdf5",
                        "compression": "gzip",
                        "window_seconds": 600,
                    },
                    "supervisor": {
                        "status_file": str(self.status_path),
                        "event_log": str(self.event_path),
                        "channel_runtime_dir": str(temp_root / "channels"),
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
        self.status_path.write_text(
            json.dumps(
                {
                    "updated_utc": "2026-05-12T10:00:00+00:00",
                    "started_utc": "2026-05-12T09:00:00+00:00",
                    "supervisor_version": "0.3.0",
                    "storage_root": str(self.runs_root),
                    "channels": [
                        {
                            "name": "line-a",
                            "label": "Linia A",
                            "enabled": True,
                            "port": "/dev/ttyUSB0",
                            "baud": 115200,
                            "process_id": 1234,
                            "running": True,
                            "restart_count": 1,
                            "last_exit_code": None,
                            "updated_utc": "2026-05-12T10:00:00+00:00",
                            "destination": str(self.runs_root / "2026-05-12" / "capture.h5"),
                            "active_file": str(self.runs_root / "2026-05-12" / "capture.h5"),
                            "status_file": str(temp_root / "channels" / "line-a.status.json"),
                            "event_log": str(temp_root / "channels" / "line-a.events.jsonl"),
                            "process_log": str(self.process_log_path),
                            "nodes": [
                                {
                                    "node_id": 1,
                                    "name": "Czujnik 1",
                                    "online": True,
                                    "sensor_odr_hz": 125,
                                    "output_odr_hz": 62.5,
                                    "samples_written": 1234,
                                    "expected_sample_seq": 1235,
                                    "last_written_seq": 1234,
                                    "bursts_ok": 1,
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
                                    "last_temperature_c": 23.5,
                                    "last_temperature_unix_ns": 1_714_382_800_000_000_000,
                                }
                            ],
                        }
                    ],
                }
            ),
            encoding="utf-8",
        )
        self.event_path.write_text(
            json.dumps({"utc": "2026-05-12T10:00:01+00:00", "severity": "info", "event": "channel_started"}) + "\n",
            encoding="utf-8",
        )

        self.controller = FakeSupervisorController()
        self.node_service = FakeNodeService()
        config_store = SystemConfigStore(self.config_path)
        app = OperatorApplication(
            self.config_path,
            self.controller,
            config_store=config_store,
            runs_repository=RunsRepository(config_store),
            preview_reader=FakePreviewReader(),
            node_service=self.node_service,
        )
        self.server = OperatorServer(("127.0.0.1", 0), app)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def set_channel_running(self, running: bool) -> None:
        payload = json.loads(self.status_path.read_text(encoding="utf-8"))
        payload["channels"][0]["running"] = running
        payload["channels"][0]["process_id"] = 1234 if running else None
        self.status_path.write_text(json.dumps(payload), encoding="utf-8")

    def tearDown(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
        self.temp_dir.cleanup()

    def request(
        self,
        method: str,
        path: str,
        body: dict[str, object] | None = None,
    ) -> tuple[int, dict[str, object]]:
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=5)
        payload = None
        headers: dict[str, str] = {}
        if body is not None:
            payload = json.dumps(body)
            headers["Content-Type"] = "application/json"
        connection.request(method, path, body=payload, headers=headers)
        response = connection.getresponse()
        raw = response.read().decode("utf-8")
        connection.close()
        return response.status, json.loads(raw)

    def request_text(self, method: str, path: str) -> tuple[int, str]:
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=5)
        connection.request(method, path)
        response = connection.getresponse()
        raw = response.read().decode("utf-8")
        connection.close()
        return response.status, raw

    def request_binary(self, method: str, path: str) -> tuple[int, bytes, dict[str, str]]:
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=5)
        connection.request(method, path)
        response = connection.getresponse()
        raw = response.read()
        headers = {key: value for key, value in response.getheaders()}
        connection.close()
        return response.status, raw, headers

    def test_routes_cover_supervisor_config_runs_preview_and_device_actions(self) -> None:
        self.set_channel_running(False)

        status, html = self.request_text("GET", "/")
        self.assertEqual(status, 200)
        self.assertIn("<h1>Panel</h1>", html)

        status, payload = self.request("GET", "/api/meta")
        self.assertEqual(status, 200)
        self.assertEqual(payload["supervisor"]["controller"], "fake")
        self.assertEqual([page["label"] for page in payload["pages"]], ["Przeglad", "Logi", "Runs"])

        status, payload = self.request("GET", "/api/system-config")
        self.assertEqual(status, 200)
        self.assertEqual(payload["config"]["system"]["site"], "hala-a")

        status, payload = self.request(
            "PUT",
            "/api/system-config",
            {
                "config": {
                    **payload["config"],
                    "system": {**payload["config"]["system"], "site": "hala-b"},
                }
            },
        )
        self.assertEqual(status, 200)
        self.assertEqual(payload["config"]["system"]["site"], "hala-b")

        status, payload = self.request("POST", "/api/supervisor/restart")
        self.assertEqual(status, 200)
        self.assertTrue(payload["active"])
        self.assertTrue(payload["effective_active"])
        self.assertEqual(self.controller.actions, ["restart"])

        status, payload = self.request("GET", "/api/logs?limit=50&channel=line-a")
        self.assertEqual(status, 200)
        self.assertEqual(payload["channels"][0]["name"], "line-a")
        self.assertIn("capture ok", payload["channels"][0]["lines"][-1])

        status, payload = self.request("GET", "/api/runs")
        self.assertEqual(status, 200)
        self.assertEqual(payload["items"][0]["type"], "directory")
        self.assertIsNotNone(payload["items"][0]["download_url"])

        status, archive_bytes, headers = self.request_binary("GET", "/api/runs/download?path=2026-05-12")
        self.assertEqual(status, 200)
        self.assertEqual(headers["Content-Type"], "application/gzip")
        self.assertIn("2026-05-12.tar.gz", headers["Content-Disposition"])
        with tarfile.open(fileobj=BytesIO(archive_bytes), mode="r:gz") as archive:
            names = archive.getnames()
        self.assertIn("2026-05-12/capture.h5", names)

        status, payload = self.request("GET", "/api/channels/line-a/nodes/1/device-config")
        self.assertEqual(status, 200)
        self.assertEqual(payload["sensor_odr_hz"], 125)

        status, payload = self.request(
            "POST",
            "/api/channels/line-a/nodes/1/device-config/apply",
            {"sensor_odr_hz": 250, "persist": True},
        )
        self.assertEqual(status, 200)
        self.assertEqual(payload["config"]["sensor_odr_hz"], 250)

        status, payload = self.request("GET", "/api/channels/line-a/nodes/1/preview?limit=128")
        self.assertEqual(status, 200)
        self.assertEqual(payload["sample_count"], 128)

    def test_device_config_is_blocked_while_channel_is_recording(self) -> None:
        self.set_channel_running(True)

        status, payload = self.request("GET", "/api/channels/line-a/nodes/1/device-config")
        self.assertEqual(status, 409)
        self.assertIn("is recording", payload["error"])

        status, payload = self.request(
            "POST",
            "/api/channels/line-a/nodes/1/device-config/apply",
            {"sensor_odr_hz": 250},
        )
        self.assertEqual(status, 409)
        self.assertIn("is recording", payload["error"])
        self.assertEqual(self.node_service.calls, [])

    def test_runs_repository_blocks_path_escape(self) -> None:
        status, payload = self.request("GET", "/api/runs?path=../secret")
        self.assertEqual(status, 400)
        self.assertIn("escapes runs root", payload["error"])


if __name__ == "__main__":
    unittest.main()
