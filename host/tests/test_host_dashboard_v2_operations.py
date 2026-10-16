from __future__ import annotations

import http.client
import json
import socket
import tempfile
import threading
import unittest
from pathlib import Path
from typing import Any

from host.dashboard.app import DashboardServer
from host.dashboard.v2 import V2DashboardRepository
from host.dashboard.v2_operations import OperationsIpcClient


class FakeOperationsClient:
    def __init__(self) -> None:
        self.submitted: list[dict[str, Any]] = []

    def status(self, channel_id: int) -> dict[str, Any]:
        return {
            "schema_version": 1,
            "ok": True,
            "channel_id": channel_id,
            "mode": "recording",
            "active_operation_id": None,
        }

    def get(self, channel_id: int, operation_id: int) -> dict[str, Any]:
        request = next(
            (value for value in self.submitted if value["operation_id"] == operation_id),
            None,
        )
        result = None
        if request is not None and request.get("operation") == "assign_address":
            result = {
                "kind": "address_assigned",
                "hardware_id": request["hardware_id"],
                "node_address": request["new_node_address"],
            }
        return {
            "schema_version": 1,
            "ok": True,
            "channel_id": channel_id,
            "operation_id": operation_id,
            "phase": "succeeded",
            "mode": "maintenance",
            "result": result,
            "error": None,
        }

    def submit(self, request: dict[str, Any]) -> dict[str, Any]:
        self.submitted.append(dict(request))
        return {
            "schema_version": 1,
            "ok": True,
            "channel_id": request["channel_id"],
            "operation_id": request["operation_id"],
            "phase": "queued",
        }


class FakeServiceControlClient:
    def __init__(self, available: bool = False) -> None:
        self.available = available
        self.actions: list[str] = []

    def status(self) -> dict[str, Any]:
        return {
            "schema_version": 1,
            "ok": True,
            "state": {"available": self.available, "active": True},
        }

    def perform(self, action: str) -> dict[str, Any]:
        self.actions.append(action)
        return {
            "schema_version": 1,
            "ok": True,
            "state": {"available": True, "active": action != "stop"},
        }

