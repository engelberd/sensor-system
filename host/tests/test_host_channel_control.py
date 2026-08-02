from __future__ import annotations

import unittest
from unittest.mock import MagicMock, patch

from host.host_channel_control import _wait_for_node_ready, build_parser, main


class ChannelControlBootWaitTests(unittest.TestCase):
    def test_direct_stop_waits_for_worker_confirmation(self) -> None:
        config = {
            "supervisor": {
                "status_file": "/tmp/supervisor.json",
                "channel_runtime_dir": "/tmp/channels",
            },
            "channels": [
                {"name": "line-g", "port": "/dev/ttyUSB6", "baud": 115200}
            ],
        }

        with (
            patch("host.host_channel_control.build_parser") as parser,
            patch("host.host_channel_control._resolve_system_config"),
            patch("host.host_channel_control._load_system_config", return_value=config),
            patch("host.host_channel_control._supervisor_status_path") as status_path,
            patch("host.host_channel_control._runtime_dir") as runtime_dir,
            patch("host.host_channel_control._write_channel_command") as write_command,
            patch("host.host_channel_control._wait_for_channel_running", return_value=True) as wait,
        ):
            parser.return_value.parse_args.return_value = build_parser().parse_args(
                ["stop", "--channel", "line-g"]
            )
            self.assertEqual(main(), 0)

        write_command.assert_called_once_with(runtime_dir.return_value, "line-g", "stop")
        wait.assert_called_once_with(status_path.return_value, "line-g", False, 10.0)

    def test_restart_defaults_cover_bootloader_and_application_delays(self) -> None:
        args = build_parser().parse_args(["restart-remote", "--channel", "line-b"])

        self.assertEqual(args.settle_ms, 6000)
        self.assertEqual(args.ready_timeout, 20.0)
        self.assertEqual(args.ready_poll_ms, 250)

    def test_wait_for_node_ready_retries_until_protocol_answers(self) -> None:
        clock = [0.0]
        serial_handles = [MagicMock(), MagicMock()]

        with (
            patch("host.host_channel_control.time.monotonic", side_effect=lambda: clock[0]),
            patch("host.host_channel_control.time.sleep", side_effect=lambda delay: clock.__setitem__(0, clock[0] + delay)),
            patch("host.host_channel_control.serial.Serial", side_effect=serial_handles),
            patch(
                "host.host_channel_control.send_and_wait",
                side_effect=[RuntimeError("not ready"), object()],
            ),
        ):
            elapsed = _wait_for_node_ready(
                "/dev/ttyUSB1", 115200, 1, 0.5, 5.0, 0.25
            )

        self.assertAlmostEqual(elapsed, 0.25)
        self.assertTrue(all(handle.close.called for handle in serial_handles))

    def test_wait_for_node_ready_has_bounded_timeout(self) -> None:
        clock = [0.0]

        with (
            patch("host.host_channel_control.time.monotonic", side_effect=lambda: clock[0]),
            patch("host.host_channel_control.time.sleep", side_effect=lambda delay: clock.__setitem__(0, clock[0] + delay)),
            patch("host.host_channel_control.serial.Serial", return_value=MagicMock()),
            patch("host.host_channel_control.send_and_wait", side_effect=RuntimeError("not ready")),
        ):
            with self.assertRaisesRegex(RuntimeError, "did not become ready within 1.0s"):
                _wait_for_node_ready(
                    "/dev/ttyUSB1", 115200, 1, 0.25, 1.0, 0.25
                )

    def test_restart_failure_still_requests_recorder_recovery(self) -> None:
        config = {
            "supervisor": {
                "status_file": "/tmp/supervisor.json",
                "channel_runtime_dir": "/tmp/channels",
            },
            "channels": [
                {"name": "line-b", "port": "/dev/ttyUSB1", "baud": 115200}
            ],
        }

        with (
            patch("host.host_channel_control.build_parser") as parser,
            patch("host.host_channel_control._resolve_system_config"),
            patch("host.host_channel_control._load_system_config", return_value=config),
            patch("host.host_channel_control._supervisor_status_path"),
            patch("host.host_channel_control._runtime_dir"),
            patch("host.host_channel_control._write_channel_command") as write_command,
            patch("host.host_channel_control._wait_for_channel_running", return_value=True),
            patch("host.host_channel_control._restart_node", side_effect=RuntimeError("node failed")),
        ):
            parser.return_value.parse_args.return_value = build_parser().parse_args(
                ["restart-remote", "--channel", "line-b", "--settle-ms", "0"]
            )
            with self.assertRaisesRegex(RuntimeError, "node failed"):
                main()

        self.assertEqual(
            [call.args[2] for call in write_command.call_args_list],
            ["stop", "start"],
        )


if __name__ == "__main__":
    unittest.main()
