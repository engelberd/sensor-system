from __future__ import annotations

import json
import tempfile
import time
import unittest
from pathlib import Path

from host.host_doctor import run_checks


class HostDoctorTests(unittest.TestCase):
    def test_disabled_missing_port_is_warning_not_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for name in ("recordings", "archive", "logs", "runtime"):
                (root / name).mkdir()
            status = root / "supervisor.json"
            status.write_text(
                json.dumps({"channels": [{"name": "line-f", "running": False}]}),
                encoding="utf-8",
            )
            config = root / "config.json"
            config.write_text(
                json.dumps(
                    {
                        "storage": {
                            "root_dir": str(root / "recordings"),
                            "archive_dir": str(root / "archive"),
                            "capture_schema": 1,
                        },
                        "supervisor": {
                            "status_file": str(status),
                            "channel_runtime_dir": str(root / "runtime"),
                            "log_dir": str(root / "logs"),
                        },
                        "channels": [
                            {
                                "name": "line-f",
                                "enabled": False,
                                "port": str(root / "missing-port"),
                                "nodes": [{"id": 1, "board_revision": 2}],
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )

            findings = run_checks(config, project_root=root, min_free_gb=0, now=time.time())

            self.assertFalse(any(item.severity == "ERROR" for item in findings))
            self.assertTrue(any(item.code == "port.missing" for item in findings))

    def test_duplicate_enabled_port_and_missing_revision_are_errors(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            config = root / "config.json"
            config.write_text(
                json.dumps(
                    {
                        "storage": {"capture_schema": 1},
                        "channels": [
                            {"name": "line-a", "port": "/dev/missing", "nodes": [{"id": 1}]},
                            {"name": "line-b", "port": "/dev/missing", "nodes": [{"id": 1}]},
                        ],
                    }
                ),
                encoding="utf-8",
            )

            findings = run_checks(config, project_root=root, min_free_gb=0)
            codes = {item.code for item in findings if item.severity == "ERROR"}

            self.assertIn("config.duplicate-port", codes)
            self.assertIn("node.board-revision", codes)


if __name__ == "__main__":
    unittest.main()
