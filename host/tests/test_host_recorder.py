from __future__ import annotations

import unittest
from argparse import Namespace
from collections import deque
from datetime import datetime, timezone
from pathlib import Path
import struct
import tempfile
from types import SimpleNamespace
from unittest.mock import patch

from host.common.system_config import HostSystemConfig
from host.host_recorder import (
    AcquisitionWindowedWriter,
    WindowedWriter,
    emit_sample_flow_changes,
    emit_stats_changes,
    ensure_storage_reserve,
    maybe_refresh_window_start_temperature,
    resolve_window_timezone,
    update_rate_metrics,
    run_recorder,
)
from host.recorder.capture_writers import Hdf5Writer
from host.recorder.decoder import raw_lsb_to_m_s2
from host.recorder.model import RecorderNode, SampleRecord
from host.recorder.ports import StorageError
from host.host_lab import (
    BURST_HEADER_FORMAT,
    BURST_TIMING_V2_FORMAT,
    CAPABILITY_BURST_TIME_V2,
    CAPABILITY_TIME_SYNC_V1,
    CMD_GET_CAPABILITIES,
    CMD_GRANT_BURST_READ,
    CMD_TIME_SYNC,
    GET_CAPABILITIES_FORMAT,
    SAMPLE_ENCODING_RAW_XYZ24_TIME_V2,
    STATUS_OK,
    TIME_SYNC_RESPONSE_FORMAT,
    TIMING_QUALITY_INVALID,
    parse_capabilities,
    parse_burst_packet,
    parse_time_sync_response,
)


