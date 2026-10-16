from __future__ import annotations

import json
import os
from pathlib import Path
import socket
import threading
import time
from typing import Any


OPERATIONS_SCHEMA_VERSION = 1
MAX_OPERATIONS_MESSAGE_BYTES = 4 * 1024
DEFAULT_OPERATIONS_SOCKET = Path("/run/sensor-system/operations-v1.sock")


class OperationsIpcClientError(RuntimeError):
    """The local host-v2 Operations IPC could not complete an exchange."""


class OperationsIpcClient:
    """Small synchronous adapter; one call exchanges one bounded datagram."""

    def __init__(
        self,
        socket_path: str | Path = DEFAULT_OPERATIONS_SOCKET,
        *,
        timeout_seconds: float = 2.0,
    ) -> None:
        self.socket_path = Path(socket_path)
        self.timeout_seconds = timeout_seconds

    def status(self, channel_id: int) -> dict[str, Any]:
        return self.exchange(
            {
                "schema_version": OPERATIONS_SCHEMA_VERSION,
                "action": "status",
                "channel_id": channel_id,
            }
        )

    def get(self, channel_id: int, operation_id: int) -> dict[str, Any]:
        return self.exchange(
            {
                "schema_version": OPERATIONS_SCHEMA_VERSION,
                "action": "get",
                "channel_id": channel_id,
                "operation_id": operation_id,
            }
        )

    def submit(self, request: dict[str, Any]) -> dict[str, Any]:
        document = dict(request)
        document["schema_version"] = OPERATIONS_SCHEMA_VERSION
        document["action"] = "submit"
        return self.exchange(document)

    def exchange(self, document: dict[str, Any]) -> dict[str, Any]:
        encoded = json.dumps(document, separators=(",", ":")).encode("utf-8")
        if len(encoded) > MAX_OPERATIONS_MESSAGE_BYTES:
            raise ValueError("operations request exceeds 4096 bytes")
        if not self.socket_path.exists():
            raise OperationsIpcClientError("host-v2 Operations socket is unavailable")

        client_path = self._client_path()
        client = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
        try:
            client.bind(str(client_path))
            os.chmod(client_path, 0o600)
            client.settimeout(self.timeout_seconds)
            client.sendto(encoded, str(self.socket_path))
            response = client.recv(MAX_OPERATIONS_MESSAGE_BYTES + 1)
        except (OSError, TimeoutError) as error:
            raise OperationsIpcClientError(f"Operations IPC failed: {error}") from error
        finally:
            client.close()
            client_path.unlink(missing_ok=True)

        if len(response) > MAX_OPERATIONS_MESSAGE_BYTES:
            raise OperationsIpcClientError("Operations response exceeds 4096 bytes")
        try:
            payload = json.loads(response.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise OperationsIpcClientError("Operations response is not valid JSON") from error
        if not isinstance(payload, dict) or payload.get("schema_version") != 1:
            raise OperationsIpcClientError("Operations response has an unsupported schema")
        return payload

    def _client_path(self) -> Path:
        nonce = time.time_ns() ^ (threading.get_ident() << 16)
        return self.socket_path.parent / f"dashboard-{os.getpid()}-{nonce & ((1 << 64) - 1):016x}.sock"


def fresh_operation_id() -> int:
    value = time.time_ns() ^ (os.getpid() << 32) ^ threading.get_ident()
    return (value & ((1 << 64) - 1)) or 1

