"""Advisory process locks with human-readable owner metadata."""

from __future__ import annotations

from datetime import datetime, timezone
import fcntl
import json
import os
from pathlib import Path
from typing import TextIO


class InstanceLock:
    """Hold an exclusive lock for the lifetime of one host process."""

    def __init__(self, path: str | Path, metadata: dict[str, object]) -> None:
        self.path = Path(path)
        self.metadata = dict(metadata)
        self.handle: TextIO | None = None

    def acquire(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        handle = self.path.open("a+", encoding="utf-8")
        try:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            handle.seek(0)
            owner = handle.read().strip() or "owner metadata unavailable"
            handle.close()
            raise RuntimeError(
                f"another process owns recorder lock '{self.path}': {owner}"
            ) from exc
        self.handle = handle
        payload = {
            "pid": os.getpid(),
            "started_utc": datetime.now(timezone.utc).isoformat(),
            **self.metadata,
        }
        handle.seek(0)
        handle.truncate()
        json.dump(payload, handle, sort_keys=True)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())

    def release(self) -> None:
        if self.handle is None:
            return
        try:
            fcntl.flock(self.handle.fileno(), fcntl.LOCK_UN)
        finally:
            self.handle.close()
            self.handle = None

    def __enter__(self) -> "InstanceLock":
        self.acquire()
        return self

    def __exit__(self, *_args: object) -> None:
        self.release()
