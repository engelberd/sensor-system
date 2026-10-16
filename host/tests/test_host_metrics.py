from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from host.common.host_metrics import HostMetricsSampler


class HostMetricsSamplerTests(unittest.TestCase):
    def test_samples_cpu_delta_and_available_memory(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            proc_root = Path(tmp)
            (proc_root / "stat").write_text("cpu  100 0 50 850 0 0 0 0 0 0\n", encoding="utf-8")
            (proc_root / "meminfo").write_text(
                "MemTotal:       1000 kB\nMemAvailable:    400 kB\n",
                encoding="utf-8",
            )
            sampler = HostMetricsSampler(proc_root)
            (proc_root / "stat").write_text("cpu  130 0 60 910 0 0 0 0 0 0\n", encoding="utf-8")

            metrics = sampler.sample()

            self.assertEqual(metrics.cpu_used_percent, 40.0)
            self.assertEqual(metrics.memory_total_bytes, 1_024_000)
            self.assertEqual(metrics.memory_available_bytes, 409_600)
            self.assertEqual(metrics.memory_used_bytes, 614_400)
            self.assertEqual(metrics.memory_used_percent, 60.0)

    def test_missing_procfs_returns_unavailable_metrics(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            metrics = HostMetricsSampler(Path(tmp)).sample()

            self.assertIsNone(metrics.cpu_used_percent)
            self.assertIsNone(metrics.memory_used_percent)


if __name__ == "__main__":
    unittest.main()