class V2OperatorApiTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        self.config = root / "system.json"
        self.config.write_text(
            json.dumps(
                {
                    "system": {"name": "operator-test"},
                    "storage": {"root_dir": "var/recordings"},
                    "channels": [
                        {
                            "name": "line-b",
                            "channel_id": 2,
                            "enabled": True,
                            "transport": {"kind": "serial-rs485"},
                            "nodes": [{"id": 1, "enabled": True}],
                        }
                    ],
                }
            ),
            encoding="utf-8",
        )
        self.operations = FakeOperationsClient()
        self.repository = V2DashboardRepository(
            self.config,
            status_path=root / "missing-status.json",
            operations_client=self.operations,
            service_control_client=FakeServiceControlClient(),
        )

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_repository_exposes_complete_single_target_operations(self) -> None:
        capabilities = self.repository.operator_capabilities_payload()
        self.assertIn("enter_maintenance", capabilities["channel_operations"])
        self.assertIn("apply_configuration", capabilities["node_operations"])
        self.assertFalse(capabilities["service_control"])
        self.assertEqual(
            capabilities["commissioning_workflow"]["scan_slot_count"], 16
        )

        channel = self.repository.dashboard_payload()["channels"][0]
        self.assertTrue(channel["runtime_v2"])
        self.assertTrue(channel["controls_enabled"])
        self.assertEqual(channel["operation_mode"], "recording")

        response = self.repository.submit_operation_payload(
            2,
            {"operation": "restart_node", "node_address": 1, "operation_id": 77},
        )

        self.assertEqual(response["phase"], "queued")
        self.assertEqual(
            self.operations.submitted,
            [
                {
                    "channel_id": 2,
                    "operation_id": 77,
                    "operation": "restart_node",
                    "node_address": 1,
                }
            ],
        )

    def test_dashboard_explains_and_exposes_commissioning_scan_state(self) -> None:
        server = DashboardServer(("127.0.0.1", 0), self.repository)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            connection = http.client.HTTPConnection(
                "127.0.0.1", server.server_port, timeout=2
            )
            connection.request("GET", "/")
            response = connection.getresponse()
            html = response.read().decode("utf-8")
            self.assertEqual(response.status, 200)
            self.assertIn('id="commission-scan-stage"', html)
            self.assertIn('id="commission-details-stage" hidden', html)
            self.assertIn('id="config-load-stage" hidden', html)
            self.assertIn('id="config-fields"', html)
            self.assertIn('$("node-config-dialog").showModal();', html)
            self.assertIn("16 szczelin odpowiedzi", html)
            self.assertIn("Rozpocząć skan teraz?", html)
            self.assertIn("przerwie nagrywanie na wszystkich aktywnych kanałach", html)
            connection.close()
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)

    def test_unix_datagram_client_exchanges_bounded_json(self) -> None:
        socket_path = Path(self.temp.name) / "operations.sock"
        server = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
        server.bind(str(socket_path))

        def reply() -> None:
            request, peer = server.recvfrom(4097)
            document = json.loads(request)
            response = {
                "schema_version": 1,
                "ok": True,
                "channel_id": document["channel_id"],
                "mode": "recording",
                "active_operation_id": None,
            }
            server.sendto(json.dumps(response).encode("utf-8"), peer)

        thread = threading.Thread(target=reply, daemon=True)
        thread.start()
        try:
            response = OperationsIpcClient(socket_path).status(2)
            self.assertTrue(response["ok"])
            self.assertEqual(response["mode"], "recording")
        finally:
            thread.join(timeout=2)
            server.close()

    def test_repository_rejects_unknown_channel_and_missing_fields(self) -> None:
        with self.assertRaisesRegex(ValueError, "not enabled"):
            self.repository.submit_operation_payload(
                3, {"operation": "enter_maintenance"}
            )
        with self.assertRaisesRegex(ValueError, "node_address is required"):
            self.repository.submit_operation_payload(2, {"operation": "restart_node"})

    def test_http_submit_and_poll_use_same_operations_client(self) -> None:
        server = DashboardServer(("127.0.0.1", 0), self.repository)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            connection = http.client.HTTPConnection(
                "127.0.0.1", server.server_port, timeout=2
            )
            body = json.dumps(
                {
                    "operation": "enter_maintenance",
                    "operation_id": 91,
                }
            )
            connection.request(
                "POST",
                "/api/v2/channels/2/operations",
                body=body,
                headers={
                    "Content-Type": "application/json",
                    "X-Sensor-System-Request": "dashboard-v2",
                },
            )
            response = connection.getresponse()
            submitted = json.loads(response.read())
            self.assertEqual(response.status, 202)
            self.assertEqual(submitted["operation_id"], 91)

            connection.request("GET", "/api/v2/channels/2/operations/91")
            response = connection.getresponse()
            completed = json.loads(response.read())
            self.assertEqual(response.status, 200)
            self.assertEqual(completed["phase"], "succeeded")
            connection.close()
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)

    def test_http_mutation_requires_request_marker(self) -> None:
        server = DashboardServer(("127.0.0.1", 0), self.repository)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            connection = http.client.HTTPConnection(
                "127.0.0.1", server.server_port, timeout=2
            )
            connection.request(
                "POST",
                "/api/v2/channels/2/operations",
                body=json.dumps({"operation": "enter_maintenance"}),
                headers={"Content-Type": "application/json"},
            )
            response = connection.getresponse()
            self.assertEqual(response.status, 403)
            response.read()
            connection.close()
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)

    def test_http_service_control_uses_separate_allowlisted_client(self) -> None:
        service = FakeServiceControlClient(available=True)
        self.repository.service_control = service
        server = DashboardServer(("127.0.0.1", 0), self.repository)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            connection = http.client.HTTPConnection(
                "127.0.0.1", server.server_port, timeout=2
            )
            connection.request(
                "POST",
                "/api/v2/service/restart",
                headers={"X-Sensor-System-Request": "dashboard-v2"},
            )
            response = connection.getresponse()
            payload = json.loads(response.read())
            self.assertEqual(response.status, 200)
            self.assertTrue(payload["state"]["active"])
            self.assertEqual(service.actions, ["restart"])
            connection.close()
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)

    def test_http_registers_commissioned_node_and_restarts_host(self) -> None:
        service = FakeServiceControlClient(available=True)
        self.repository.service_control = service
        server = DashboardServer(("127.0.0.1", 0), self.repository)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            connection = http.client.HTTPConnection(
                "127.0.0.1", server.server_port, timeout=2
            )
            connection.request(
                "POST",
                "/api/v2/channels/2/commission",
                body=json.dumps(
                    {
                        "name": "North bearing",
                        "node_address": 2,
                        "hardware_id": "aabbccddeeff0011",
                        "board_revision": 2,
                        "round_nonce": 123,
                    }
                ),
                headers={
                    "Content-Type": "application/json",
                    "X-Sensor-System-Request": "dashboard-v2",
                },
            )
            response = connection.getresponse()
            payload = json.loads(response.read())
            self.assertEqual(response.status, 200)
            self.assertTrue(payload["ok"])
            self.assertEqual(service.actions, ["restart"])
            saved = json.loads(self.config.read_text(encoding="utf-8"))
            self.assertEqual(saved["channels"][0]["nodes"][1]["name"], "North bearing")
            connection.close()
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)


if __name__ == "__main__":
    unittest.main()
