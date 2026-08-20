from __future__ import annotations

import json
from pathlib import Path
import tempfile
import unittest

from host.common.instance_lock import InstanceLock


class InstanceLockTests(unittest.TestCase):
    def test_second_owner_is_rejected_and_metadata_names_holder(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "line-a.recorder.lock"
            first = InstanceLock(path, {"channel_name": "line-a"})
            second = InstanceLock(path, {"channel_name": "line-a"})
            first.acquire()
            try:
                owner = json.loads(path.read_text(encoding="utf-8"))
                self.assertEqual(owner["channel_name"], "line-a")
                with self.assertRaisesRegex(RuntimeError, "line-a"):
                    second.acquire()
            finally:
                first.release()

            second.acquire()
            second.release()


if __name__ == "__main__":
    unittest.main()
