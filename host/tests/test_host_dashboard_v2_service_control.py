from __future__ import annotations

import json
import socket
import subprocess
import threading
import unittest
from typing import Any

from host.dashboard.v2_service_control import (
    ServiceControlServer,
    SystemdServiceController,
)


class FakeSystemdController(SystemdServiceController):
    def __init__(self) -> None:
        super().__init__("sensor-host-v2.service", "sensor-analysis-export-v2.timer")
        self.commands: list[tuple[str, ...]] = []
        self.active = False

    def _systemctl(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        self.commands.append(arguments)
        if arguments[0] == "show":
            unit = arguments[-1]
            active = self.active if unit == self.host_unit else self.active
            return subprocess.CompletedProcess(
                ["systemctl", *arguments],
                0,
                stdout=(
                    "LoadState=loaded\n"
                    f"ActiveState={'active' if active else 'inactive'}\n"
                    f"SubState={'running' if active else 'dead'}\n"
                    "UnitFileState=enabled\n"
                    f"ExecMainPID={123 if active and unit == self.host_unit else 0}\n"
                ),
                stderr="",
            )
        if arguments[0] in {"start", "restart"} and arguments[-1] == self.host_unit:
            self.active = True
        if arguments[0] == "stop" and arguments[-1] == self.host_unit:
            self.active = False
        return subprocess.CompletedProcess(["systemctl", *arguments], 0, stdout="", stderr="")


class ServiceControlTests(unittest.TestCase):
    def test_controller_uses_fixed_order_for_host_and_export_timer(self) -> None:
        controller = FakeSystemdController()
        state = controller.perform("start")
        self.assertTrue(state["active"])
        self.assertEqual(
            controller.commands[:2],
            [
                ("start", "sensor-host-v2.service"),
                ("start", "sensor-analysis-export-v2.timer"),
            ],
        )

        controller.commands.clear()
        state = controller.perform("stop")
        self.assertFalse(state["active"])
        self.assertEqual(
            controller.commands[:2],
            [
                ("stop", "sensor-analysis-export-v2.timer"),
                ("stop", "sensor-host-v2.service"),
            ],
        )

    def test_server_rejects_everything_outside_the_action_allowlist(self) -> None:
        controller = FakeSystemdController()
        server = ServiceControlServer("/unused", controller)
        client, accepted = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)

        def handle() -> None:
            with accepted:
                server._handle(accepted)

        thread = threading.Thread(target=handle)
        thread.start()
        with client:
            client.sendall(json.dumps({"schema_version": 1, "action": "enable"}).encode())
            client.shutdown(socket.SHUT_WR)
            response: dict[str, Any] = json.loads(client.recv(2048))
        thread.join(timeout=2)
        self.assertFalse(response["ok"])
        self.assertEqual(controller.commands, [])


if __name__ == "__main__":
    unittest.main()
