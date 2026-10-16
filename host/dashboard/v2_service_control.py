from __future__ import annotations

import json
import os
import socket
import subprocess
import time
from pathlib import Path
from typing import Any


SERVICE_CONTROL_SCHEMA_VERSION = 1
DEFAULT_SERVICE_CONTROL_SOCKET = Path("/run/sensor-system-control/service-control-v1.sock")
MAX_SERVICE_CONTROL_BYTES = 2048


class ServiceControlError(RuntimeError):
    pass


class ServiceControlClient:
    def __init__(self, socket_path: str | Path = DEFAULT_SERVICE_CONTROL_SOCKET, timeout_s: float = 130.0) -> None:
        self.socket_path = Path(socket_path)
        self.timeout_s = timeout_s

    def status(self) -> dict[str, Any]:
        return self._request({"schema_version": 1, "action": "status"}, timeout_s=2.0)

    def perform(self, action: str) -> dict[str, Any]:
        if action not in {"start", "stop", "restart"}:
            raise ValueError("unsupported service action")
        return self._request({"schema_version": 1, "action": action}, timeout_s=self.timeout_s)

    def _request(self, document: dict[str, Any], *, timeout_s: float) -> dict[str, Any]:
        encoded = json.dumps(document, separators=(",", ":")).encode("utf-8")
        if len(encoded) > MAX_SERVICE_CONTROL_BYTES:
            raise ServiceControlError("service control request is too large")
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
                client.settimeout(timeout_s)
                client.connect(str(self.socket_path))
                client.sendall(encoded)
                client.shutdown(socket.SHUT_WR)
                chunks: list[bytes] = []
                total = 0
                while True:
                    chunk = client.recv(min(1024, MAX_SERVICE_CONTROL_BYTES + 1 - total))
                    if not chunk:
                        break
                    chunks.append(chunk)
                    total += len(chunk)
                    if total > MAX_SERVICE_CONTROL_BYTES:
                        raise ServiceControlError("service control response is too large")
        except (OSError, TimeoutError) as exc:
            raise ServiceControlError(f"service control unavailable: {exc}") from exc
        try:
            response = json.loads(b"".join(chunks))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise ServiceControlError("service control returned invalid JSON") from exc
        if not isinstance(response, dict) or response.get("schema_version") != 1:
            raise ServiceControlError("service control returned an unsupported response")
        if response.get("ok") is not True:
            error = response.get("error")
            message = error.get("message") if isinstance(error, dict) else None
            raise ServiceControlError(str(message or "service control rejected the request"))
        return response


class SystemdServiceController:
    def __init__(
        self,
        host_unit: str = "sensor-host-v2.service",
        analysis_timer: str = "sensor-analysis-export-v2.timer",
        timeout_s: float = 125.0,
    ) -> None:
        self.host_unit = host_unit
        self.analysis_timer = analysis_timer
        self.timeout_s = timeout_s

    def _systemctl(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        try:
            return subprocess.run(
                ["systemctl", *arguments],
                check=False,
                capture_output=True,
                text=True,
                timeout=self.timeout_s,
            )
        except (OSError, subprocess.TimeoutExpired) as exc:
            raise ServiceControlError(f"systemctl failed: {exc}") from exc

    def _unit_state(self, unit: str) -> dict[str, Any]:
        result = self._systemctl(
            "show",
            "--property=LoadState,ActiveState,SubState,UnitFileState,ExecMainPID",
            unit,
        )
        if result.returncode != 0:
            raise ServiceControlError((result.stderr or result.stdout or "systemctl show failed").strip())
        fields: dict[str, str] = {}
        for line in result.stdout.splitlines():
            if "=" in line:
                key, value = line.split("=", 1)
                fields[key] = value
        pid_text = fields.get("ExecMainPID", "0")
        return {
            "unit": unit,
            "load_state": fields.get("LoadState"),
            "active_state": fields.get("ActiveState"),
            "sub_state": fields.get("SubState"),
            "unit_file_state": fields.get("UnitFileState"),
            "pid": int(pid_text) if pid_text.isdigit() and int(pid_text) > 0 else None,
        }

    def status(self) -> dict[str, Any]:
        host = self._unit_state(self.host_unit)
        timer = self._unit_state(self.analysis_timer)
        return {
            "available": host["load_state"] == "loaded",
            "active": host["active_state"] == "active",
            "host": host,
            "analysis_timer": timer,
        }

    def perform(self, action: str) -> dict[str, Any]:
        if action == "start":
            commands = [("start", self.host_unit), ("start", self.analysis_timer)]
        elif action == "stop":
            commands = [("stop", self.analysis_timer), ("stop", self.host_unit)]
        elif action == "restart":
            commands = [("restart", self.host_unit), ("start", self.analysis_timer)]
        else:
            raise ValueError("unsupported service action")
        for command in commands:
            result = self._systemctl(*command)
            if result.returncode != 0:
                raise ServiceControlError(
                    (result.stderr or result.stdout or f"systemctl {' '.join(command)} failed").strip()
                )
        return self.status()


class ServiceControlServer:
    def __init__(self, socket_path: str | Path, controller: SystemdServiceController) -> None:
        self.socket_path = Path(socket_path)
        self.controller = controller

    def serve_forever(self) -> None:
        self.socket_path.parent.mkdir(parents=True, exist_ok=True)
        if self.socket_path.exists() or self.socket_path.is_symlink():
            self.socket_path.unlink()
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server:
            server.bind(str(self.socket_path))
            os.chmod(self.socket_path, 0o660)
            server.listen(4)
            try:
                while True:
                    connection, _ = server.accept()
                    with connection:
                        connection.settimeout(2.0)
                        self._handle(connection)
            finally:
                self.socket_path.unlink(missing_ok=True)

    def _handle(self, connection: socket.socket) -> None:
        try:
            chunks: list[bytes] = []
            total = 0
            while True:
                chunk = connection.recv(min(1024, MAX_SERVICE_CONTROL_BYTES + 1 - total))
                if not chunk:
                    break
                chunks.append(chunk)
                total += len(chunk)
                if total > MAX_SERVICE_CONTROL_BYTES:
                    raise ServiceControlError("request exceeds 2048 bytes")
            payload = b"".join(chunks)
            request = json.loads(payload)
            if not isinstance(request, dict) or request.get("schema_version") != 1:
                raise ServiceControlError("expected service control schema version 1")
            action = request.get("action")
            if action == "status":
                state = self.controller.status()
            elif action in {"start", "stop", "restart"}:
                state = self.controller.perform(str(action))
            else:
                raise ServiceControlError("unsupported service action")
            response = {"schema_version": 1, "ok": True, "state": state}
            print(json.dumps({"event": "service_control", "action": action, "ok": True, "unix_ms": time.time_ns() // 1_000_000}), flush=True)
        except (OSError, ValueError, json.JSONDecodeError, ServiceControlError) as exc:
            response = {
                "schema_version": 1,
                "ok": False,
                "error": {"code": "service_control_failed", "message": str(exc)},
            }
            print(json.dumps({"event": "service_control", "ok": False, "error": str(exc), "unix_ms": time.time_ns() // 1_000_000}), flush=True)
        encoded = json.dumps(response, separators=(",", ":")).encode("utf-8")
        try:
            connection.sendall(encoded[:MAX_SERVICE_CONTROL_BYTES])
        except OSError:
            pass
