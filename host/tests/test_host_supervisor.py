from __future__ import annotations

import unittest
from pathlib import Path

from host.common.system_config import HostSystemConfig
from host.common.runtime_status import JsonStatusWriter
from host.host_supervisor import (
    WorkerState,
    build_supervisor_snapshot,
    build_worker_command,
    channel_output_dir,
)


class SupervisorWorkerCommandTests(unittest.TestCase):
    def make_config(self) -> HostSystemConfig:
        return HostSystemConfig.from_dict(
            {
                "storage": {
                    "root_dir": "/data/sensor-system",
                    "format": "hdf5",
                    "compression": "gzip",
                    "window_seconds": 600,
                },
                "channels": [
                    {
                        "name": "line-a",
                        "port": "/dev/ttyUSB0",
                        "nodes": [{"id": 1}],
                    }
                ],
            }
        )

    def test_channel_output_dir_is_namespaced_under_storage_root(self) -> None:
        config = self.make_config()
        channel = config.channels[0]

        self.assertEqual(channel_output_dir(config, channel), "/data/sensor-system/line-a")

    def test_worker_command_uses_channel_specific_output_dir(self) -> None:
        config = self.make_config()
        channel = config.channels[0]

        command = build_worker_command(
            "python3",
            Path("host/host_recorder.py"),
            channel,
            config,
            Path("/tmp/line-a.status.json"),
            Path("/tmp/line-a.events.jsonl"),
        )

        output_dir_index = command.index("--output-dir") + 1
        self.assertEqual(command[output_dir_index], "/data/sensor-system/line-a")

    def test_supervisor_snapshot_uses_channel_destination_before_runtime_status(self) -> None:
        config = self.make_config()
        channel = config.channels[0]
        state = WorkerState(
            config=channel,
            status_file=Path("/tmp/missing-line-a.status.json"),
            event_log=Path("/tmp/line-a.events.jsonl"),
            process_log=Path("/tmp/line-a.process.log"),
        )

        snapshot = build_supervisor_snapshot(
            config,
            JsonStatusWriter("/tmp/supervisor.status.json"),
            Path("/tmp/supervisor.events.jsonl"),
            "2026-06-15T00:00:00+00:00",
            [state],
        )

        self.assertEqual(snapshot.channels[0].destination, "/data/sensor-system/line-a")


if __name__ == "__main__":
    unittest.main()
