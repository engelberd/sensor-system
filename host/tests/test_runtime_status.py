from __future__ import annotations

import tempfile
import unittest
from dataclasses import dataclass
from pathlib import Path

from host.common.runtime_status import JsonStatusWriter, JsonlEventWriter


@dataclass
class ExampleStatus:
    value: int


class RuntimeStatusWriterTests(unittest.TestCase):
    def test_status_write_failure_is_non_fatal(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            blocked_parent = Path(tmp) / "not-a-directory"
            blocked_parent.write_text("occupied", encoding="utf-8")
            writer = JsonStatusWriter(
                blocked_parent / "status.json",
                warning_interval_s=3600.0,
            )

            self.assertFalse(writer.write(ExampleStatus(value=1)))

    def test_event_log_rotates_at_configured_size(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "events.jsonl"
            writer = JsonlEventWriter(path, max_bytes=180, backup_count=2)

            for index in range(12):
                self.assertTrue(writer.emit("example", fields={"index": index}))

            self.assertTrue(path.exists())
            self.assertTrue(path.with_name("events.jsonl.1").exists())
            self.assertLessEqual(len(list(Path(tmp).glob("events.jsonl*"))), 3)

    def test_event_write_failure_is_non_fatal(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            blocked_parent = Path(tmp) / "not-a-directory"
            blocked_parent.write_text("occupied", encoding="utf-8")
            writer = JsonlEventWriter(
                blocked_parent / "events.jsonl",
                warning_interval_s=3600.0,
            )

            self.assertFalse(writer.emit("example"))
