from __future__ import annotations

import unittest
from argparse import Namespace
from datetime import datetime, timezone
from types import SimpleNamespace
from unittest.mock import patch

from host.common.system_config import HostSystemConfig
from host.host_recorder import (
    WindowedWriter,
    raw_lsb_to_m_s2,
    maybe_refresh_window_start_temperature,
    resolve_window_timezone,
)


class RecorderWindowingTests(unittest.TestCase):
    def make_args(
        self,
        *,
        output_dir: str = "runs/sensor-system",
        format_name: str = "hdf5",
        window_seconds: int = 600,
        channel_name: str = "line-a",
        window_timezone_name: str = "Europe/Warsaw",
    ) -> Namespace:
        return Namespace(
            output_dir=output_dir,
            format=format_name,
            window_seconds=window_seconds,
            channel_name=channel_name,
            window_timezone=resolve_window_timezone(window_timezone_name),
            window_timezone_name=window_timezone_name,
            output=None,
            overwrite=False,
            compression="gzip",
            timeout=0.5,
            error_sleep=0.1,
            temperature_interval=3600.0,
        )

    def test_windowed_paths_follow_configured_timezone(self) -> None:
        args = self.make_args()
        writer = WindowedWriter(args, metadata={}, nodes=[])

        now_utc = datetime(2026, 4, 30, 10, 7, 31, tzinfo=timezone.utc)
        window_start = writer.current_window(now_utc)

        self.assertEqual(window_start, datetime(2026, 4, 30, 10, 0, 0, tzinfo=timezone.utc))
        self.assertEqual(
            str(writer.window_path_for(window_start)),
            "runs/sensor-system/2026-04-30/2026-04-30_12-00.h5",
        )

    def test_daily_paths_use_local_day_boundary(self) -> None:
        args = self.make_args(
            output_dir="runs/archive",
            window_seconds=86400,
            channel_name="default",
        )
        writer = WindowedWriter(args, metadata={}, nodes=[])

        now_utc = datetime(2026, 4, 29, 22, 30, 0, tzinfo=timezone.utc)
        window_start = writer.current_window(now_utc)

        self.assertEqual(window_start, datetime(2026, 4, 29, 22, 0, 0, tzinfo=timezone.utc))
        self.assertEqual(str(writer.window_path_for(window_start)), "runs/archive/2026-04-30.h5")

    def test_window_start_temperature_uses_first_sequence_in_new_window(self) -> None:
        args = self.make_args()
        writer = WindowedWriter(args, metadata={}, nodes=[])
        node = SimpleNamespace(
            node_id=1,
            expected_sample_seq=1234,
            last_temperature_window_start=None,
            next_window_temperature_retry_at=0.0,
            next_temperature_at=0.0,
            last_temperature_c=None,
        )
        captured_events: list[tuple[str, dict[str, object]]] = []

        class EventWriter:
            def emit(self, event: str, **kwargs: object) -> None:
                captured_events.append((event, dict(kwargs)))

        now_utc = datetime(2026, 4, 30, 10, 7, 31, tzinfo=timezone.utc)

        def fake_refresh_temperature(*_args, **kwargs):
            node.last_temperature_c = 21.5
            return kwargs["sample_seq_anchor"]

        with patch("host.host_recorder.refresh_temperature", side_effect=fake_refresh_temperature) as mock_refresh:
            maybe_refresh_window_start_temperature(
                client=SimpleNamespace(),
                writer=writer,
                node=node,
                args=args,
                event_writer=EventWriter(),
                now_monotonic=42.0,
                now_utc=now_utc,
            )

        self.assertEqual(mock_refresh.call_args.kwargs["sample_seq_anchor"], 1234)
        self.assertEqual(node.last_temperature_window_start, writer.current_window(now_utc))
        self.assertEqual(node.next_temperature_at, 3642.0)
        self.assertEqual(captured_events[0][0], "temperature_sampled")
        self.assertEqual(captured_events[0][1]["fields"]["reason"], "window_start")


class SystemConfigCompatibilityTests(unittest.TestCase):
    def test_legacy_config_defaults_to_ten_minute_windows(self) -> None:
        config = HostSystemConfig.from_dict(
            {
                "serial": {"port": "/dev/sensor-system-rs485"},
                "nodes": [{"node_id": 1, "enabled": True}],
            }
        )

        self.assertEqual(config.storage.window_seconds, 600)

    def test_legacy_rotate_daily_flag_is_still_supported(self) -> None:
        config = HostSystemConfig.from_dict(
            {
                "serial": {"port": "/dev/sensor-system-rs485"},
                "storage": {"rotate_daily": True},
                "nodes": [{"node_id": 1, "enabled": True}],
            }
        )

        self.assertEqual(config.storage.window_seconds, 86400)


class RecorderScalingTests(unittest.TestCase):
    def test_raw_samples_are_scaled_to_m_s2_using_range(self) -> None:
        self.assertAlmostEqual(raw_lsb_to_m_s2(1, 2), 3.9e-6 * 9.80665, places=9)
        self.assertAlmostEqual(raw_lsb_to_m_s2(1, 4), 7.8e-6 * 9.80665, places=9)
        self.assertAlmostEqual(raw_lsb_to_m_s2(1, 8), 15.6e-6 * 9.80665, places=9)


if __name__ == "__main__":
    unittest.main()
