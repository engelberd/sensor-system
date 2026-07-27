from __future__ import annotations

import tempfile
import unittest
import json
from pathlib import Path

from host.common.system_config import HostSystemConfig
from host.common.runtime_status import JsonStatusWriter
from host.host_supervisor import (
    apply_supervisor_command,
    apply_channel_command,
    WorkerState,
    build_supervisor_snapshot,
    build_worker_command,
    channel_command_path,
    channel_output_dir,
    load_channel_command,
    supervisor_command_path,
    rotate_process_log,
    restart_delay_for,
)
from host.common.runtime_status import JsonlEventWriter


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
        console_interval_index = command.index("--console-status-interval") + 1
        self.assertEqual(command[console_interval_index], "30.0")
        timing_mode_index = command.index("--timing-mode") + 1
        self.assertEqual(command[timing_mode_index], "legacy")

    def test_worker_command_passes_required_timing_mode(self) -> None:
        config = HostSystemConfig.from_dict(
            {
                "storage": {
                    "root_dir": "/data/sensor-system",
                    "format": "hdf5",
                },
                "channels": [
                    {
                        "name": "line-d",
                        "port": "/dev/ttyUSB3",
                        "timing_mode": "required",
                        "nodes": [{"id": 1}],
                    }
                ],
            }
        )

        command = build_worker_command(
            "python3",
            Path("host/host_recorder.py"),
            config.channels[0],
            config,
            Path("/tmp/line-d.status.json"),
            Path("/tmp/line-d.events.jsonl"),
        )

        timing_mode_index = command.index("--timing-mode") + 1
        self.assertEqual(command[timing_mode_index], "required")

    def test_invalid_channel_timing_mode_is_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "unsupported timing_mode"):
            HostSystemConfig.from_dict(
                {
                    "channels": [
                        {
                            "name": "line-a",
                            "port": "/dev/ttyUSB0",
                            "timing_mode": "unsafe",
                            "nodes": [{"id": 1}],
                        }
                    ],
                }
            )

    def test_required_timing_mode_rejects_non_hdf5_storage(self) -> None:
        with self.assertRaisesRegex(ValueError, "requires HDF5 storage"):
            HostSystemConfig.from_dict(
                {
                    "storage": {"format": "csv"},
                    "channels": [
                        {
                            "name": "line-d",
                            "port": "/dev/ttyUSB3",
                            "timing_mode": "required",
                            "nodes": [{"id": 1}],
                        }
                    ],
                }
            )

    def test_process_log_rotation_is_bounded(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "line-a.process.log"
            path.write_text("x" * 200, encoding="utf-8")

            self.assertTrue(rotate_process_log(path, max_bytes=100, backup_count=2))
            self.assertEqual(path.read_text(encoding="utf-8"), "")
            self.assertEqual(
                path.with_name("line-a.process.log.1").read_text(encoding="utf-8"),
                "x" * 200,
            )

    def test_restart_delay_uses_capped_exponential_backoff(self) -> None:
        self.assertEqual(restart_delay_for(1, 2.0, 60.0), 2.0)
        self.assertEqual(restart_delay_for(2, 2.0, 60.0), 4.0)
        self.assertEqual(restart_delay_for(6, 2.0, 60.0), 60.0)
        self.assertEqual(restart_delay_for(20, 2.0, 60.0), 60.0)

    def test_channel_output_dir_can_override_namespaced_default(self) -> None:
        config = HostSystemConfig.from_dict(
            {
                "storage": {"root_dir": "/data/sensor-system"},
                "channels": [
                    {
                        "name": "line-a",
                        "port": "/dev/ttyUSB0",
                        "output_dir": "/archive/line-a",
                        "nodes": [{"id": 1}],
                    }
                ],
            }
        )

        self.assertEqual(
            channel_output_dir(config, config.channels[0]),
            "/archive/line-a",
        )

    def test_supervisor_snapshot_uses_channel_destination_before_runtime_status(self) -> None:
        config = self.make_config()
        channel = config.channels[0]
        state = WorkerState(
            config=channel,
            status_file=Path("/tmp/missing-line-a.status.json"),
            event_log=Path("/tmp/line-a.events.jsonl"),
            process_log=Path("/tmp/line-a.process.log"),
            command_file=Path("/tmp/line-a.command.json"),
        )

        snapshot = build_supervisor_snapshot(
            config,
            JsonStatusWriter("/tmp/supervisor.status.json"),
            Path("/tmp/supervisor.events.jsonl"),
            "2026-06-15T00:00:00+00:00",
            [state],
        )

        self.assertEqual(snapshot.channels[0].destination, "/data/sensor-system/line-a")
        self.assertEqual(snapshot.channels[0].timing_mode, "legacy")
        self.assertTrue(snapshot.channels[0].desired_running)
        self.assertEqual(snapshot.channels[0].control_state, "waiting")
        self.assertEqual(snapshot.storage_total_bytes, 0)

    def test_supervisor_snapshot_tolerates_older_runtime_status_without_rate_metrics(self) -> None:
        config = self.make_config()
        channel = config.channels[0]
        state = WorkerState(
            config=channel,
            status_file=Path("/tmp/line-a.status.json"),
            event_log=Path("/tmp/line-a.events.jsonl"),
            process_log=Path("/tmp/line-a.process.log"),
            command_file=Path("/tmp/line-a.command.json"),
            last_status={
                "nodes": [
                    {
                        "node_id": 1,
                        "online": True,
                        "sensor_odr_hz": 250,
                        "output_odr_hz": 125.0,
                        "samples_written": 500,
                        "expected_sample_seq": 501,
                        "last_written_seq": 500,
                        "bursts_ok": 10,
                        "bursts_no_data": 0,
                        "bursts_failed": 0,
                        "gaps_detected": 0,
                        "empty_polls": 0,
                        "sensor_loss_total": 0,
                        "sensor_loss_session": 0,
                        "rx_overflow_total": 0,
                        "rx_overflow_session": 0,
                        "packet_overwrite_total": 0,
                        "packet_overwrite_session": 0,
                        "baseline_sensor_loss": 0,
                        "baseline_rx_overflow_count": 0,
                        "baseline_packet_overwrite_count": 0,
                        "last_temperature_c": 22.5,
                        "last_temperature_unix_ns": 1234567890,
                    }
                ]
            },
        )

        snapshot = build_supervisor_snapshot(
            config,
            JsonStatusWriter("/tmp/supervisor.status.json"),
            Path("/tmp/supervisor.events.jsonl"),
            "2026-06-15T00:00:00+00:00",
            [state],
        )

        node = snapshot.channels[0].nodes[0]
        self.assertIsNone(node.instant_samples_per_second_5s)
        self.assertIsNone(node.rate_stability_percent_5s)

    def test_channel_command_path_and_load(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            runtime_dir = Path(tmp)
            command_file = channel_command_path(runtime_dir, "line-a")
            command_file.write_text('{"action": "restart"}', encoding="utf-8")

            payload = load_channel_command(command_file)

            self.assertEqual(payload, {"action": "restart"})
            self.assertFalse(command_file.exists())

    def test_supervisor_command_path(self) -> None:
        self.assertEqual(
            supervisor_command_path(Path("/tmp/runtime")),
            Path("/tmp/runtime/supervisor.command.json"),
        )

    def test_apply_channel_command_stop_marks_manual_stop(self) -> None:
        config = self.make_config()
        channel = config.channels[0]
        with tempfile.TemporaryDirectory() as tmp:
            event_writer = JsonlEventWriter(Path(tmp) / "events.jsonl")
            state = WorkerState(
                config=channel,
                status_file=Path(tmp) / "line-a.status.json",
                event_log=Path(tmp) / "line-a.events.jsonl",
                process_log=Path(tmp) / "line-a.process.log",
                command_file=Path(tmp) / "line-a.command.json",
                desired_running=True,
            )

            apply_channel_command(
                state,
                "stop",
                now_monotonic=10.0,
                restart_delay_s=2.0,
                event_writer=event_writer,
            )

            self.assertFalse(state.desired_running)

    def test_apply_channel_command_restart_does_not_increment_restart_count(self) -> None:
        config = self.make_config()
        channel = config.channels[0]
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            event_writer = JsonlEventWriter(temp_dir / "events.jsonl")
            state = WorkerState(
                config=channel,
                status_file=temp_dir / "line-a.status.json",
                event_log=temp_dir / "line-a.events.jsonl",
                process_log=temp_dir / "line-a.process.log",
                command_file=temp_dir / "line-a.command.json",
                desired_running=True,
                restart_count=5,
            )

            apply_channel_command(
                state,
                "restart",
                now_monotonic=10.0,
                restart_delay_s=2.0,
                event_writer=event_writer,
            )

            self.assertEqual(state.restart_count, 5)
            self.assertEqual(state.next_start_monotonic, 10.0)

    def test_apply_channel_command_purge_clears_runtime_files(self) -> None:
        config = self.make_config()
        channel = config.channels[0]
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            supervisor_event_log = temp_dir / "supervisor.events.jsonl"
            event_writer = JsonlEventWriter(supervisor_event_log)
            state = WorkerState(
                config=channel,
                status_file=temp_dir / "line-a.status.json",
                event_log=temp_dir / "line-a.events.jsonl",
                process_log=temp_dir / "line-a.process.log",
                command_file=temp_dir / "line-a.command.json",
                desired_running=True,
                restart_count=7,
                last_exit_code=1,
                last_status={"updated_utc": "2026-06-18T19:00:00+00:00"},
                last_status_updated_utc="2026-06-18T19:00:00+00:00",
            )
            state.status_file.write_text('{"updated_utc": "2026-06-18T19:00:00+00:00"}', encoding="utf-8")
            state.event_log.write_text('{"event":"runtime_error"}\n', encoding="utf-8")
            state.process_log.write_text("[ERROR] boom\n", encoding="utf-8")
            supervisor_event_log.write_text(
                "\n".join(
                    [
                        json.dumps({"event": "channel_started", "channel_name": "line-a"}),
                        json.dumps({"event": "channel_started", "channel_name": "line-b"}),
                    ]
                )
                + "\n",
                encoding="utf-8",
            )

            apply_channel_command(
                state,
                "purge",
                now_monotonic=10.0,
                restart_delay_s=2.0,
                event_writer=event_writer,
            )

            self.assertTrue(state.desired_running)
            self.assertEqual(state.next_start_monotonic, 10.0)
            self.assertEqual(state.restart_count, 0)
            self.assertIsNone(state.last_exit_code)
            self.assertIsNone(state.last_status)
            self.assertIsNone(state.last_status_updated_utc)
            self.assertFalse(state.event_log.exists())
            self.assertFalse(state.status_file.exists())
            self.assertEqual(state.process_log.read_text(encoding="utf-8"), "")
            supervisor_payloads = [
                json.loads(line)
                for line in supervisor_event_log.read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
            self.assertEqual(
                [payload.get("channel_name") for payload in supervisor_payloads if payload.get("event") == "channel_started"],
                ["line-b"],
            )
            self.assertEqual(supervisor_payloads[-1]["event"], "channel_purged")
            self.assertEqual(supervisor_payloads[-1]["channel_name"], "line-a")

    def test_apply_supervisor_command_restart_all_marks_all_enabled_channels(self) -> None:
        config = HostSystemConfig.from_dict(
            {
                "channels": [
                    {"name": "line-a", "port": "/dev/ttyUSB0", "nodes": [{"id": 1}]},
                    {"name": "line-b", "port": "/dev/ttyUSB1", "nodes": [{"id": 1}]},
                ]
            }
        )
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            event_writer = JsonlEventWriter(temp_dir / "supervisor.events.jsonl")
            states = [
                WorkerState(
                    config=config.channels[0],
                    status_file=temp_dir / "line-a.status.json",
                    event_log=temp_dir / "line-a.events.jsonl",
                    process_log=temp_dir / "line-a.process.log",
                    command_file=temp_dir / "line-a.command.json",
                    desired_running=False,
                    restart_count=4,
                    next_start_monotonic=0.0,
                ),
                WorkerState(
                    config=config.channels[1],
                    status_file=temp_dir / "line-b.status.json",
                    event_log=temp_dir / "line-b.events.jsonl",
                    process_log=temp_dir / "line-b.process.log",
                    command_file=temp_dir / "line-b.command.json",
                    desired_running=False,
                    restart_count=7,
                    next_start_monotonic=0.0,
                ),
            ]

            apply_supervisor_command(
                states,
                "restart_all",
                now_monotonic=12.0,
                restart_delay_s=2.0,
                event_writer=event_writer,
            )

            self.assertTrue(all(state.desired_running for state in states))
            self.assertEqual(states[0].next_start_monotonic, 12.0)
            self.assertEqual(states[1].next_start_monotonic, 12.0)
            self.assertEqual(states[0].restart_count, 4)
            self.assertEqual(states[1].restart_count, 7)
            payloads = [
                json.loads(line)
                for line in (temp_dir / "supervisor.events.jsonl").read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
            self.assertEqual(payloads[-1]["event"], "supervisor_restart_all_requested")

    def test_apply_supervisor_command_purge_all_clears_runtime_for_all_enabled_channels(self) -> None:
        config = HostSystemConfig.from_dict(
            {
                "channels": [
                    {"name": "line-a", "port": "/dev/ttyUSB0", "nodes": [{"id": 1}]},
                    {"name": "line-b", "port": "/dev/ttyUSB1", "nodes": [{"id": 1}]},
                ]
            }
        )
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            supervisor_event_log = temp_dir / "supervisor.events.jsonl"
            event_writer = JsonlEventWriter(supervisor_event_log)
            states = [
                WorkerState(
                    config=config.channels[0],
                    status_file=temp_dir / "line-a.status.json",
                    event_log=temp_dir / "line-a.events.jsonl",
                    process_log=temp_dir / "line-a.process.log",
                    command_file=temp_dir / "line-a.command.json",
                    desired_running=False,
                    restart_count=4,
                    next_start_monotonic=0.0,
                    last_exit_code=1,
                    last_status={"updated_utc": "2026-06-18T19:00:00+00:00"},
                    last_status_updated_utc="2026-06-18T19:00:00+00:00",
                ),
                WorkerState(
                    config=config.channels[1],
                    status_file=temp_dir / "line-b.status.json",
                    event_log=temp_dir / "line-b.events.jsonl",
                    process_log=temp_dir / "line-b.process.log",
                    command_file=temp_dir / "line-b.command.json",
                    desired_running=False,
                    restart_count=7,
                    next_start_monotonic=0.0,
                    last_exit_code=2,
                    last_status={"updated_utc": "2026-06-18T19:05:00+00:00"},
                    last_status_updated_utc="2026-06-18T19:05:00+00:00",
                ),
            ]
            for state in states:
                state.status_file.write_text('{"updated_utc": "2026-06-18T19:00:00+00:00"}', encoding="utf-8")
                state.event_log.write_text('{"event":"runtime_error"}\n', encoding="utf-8")
                state.process_log.write_text("[ERROR] boom\n", encoding="utf-8")
            supervisor_event_log.write_text(
                "\n".join(
                    [
                        json.dumps({"event": "channel_started", "channel_name": "line-a"}),
                        json.dumps({"event": "channel_started", "channel_name": "line-b"}),
                    ]
                )
                + "\n",
                encoding="utf-8",
            )

            apply_supervisor_command(
                states,
                "purge_all",
                now_monotonic=12.0,
                restart_delay_s=2.0,
                event_writer=event_writer,
            )

            self.assertTrue(all(state.desired_running for state in states))
            self.assertEqual(states[0].next_start_monotonic, 12.0)
            self.assertEqual(states[1].next_start_monotonic, 12.0)
            for state in states:
                self.assertEqual(state.restart_count, 0)
                self.assertIsNone(state.last_exit_code)
                self.assertIsNone(state.last_status)
                self.assertIsNone(state.last_status_updated_utc)
                self.assertFalse(state.event_log.exists())
                self.assertFalse(state.status_file.exists())
                self.assertEqual(state.process_log.read_text(encoding="utf-8"), "")
            payloads = [
                json.loads(line)
                for line in supervisor_event_log.read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
            self.assertEqual(
                [payload.get("channel_name") for payload in payloads if payload.get("event") == "channel_purged"],
                ["line-a", "line-b"],
            )
            self.assertEqual(payloads[-1]["event"], "supervisor_purge_all_requested")


if __name__ == "__main__":
    unittest.main()