class RecorderWindowingTests(unittest.TestCase):
    def test_storage_reserve_fails_before_disk_is_completely_full(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            with patch(
                "host.host_recorder.shutil.disk_usage",
                return_value=SimpleNamespace(free=1024),
            ):
                with self.assertRaisesRegex(StorageError, "reserve breached"):
                    ensure_storage_reserve(Path(tmp) / "future", 2048)

    def test_preflight_storage_failure_uses_fatal_storage_exit_code(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            args = SimpleNamespace(
                status_file=str(Path(tmp) / "status.json"),
                event_log=str(Path(tmp) / "events.jsonl"),
                output_dir=str(Path(tmp) / "recordings"),
                output=None,
                channel_name="line-a",
                min_free_bytes=2048,
                port="/dev/test",
                baud=115200,
                start_from="newest",
                grant_packets=4,
                window_seconds=600,
                window_timezone_name="UTC",
                timing_mode="required",
            )
            with patch(
                "host.host_recorder.ensure_storage_reserve",
                side_effect=StorageError("reserve breached"),
            ):
                self.assertEqual(run_recorder(args), 3)

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
            timing_mode="legacy",
        )

    def test_windowed_paths_follow_configured_timezone(self) -> None:
        args = self.make_args()
        writer = WindowedWriter(args, metadata={}, nodes=[])

        now_utc = datetime(2026, 4, 30, 10, 7, 31, tzinfo=timezone.utc)
        window_start = writer.current_window(now_utc)

        self.assertEqual(window_start, datetime(2026, 4, 30, 10, 0, 0, tzinfo=timezone.utc))
        self.assertEqual(writer.window_end_for(now_utc), datetime(2026, 4, 30, 10, 10, 0, tzinfo=timezone.utc))
        self.assertEqual(
            str(writer.window_path_for(window_start)),
            "runs/sensor-system/2026-04-30/line-a_2026-04-30_12-00-00.h5",
        )

    def test_daily_paths_keep_exact_start_time_in_filename(self) -> None:
        args = self.make_args(
            output_dir="runs/archive",
            window_seconds=86400,
            channel_name="line-archive",
        )
        writer = WindowedWriter(args, metadata={}, nodes=[])

        now_utc = datetime(2026, 4, 29, 22, 30, 45, tzinfo=timezone.utc)
        window_start = writer.current_window(now_utc)

        self.assertEqual(window_start, datetime(2026, 4, 29, 22, 0, 0, tzinfo=timezone.utc))
        self.assertEqual(writer.window_end_for(now_utc), datetime(2026, 4, 30, 22, 0, 0, tzinfo=timezone.utc))
        self.assertEqual(
            str(writer.window_path_for(window_start)),
            "runs/archive/2026-04-30/line-archive_2026-04-30_00-00-00.h5",
        )

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
        self.assertEqual(
            node.last_temperature_window_start,
            datetime(2026, 4, 30, 10, 0, 0, tzinfo=timezone.utc),
        )
        self.assertEqual(node.next_temperature_at, 3642.0)
        self.assertEqual(captured_events[0][0], "temperature_sampled")
        self.assertEqual(captured_events[0][1]["fields"]["reason"], "window_start")


class TimedBurstParserTests(unittest.TestCase):
    def build_timed_payload(self, *, quality: int = 1) -> bytes:
        header = struct.pack(
            BURST_HEADER_FORMAT,
            CMD_GRANT_BURST_READ,
            STATUS_OK,
            9,
            100,
            2,
            SAMPLE_ENCODING_RAW_XYZ24_TIME_V2,
        )
        timing = struct.pack(
            BURST_TIMING_V2_FORMAT,
            2,
            1,
            quality,
            0x1122334455667788,
            7,
            5_000_000,
            5_008_000,
            8_000 << 16,
            3,
        )
        return header + timing + bytes(18)

    def test_v2_parser_reconstructs_endpoint_times(self) -> None:
        packet = parse_burst_packet(self.build_timed_payload())
        self.assertIsNotNone(packet)
        assert packet is not None
        self.assertEqual(packet.boot_epoch, 0x1122334455667788)
        self.assertEqual(packet.timing_segment_id, 7)
        self.assertEqual(packet.device_time_us_for_index(0), 5_000_000)
        self.assertEqual(packet.device_time_us_for_index(1), 5_008_000)

    def test_invalid_quality_never_materializes_device_time(self) -> None:
        packet = parse_burst_packet(
            self.build_timed_payload(quality=TIMING_QUALITY_INVALID)
        )
        self.assertIsNotNone(packet)
        assert packet is not None
        self.assertIsNone(packet.device_time_us_for_index(0))

    def test_v2_parser_rejects_truncated_payload(self) -> None:
        self.assertIsNone(parse_burst_packet(self.build_timed_payload()[:-1]))

    def test_capabilities_and_time_sync_responses_are_strictly_parsed(
        self,
    ) -> None:
        capabilities = parse_capabilities(struct.pack(
            GET_CAPABILITIES_FORMAT,
            CMD_GET_CAPABILITIES,
            STATUS_OK,
            1,
            2,
            CAPABILITY_BURST_TIME_V2 | CAPABILITY_TIME_SYNC_V1,
            32,
        ))
        self.assertEqual(capabilities.burst_format_max, 2)
        self.assertEqual(capabilities.max_samples_per_packet, 32)

        response = parse_time_sync_response(struct.pack(
            TIME_SYNC_RESPONSE_FORMAT,
            CMD_TIME_SYNC,
            STATUS_OK,
            1,
            17,
            0x1234,
            9_000_000,
            1_000_000,
            1_000_010,
        ))
        self.assertEqual(response.sync_id, 17)
        self.assertEqual(response.boot_epoch, 0x1234)
        with self.assertRaises(ValueError):
            parse_time_sync_response(struct.pack(
                TIME_SYNC_RESPONSE_FORMAT,
                CMD_TIME_SYNC,
                STATUS_OK,
                1,
                17,
                0x1234,
                9_000_000,
                1_000_010,
                1_000_000,
            ))


class AcquisitionWindowWriterTests(unittest.TestCase):
    def test_splits_samples_at_half_open_boundary_and_finalizes_atomically(
        self,
    ) -> None:
        import h5py

        with tempfile.TemporaryDirectory() as tmp:
            args = RecorderWindowingTests().make_args(
                output_dir=tmp,
                window_timezone_name="UTC",
            )
            args.timing_mode = "required"
            args.compression = "none"
            node = RecorderNode(
                node_id=1,
                config=SimpleNamespace(
                    odr_hz=250,
                    range_g=2,
                    high_pass_corner=0,
                    fifo_watermark=30,
                    offset_x=0,
                    offset_y=0,
                    offset_z=0,
                ),
            )
            writer = AcquisitionWindowedWriter(
                args,
                metadata={},
                nodes=[node],
            )
            boundary_ns = int(
                datetime(
                    2026,
                    4,
                    30,
                    10,
                    10,
                    tzinfo=timezone.utc,
                ).timestamp()
                * 1_000_000_000
            )
            samples = [
                SampleRecord(
                    1, 10, 1, 2, 3, 4,
                    device_time_us=1000,
                    boot_epoch=7,
                    timing_segment_id=2,
                    timing_quality_flags=1,
                    acquisition_utc_ns=boundary_ns - 1_000_000,
                    timing_uncertainty_ns=100,
                ),
                SampleRecord(
                    1, 11, 1, 2, 3, 4,
                    device_time_us=9000,
                    boot_epoch=7,
                    timing_segment_id=2,
                    timing_quality_flags=1,
                    acquisition_utc_ns=boundary_ns,
                    timing_uncertainty_ns=0,
                ),
            ]
            writer.write_samples(1, samples)
            writer.write_samples(1, samples)
            writer.close()

            files = sorted(Path(tmp).rglob("*.h5"))
            self.assertEqual(len(files), 2)
            self.assertEqual(list(Path(tmp).rglob("*.partial")), [])
            for path in files:
                with h5py.File(path, "r") as handle:
                    self.assertEqual(handle.attrs["schema_version"], 5)
                    self.assertTrue(handle.attrs["complete"])
                    self.assertEqual(handle["nodes/1/samples"].shape[0], 1)
                    self.assertEqual(
                        handle["nodes/1/time_anchors"].shape[0],
                        1,
                    )

    def test_partial_recovery_truncates_unjournaled_dataset_tail(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "capture.h5.partial"
            node = RecorderNode(
                node_id=1,
                config=SimpleNamespace(
                    odr_hz=250,
                    range_g=2,
                    high_pass_corner=0,
                    fifo_watermark=30,
                    offset_x=0,
                    offset_y=0,
                    offset_z=0,
                ),
            )
            writer = Hdf5Writer(path, {}, "none")
            writer.add_node(node)
            sample = SampleRecord(
                1, 10, 1, 2, 3, 4,
                boot_epoch=7,
                timing_segment_id=2,
            )
            writer.write_samples(1, [sample])

            dataset = writer.datasets[1]
            dataset.resize((2,))
            dataset[1] = dataset[0]
            writer.close()

            recovered = Hdf5Writer(path, {}, "none", append=True)
            recovered.add_node(node)
            self.assertEqual(recovered.datasets[1].shape[0], 1)
            self.assertEqual(recovered.ingest_batches.shape[0], 1)
            recovered.close()

    def test_hdf_time_anchors_never_join_packets_or_segments(self) -> None:
        import h5py

        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "capture.h5"
            node = RecorderNode(
                node_id=1,
                config=SimpleNamespace(
                    odr_hz=250,
                    range_g=2,
                    high_pass_corner=0,
                    fifo_watermark=30,
                    offset_x=0,
                    offset_y=0,
                    offset_z=0,
                ),
            )
            writer = Hdf5Writer(path, {}, "none")
            writer.add_node(node)
            samples = [
                SampleRecord(
                    1, 10 + index, 1, 2, 3,
                    4 if index < 2 else 5,
                    device_time_us=1000 + index * 8000,
                    boot_epoch=7,
                    timing_segment_id=2 if index < 2 else 3,
                    timing_quality_flags=1,
                    acquisition_utc_ns=1_000_000_000 + index * 8_000_000,
                    timing_uncertainty_ns=100,
                )
                for index in range(4)
            ]
            writer.write_samples(1, samples)
            writer.finalize()
            writer.close()

            with h5py.File(path, "r") as handle:
                anchors = handle["nodes/1/time_anchors"]
                self.assertEqual(anchors.shape[0], 2)
                self.assertEqual(list(anchors["packet_seq"]), [4, 5])
                self.assertEqual(list(anchors["timing_segment_id"]), [2, 3])
                self.assertEqual(list(anchors["sample_count"]), [2, 2])
                self.assertEqual(
                    int(anchors[0]["first_acquisition_utc_ns"]),
                    1_000_000_000,
                )
                self.assertEqual(
                    int(anchors[1]["last_acquisition_utc_ns"]),
                    1_024_000_000,
                )

    def test_required_mode_quarantines_unsynced_and_ambiguous_samples(
        self,
    ) -> None:
        import h5py

        with tempfile.TemporaryDirectory() as tmp:
            args = RecorderWindowingTests().make_args(
                output_dir=tmp,
                window_timezone_name="UTC",
            )
            args.timing_mode = "required"
            args.compression = "none"
            node = RecorderNode(
                node_id=1,
                config=SimpleNamespace(
                    odr_hz=250,
                    range_g=2,
                    high_pass_corner=0,
                    fifo_watermark=30,
                    offset_x=0,
                    offset_y=0,
                    offset_z=0,
                ),
            )
            writer = AcquisitionWindowedWriter(
                args,
                metadata={},
                nodes=[node],
            )
            boundary_ns = int(
                datetime(
                    2026, 4, 30, 10, 10, tzinfo=timezone.utc
                ).timestamp() * 1_000_000_000
            )
            writer.write_samples(1, [
                SampleRecord(
                    1, 10, 1, 2, 3, 4,
                    device_time_us=1000,
                    boot_epoch=7,
                    timing_segment_id=2,
                    timing_quality_flags=1,
                ),
                SampleRecord(
                    1, 11, 1, 2, 3, 4,
                    device_time_us=9000,
                    boot_epoch=7,
                    timing_segment_id=2,
                    timing_quality_flags=1,
                    acquisition_utc_ns=boundary_ns + 1_000_000,
                    timing_uncertainty_ns=2_000_000,
                ),
            ])
            writer.close()

            files = sorted(Path(tmp).rglob("*.h5"))
            self.assertEqual(len(files), 2)
            reasons = set()
            for path in files:
                with h5py.File(path, "r") as handle:
                    self.assertTrue(handle.attrs["timing_quarantine"])
                    reasons.add(handle.attrs["timing_quarantine_reason"])
                    self.assertTrue(handle.attrs["complete"])
                    self.assertEqual(
                        handle["nodes/1/samples"].shape[0],
                        1,
                    )
            self.assertEqual(reasons, {"unsynced", "ambiguous"})


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


class RecorderRateMetricTests(unittest.TestCase):
    def test_rate_metrics_capture_recent_rate_and_stability(self) -> None:
        node = RecorderNode(
            node_id=1,
            config=SimpleNamespace(odr_hz=250, range_g=2),
            rate_history=deque(),
        )

        for moment, samples_written in [
            (0.0, 0),
            (1.0, 125),
            (2.0, 250),
            (3.0, 375),
            (4.0, 500),
            (5.0, 625),
        ]:
            node.samples_written = samples_written
            update_rate_metrics([node], moment)

        self.assertAlmostEqual(node.instant_samples_per_second_5s or 0.0, 125.0, places=3)
        self.assertAlmostEqual(node.rate_stability_percent_5s or 0.0, 100.0, places=3)

    def test_rate_metrics_drop_stability_when_rate_fluctuates(self) -> None:
        node = RecorderNode(
            node_id=1,
            config=SimpleNamespace(odr_hz=250, range_g=2),
            rate_history=deque(),
        )

        for moment, samples_written in [
            (0.0, 0),
            (1.0, 125),
            (2.0, 125),
            (3.0, 250),
            (4.0, 250),
            (5.0, 375),
        ]:
            node.samples_written = samples_written
            update_rate_metrics([node], moment)

        self.assertAlmostEqual(node.instant_samples_per_second_5s or 0.0, 75.0, places=3)
        self.assertLess(node.rate_stability_percent_5s or 100.0, 60.0)

    def test_sample_flow_emits_one_stall_and_one_recovery_event(self) -> None:
        node = RecorderNode(
            node_id=1,
            config=SimpleNamespace(odr_hz=250, range_g=2),
            online=True,
            instant_samples_per_second_5s=0.0,
            samples_written=1234,
            expected_sample_seq=1235,
        )
        captured: list[tuple[str, dict[str, object]]] = []

        class Writer:
            def emit(self, event: str, **kwargs: object) -> None:
                captured.append((event, dict(kwargs)))

        writer = Writer()
        emit_sample_flow_changes([node], 10.0, writer)
        emit_sample_flow_changes([node], 12.1, writer)
        emit_sample_flow_changes([node], 20.0, writer)

        self.assertEqual(node.sample_flow_state, "stalled")
        self.assertEqual([event for event, _ in captured], ["sample_flow_stalled"])
        self.assertEqual(captured[0][1]["severity"], "error")

        node.instant_samples_per_second_5s = 125.0
        emit_sample_flow_changes([node], 21.0, writer)

        self.assertEqual(node.sample_flow_state, "flowing")
        self.assertEqual(
            [event for event, _ in captured],
            ["sample_flow_stalled", "sample_flow_recovered"],
        )


class RecorderDiagnosticTests(unittest.TestCase):
    def test_stats_counter_increase_emits_user_visible_and_archived_alarm(self) -> None:
        node = RecorderNode(node_id=1, config=SimpleNamespace(odr_hz=250, range_g=2))
        previous = SimpleNamespace(
            dropped_samples=0,
            fifo_overrun_events=0,
            fifo_discarded_samples=0,
            fifo_uncertain_loss_events=0,
            sensor_errors=0,
            soft_recover_count=0,
        )
        node.last_stats = SimpleNamespace(
            dropped_samples=21,
            fifo_overrun_events=19,
            fifo_discarded_samples=2,
            fifo_uncertain_loss_events=0,
            sensor_errors=1,
            soft_recover_count=0,
            last_sample_seq=10260458,
        )
        visible: list[tuple[str, dict[str, object]]] = []
        archived: list[tuple[str, dict[str, object]]] = []

        class Writer:
            def __init__(self, target: list[tuple[str, dict[str, object]]]) -> None:
                self.target = target

            def emit(self, event: str, **kwargs: object) -> None:
                self.target.append((event, dict(kwargs)))

        emit_stats_changes(node, previous, Writer(visible), Writer(archived))

        self.assertEqual([event for event, _ in visible], [
            "sample_loss", "fifo_overrun", "fifo_samples_discarded", "sensor_read_error"
        ])
        self.assertEqual(visible, archived)
        self.assertEqual(visible[0][1]["severity"], "critical")
        self.assertEqual(visible[0][1]["fields"]["delta"], 21)


if __name__ == "__main__":
    unittest.main()
