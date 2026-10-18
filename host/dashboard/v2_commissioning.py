from __future__ import annotations

import json
import os
import re
import tempfile
from pathlib import Path
from typing import Any


_HARDWARE_ID = re.compile(r"^[0-9a-fA-F]{16}$")


class CommissioningProfileError(ValueError):
    """A discovered node cannot be added safely to the installation profile."""


class CommissioningProfileStore:
    """Atomically adds stable node identity to the tracked system profile."""

    def __init__(self, active_config_path: str | Path) -> None:
        self.active_config_path = Path(active_config_path).resolve()

    def profile_path(self) -> Path:
        current = self.active_config_path
        visited: set[Path] = set()
        while True:
            if current in visited:
                raise CommissioningProfileError("circular config extends")
            visited.add(current)
            document = self._read_document(current)
            parent = document.get("extends")
            if not isinstance(parent, str) or not parent.strip():
                return current
            candidate = Path(parent)
            if not candidate.is_absolute():
                candidate = current.parent / candidate
            candidate = candidate.resolve()
            if candidate.parent.name == "systems":
                return candidate
            current = candidate

    def add_node(
        self,
        *,
        channel_id: int,
        node_name: str,
        node_address: int,
        hardware_id: str,
        board_revision: int,
        output_rate_hz: float | int | str = 125,
    ) -> dict[str, Any]:
        node = self.validate_node(
            channel_id=channel_id,
            node_name=node_name,
            node_address=node_address,
            hardware_id=hardware_id,
            board_revision=board_revision,
            output_rate_hz=output_rate_hz,
        )

        profile_path = self.profile_path()
        profile = self._read_document(profile_path)
        channels = profile.get("channels")
        if not isinstance(channels, list):
            raise CommissioningProfileError("installation profile has no channels array")
        channel = next(
            (
                value
                for value in channels
                if isinstance(value, dict) and int(value.get("channel_id", 0)) == channel_id
            ),
            None,
        )
        if channel is None:
            raise CommissioningProfileError("channel is not defined in the installation profile")
        nodes = channel.get("nodes")
        if not isinstance(nodes, list):
            raise CommissioningProfileError("channel has no nodes array")

        nodes.append(node)
        self._atomic_write(profile_path, profile)
        return {"profile_path": str(profile_path), "node": node}

    def update_node_output_rate(
        self,
        *,
        channel_id: int,
        node_address: int,
        output_rate_hz: float | int,
    ) -> dict[str, Any]:
        """Persist the scheduler-relevant part of node configuration.

        The complete measurement configuration is owned and persisted by the
        node.  Output ODR is also installation policy because the host uses it
        for admission and scheduling after the next process start.
        """
        rate = float(output_rate_hz)
        supported_rates = {
            4000.0,
            2000.0,
            1000.0,
            500.0,
            250.0,
            125.0,
            62.5,
            31.25,
            15.625,
            7.8125,
            3.90625,
        }
        if rate not in supported_rates:
            raise CommissioningProfileError("unsupported output rate")

        profile_path = self.profile_path()
        profile = self._read_document(profile_path)
        channels = profile.get("channels")
        if not isinstance(channels, list):
            raise CommissioningProfileError("installation profile has no channels array")
        channel = next(
            (
                value
                for value in channels
                if isinstance(value, dict) and int(value.get("channel_id", 0)) == channel_id
            ),
            None,
        )
        if channel is None:
            raise CommissioningProfileError("channel is not defined in the installation profile")
        nodes = channel.get("nodes")
        if not isinstance(nodes, list):
            raise CommissioningProfileError("channel has no nodes array")
        node = next(
            (
                value
                for value in nodes
                if isinstance(value, dict) and int(value.get("id", 0)) == node_address
            ),
            None,
        )
        if node is None:
            raise CommissioningProfileError("node is not defined in the installation profile")

        stored_rate: float | int = int(rate) if rate.is_integer() else rate
        try:
            unchanged = float(node.get("output_rate_hz")) == rate
        except (TypeError, ValueError):
            unchanged = False
        if unchanged:
            return {
                "profile_path": str(profile_path),
                "channel_id": channel_id,
                "node_address": node_address,
                "output_rate_hz": stored_rate,
            }
        node["output_rate_hz"] = stored_rate
        self._atomic_write(profile_path, profile)
        return {
            "profile_path": str(profile_path),
            "channel_id": channel_id,
            "node_address": node_address,
            "output_rate_hz": stored_rate,
        }

    def verify_writable(self) -> Path:
        profile_path = self.profile_path()
        descriptor: int | None = None
        temporary_path: Path | None = None
        try:
            descriptor, temporary_name = tempfile.mkstemp(
                prefix=".commissioning-preflight.", dir=profile_path.parent
            )
            temporary_path = Path(temporary_name)
            os.write(descriptor, b"profile-write-preflight\n")
            os.fsync(descriptor)
        except OSError as exc:
            raise CommissioningProfileError(
                f"installation profile directory is not writable: {exc}"
            ) from exc
        finally:
            if descriptor is not None:
                os.close(descriptor)
            if temporary_path is not None:
                temporary_path.unlink(missing_ok=True)
        return profile_path

    def validate_node(
        self,
        *,
        channel_id: int,
        node_name: str,
        node_address: int,
        hardware_id: str,
        board_revision: int,
        output_rate_hz: float | int | str = 125,
    ) -> dict[str, Any]:
        name = node_name.strip()
        hardware = hardware_id.lower()
        if not name or len(name) > 64 or any(ord(character) < 32 for character in name):
            raise CommissioningProfileError("node name must contain 1..64 printable characters")
        if not 1 <= channel_id <= 16:
            raise CommissioningProfileError("channel_id must be in 1..16")
        if not 1 <= node_address <= 253:
            raise CommissioningProfileError("node address must be in 1..253")
        if not _HARDWARE_ID.fullmatch(hardware):
            raise CommissioningProfileError("hardware_id must contain exactly 16 hex digits")
        if board_revision not in {1, 2}:
            raise CommissioningProfileError("unsupported board revision")

        effective = _load_merged_document(self.active_config_path)
        for channel in effective.get("channels", []):
            if not isinstance(channel, dict):
                continue
            for node in channel.get("nodes", []):
                if not isinstance(node, dict):
                    continue
                if str(node.get("hardware_id", "")).lower() == hardware:
                    raise CommissioningProfileError("hardware_id is already present in the installation profile")
                if int(channel.get("channel_id", 0)) == channel_id and int(node.get("id", 0)) == node_address:
                    raise CommissioningProfileError("node address is already used on this channel")

        profile = self._read_document(self.profile_path())
        channels = profile.get("channels")
        if not isinstance(channels, list) or not any(
            (
                isinstance(value, dict)
                and int(value.get("channel_id", 0)) == channel_id
                and isinstance(value.get("nodes"), list)
            )
            for value in channels
        ):
            raise CommissioningProfileError("channel is not defined in the installation profile")
        return {
            "id": node_address,
            "name": name,
            "hardware_id": hardware,
            "board_revision": board_revision,
            "enabled": True,
            "output_rate_hz": output_rate_hz,
        }

    @staticmethod
    def _read_document(path: Path) -> dict[str, Any]:
        try:
            document = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            raise CommissioningProfileError(f"cannot read installation profile: {exc}") from exc
        if not isinstance(document, dict):
            raise CommissioningProfileError("installation profile root must be an object")
        return document

    @staticmethod
    def _atomic_write(path: Path, document: dict[str, Any]) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        descriptor, temporary_name = tempfile.mkstemp(
            prefix=f".{path.name}.", suffix=".tmp", dir=path.parent
        )
        temporary_path = Path(temporary_name)
        try:
            with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
                json.dump(document, handle, indent=2, ensure_ascii=False)
                handle.write("\n")
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(temporary_path, path)
            directory = os.open(path.parent, os.O_RDONLY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            temporary_path.unlink(missing_ok=True)


def _load_merged_document(path: Path, visited: set[Path] | None = None) -> dict[str, Any]:
    resolved = path.resolve()
    seen = set() if visited is None else set(visited)
    if resolved in seen:
        raise CommissioningProfileError("circular config extends")
    seen.add(resolved)
    document = CommissioningProfileStore._read_document(resolved)
    parent = document.get("extends")
    if not isinstance(parent, str) or not parent.strip():
        return {key: value for key, value in document.items() if key != "extends"}
    parent_path = Path(parent)
    if not parent_path.is_absolute():
        parent_path = resolved.parent / parent_path
    merged = _load_merged_document(parent_path, seen)
    return _deep_merge(merged, document)


def _deep_merge(base: Any, overlay: Any, section: str | None = None) -> Any:
    if isinstance(base, dict) and isinstance(overlay, dict):
        result = dict(base)
        for key, value in overlay.items():
            if key == "extends":
                continue
            result[key] = _deep_merge(result[key], value, key) if key in result else value
        return result
    if isinstance(base, list) and isinstance(overlay, list) and section in {"channels", "nodes"}:
        result = list(base)
        identity_key = "name" if section == "channels" else "id"
        positions = {
            item.get(identity_key): index
            for index, item in enumerate(result)
            if isinstance(item, dict) and identity_key in item
        }
        for item in overlay:
            identity = item.get(identity_key) if isinstance(item, dict) else None
            if identity in positions:
                index = positions[identity]
                result[index] = _deep_merge(result[index], item, section)
            else:
                positions[identity] = len(result)
                result.append(item)
        return result
    return overlay
