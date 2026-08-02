from __future__ import annotations

import json
import struct
import tempfile
import unittest
from pathlib import Path

from host.host_configurator import (
    CMD_GET_PERSISTENT_DIAGNOSTIC_RECORD,
    CMD_GET_FAULT_SNAPSHOT,
    CMD_READ_DIAGNOSTIC_EVENTS,
    GET_PERSISTENT_DIAGNOSTIC_RECORD_FORMAT,
    GET_PERSISTENT_DIAGNOSTIC_RECORD_FORMAT_V2,
    GET_PERSISTENT_DIAGNOSTIC_RECORD_FORMAT_V3,
    GET_FAULT_SNAPSHOT_FORMAT_V3,
    GET_STATUS_FORMAT_V2,
    GET_STATUS_FORMAT_V3,
    GET_STATUS_FORMAT_V4,
    GET_STATUS_FORMAT_V5,
    GET_STATUS_FORMAT_V6,
    GET_STATUS_FORMAT_V7,
    GET_STATUS_FORMAT_V8,
    GET_STATUS_FORMAT_V9,
    GET_CONFIG_FORMAT,
    READ_DIAGNOSTIC_EVENTS_HEADER_FORMAT,
    READ_DIAGNOSTIC_EVENT_FORMAT,
    ConfigView,
    build_parser,
    format_diagnostic_detail,
    parse_diagnostic_events,
    parse_persistent_diagnostic_record_view,
    parse_fault_snapshot_view,
    parse_status_view,
    parse_config_view,
    sync_system_config_from_device_config,
)
from host.host_lab import STATS_FORMAT_V7, parse_stats


class HostConfiguratorSyncTests(unittest.TestCase):
    def test_parse_current_config_includes_board_revision(self) -> None:
        payload = struct.pack(
            GET_CONFIG_FORMAT,
            0x20, 0, 1, 115200, 250, 2,
            0, 0, 0, 30, 0, 1, 0, 1, 2, 7, 1234, 2,
        )
        config = parse_config_view(payload)
        self.assertEqual(config.board_revision, 2)

    def test_sync_system_config_updates_confirmed_device_settings(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            config_path = Path(tmp) / "system_config.json"
            config_path.write_text(
                json.dumps(
                    {
                        "system": {"name": "sensor-system-production"},
                        "storage": {"root_dir": "runs"},
                        "supervisor": {"status_file": "/tmp/status.json"},
                        "channels": [
                            {
                                "name": "line-a",
                                "port": "/dev/ttyACM0",
                                "baud": 115200,
                                "nodes": [
                                    {
                                        "id": 1,
                                        "name": "Czujnik 1",
                                        "enabled": True,
                                        "expected_odr_hz": 500,
                                    }
                                ],
                            }
                        ],
                    }
                )
                + "\n",
                encoding="utf-8",
            )

            result = sync_system_config_from_device_config(
                config_path,
                port="/dev/ttyACM0",
                previous_node_id=1,
                updated=ConfigView(
                    node_id=1,
                    baudrate=57600,
                    odr_hz=125,
                    range_g=4,
                    offset_x=11,
                    offset_y=22,
                    offset_z=33,
                    fifo_watermark=30,
                    act_threshold=0,
                    act_count=0,
                    high_pass_corner=3,
                ),
            )

            self.assertEqual(result.channel_name, "line-a")
            saved = json.loads(config_path.read_text(encoding="utf-8"))
            channel = saved["channels"][0]
            node = channel["nodes"][0]
            self.assertEqual(channel["baud"], 57600)
            self.assertEqual(node["id"], 1)
            self.assertEqual(node["expected_odr_hz"], 62.5)
            self.assertEqual(node["sensor_odr_hz"], 125)
            self.assertEqual(node["range_g"], 4)
            self.assertEqual(node["high_pass_corner"], 3)
            self.assertEqual(node["fifo_watermark"], 30)
            self.assertEqual(node["offset_x"], 11)
            self.assertEqual(node["offset_y"], 22)
            self.assertEqual(node["offset_z"], 33)

    def test_set_odr_parser_rejects_disabled_lower_odr_value(self) -> None:
        with self.assertRaises(SystemExit):
            build_parser().parse_args(["set-odr", "62.5"])

    def test_set_baudrate_parser_rejects_disabled_higher_baudrate(self) -> None:
        with self.assertRaises(SystemExit):
            build_parser().parse_args(["set-baudrate", "230400"])

    def test_parse_extended_status_view(self) -> None:
        payload = struct.pack(
            GET_STATUS_FORMAT_V5,
            0x40,
            0,
            7,
            2,
            125,
            4,
            2,
            0x00010203,
            9,
            12345,
            67890,
            321,
            36,
            3,
            0x3F,
            12,
            2,
            5,
            9,
            101,
            202,
            303,
            404,
            0x01021E01,
        )
        status = parse_status_view(payload)
        self.assertEqual(status.node_id, 7)
        self.assertEqual(status.uptime_ms, 12345)
        self.assertEqual(status.last_sample_seq, 67890)
        self.assertEqual(status.last_progress_ms_ago, 321)
        self.assertEqual(status.last_error_code, 36)
        self.assertEqual(status.reset_cause, 3)
        self.assertEqual(status.diagnostic_flags, 0x3F)
        self.assertEqual(status.fifo_poll_fallback_reads, 12)
        self.assertEqual(status.soft_recover_count, 2)
        self.assertEqual(status.no_data_with_irq, 5)
        self.assertEqual(status.no_data_without_irq, 9)
        self.assertEqual(status.irq_int1_events, 101)
        self.assertEqual(status.irq_drdy_events, 202)
        self.assertEqual(status.gpio_int1_edges, 303)
        self.assertEqual(status.gpio_drdy_edges, 404)
        self.assertEqual(status.debug_config_snapshot, 0x01021E01)

    def test_parse_v6_status_view_includes_irq_debug_fields(self) -> None:
        payload = struct.pack(
            GET_STATUS_FORMAT_V6,
            0x40,
            0,
            7,
            2,
            125,
            4,
            2,
            0x00010203,
            9,
            12345,
            67890,
            321,
            36,
            3,
            0x3F,
            12,
            2,
            5,
            9,
            101,
            202,
            303,
            404,
            0x01021E01,
            33,
            44,
            55,
            0x08071E02,
        )
        status = parse_status_view(payload)
        self.assertEqual(status.irq_status_not_full, 33)
        self.assertEqual(status.irq_fifo_entries_lt_3, 44)
        self.assertEqual(status.irq_fifo_entries_lt_watermark, 55)
        self.assertEqual(status.debug_irq_snapshot, 0x08071E02)

    def test_parse_v7_status_view_includes_loss_accounting(self) -> None:
        payload = struct.pack(
            GET_STATUS_FORMAT_V7,
            0x40, 0, 7, 2, 250, 2, 2, 0x00030700, 9,
            12345, 67890, 3, 0, 1, 0,
            12, 2, 5, 9, 101, 0, 303, 0, 0x00001E06,
            33, 44, 55, 0x0C1E0000,
            77, 2, 4, 1,
        )
        status = parse_status_view(payload)
        self.assertEqual(status.spurious_int1_events, 77)
        self.assertEqual(status.fifo_overrun_events, 2)
        self.assertEqual(status.fifo_discarded_samples, 4)
        self.assertEqual(status.fifo_uncertain_loss_events, 1)

    def test_parse_v8_status_view_includes_timing_diagnostics(self) -> None:
        payload = struct.pack(
            GET_STATUS_FORMAT_V8,
            0x40, 0, 7, 2, 250, 2, 2, 0x00030700, 9,
            12345, 67890, 3, 0, 1, 0,
            12, 2, 5, 9, 101, 0, 303, 0, 0x00001E06,
            33, 44, 55, 0x0C1E0000,
            77, 2, 4, 1,
            8, 9, 10, 11,
        )
        status = parse_status_view(payload)
        self.assertEqual(status.drdy_timestamp_ring_overflow, 8)
        self.assertEqual(status.timing_binding_mismatch, 9)
        self.assertEqual(status.timing_binding_invalidations, 10)
        self.assertEqual(status.timing_segment_id, 11)

    def test_parse_v9_status_view_includes_board_revision(self) -> None:
        payload = struct.pack(
            GET_STATUS_FORMAT_V9,
            0x40, 0, 7, 2, 250, 2, 4, 0x00040000, 9,
            12345, 67890, 3, 0, 1, 0,
            12, 2, 5, 9, 101, 0, 303, 0, 0x00001E06,
            33, 44, 55, 0x0C1E0000,
            77, 2, 4, 1,
            8, 9, 10, 11,
            2,
        )
        status = parse_status_view(payload)
        self.assertEqual(status.board_revision, 2)

    def test_parse_v2_status_view_keeps_new_fields_defaulted(self) -> None:
        payload = struct.pack(
            GET_STATUS_FORMAT_V2,
            0x40,
            0,
            7,
            2,
            125,
            4,
            2,
            0x00010203,
            9,
            12345,
            67890,
            321,
            36,
            3,
            0x0F,
        )
        status = parse_status_view(payload)
        self.assertEqual(status.fifo_poll_fallback_reads, 0)
        self.assertEqual(status.soft_recover_count, 0)

    def test_parse_v3_status_view_keeps_irq_source_counts_defaulted(self) -> None:
        payload = struct.pack(
            GET_STATUS_FORMAT_V3,
            0x40,
            0,
            7,
            2,
            125,
            4,
            2,
            0x00010203,
            9,
            12345,
            67890,
            321,
            36,
            3,
            0x3F,
            12,
            2,
            5,
            9,
        )
        status = parse_status_view(payload)
        self.assertEqual(status.irq_int1_events, 0)
        self.assertEqual(status.irq_drdy_events, 0)

    def test_parse_v4_status_view_keeps_gpio_debug_fields_defaulted(self) -> None:
        payload = struct.pack(
            GET_STATUS_FORMAT_V4,
            0x40,
            0,
            7,
            2,
            125,
            4,
            2,
            0x00010203,
            9,
            12345,
            67890,
            321,
            36,
            3,
            0x3F,
            12,
            2,
            5,
            9,
            101,
            202,
        )
        status = parse_status_view(payload)
        self.assertEqual(status.gpio_int1_edges, 0)
        self.assertEqual(status.gpio_drdy_edges, 0)
        self.assertEqual(status.debug_config_snapshot, 0)

    def test_parse_diagnostic_events(self) -> None:
        payload = struct.pack(
            READ_DIAGNOSTIC_EVENTS_HEADER_FORMAT,
            CMD_READ_DIAGNOSTIC_EVENTS,
            0,
            2,
            0,
            11,
            13,
        )
        payload += struct.pack(
            READ_DIAGNOSTIC_EVENT_FORMAT,
            11,
            1000,
            34,
            2,
            0,
            555,
            8,
            0,
        )
        payload += struct.pack(
            READ_DIAGNOSTIC_EVENT_FORMAT,
            12,
            2000,
            37,
            1,
            1,
            777,
            8,
            1,
        )

        first_event_id, next_event_id, events = parse_diagnostic_events(payload)
        self.assertEqual(first_event_id, 11)
        self.assertEqual(next_event_id, 13)
        self.assertEqual(len(events), 2)
        self.assertEqual(events[0].event_code, 34)
        self.assertEqual(events[0].repeat_count, 0)
        self.assertEqual(events[0].sample_seq, 555)
        self.assertEqual(events[1].event_code, 37)
        self.assertEqual(events[1].repeat_count, 1)
        self.assertEqual(events[1].arg1, 1)

    def test_parse_stats_v3(self) -> None:
        payload = struct.pack(
            "<BBQ" + ("I" * 12) + "Q" + ("I" * 9),
            0x43,
            0,
            1001,
            1000,
            3,
            4,
            5000,
            100,
            7,
            8,
            90,
            91,
            92,
            10,
            11,
            999,
            1234,
            2,
            1,
            12,
            13,
            14,
            15,
            1600,
            1700,
        )
        stats = parse_stats(payload)
        self.assertEqual(stats.next_sample_seq, 1001)
        self.assertEqual(stats.fifo_poll_fallback_reads, 12)
        self.assertEqual(stats.no_data_with_irq, 13)
        self.assertEqual(stats.no_data_without_irq, 14)
        self.assertEqual(stats.soft_recover_count, 15)
        self.assertEqual(stats.last_irq_event_ms, 1600)
        self.assertEqual(stats.last_soft_recover_ms, 1700)

    def test_parse_stats_v4(self) -> None:
        payload = struct.pack(
            "<BBQ" + ("I" * 14) + "Q" + ("I" * 9),
            0x43,
            0,
            1001,
            1000,
            3,
            4,
            5000,
            100,
            7,
            8,
            90,
            91,
            92,
            55,
            66,
            10,
            11,
            999,
            1234,
            2,
            1,
            12,
            13,
            14,
            15,
            1600,
            1700,
        )
        stats = parse_stats(payload)
        self.assertEqual(stats.fifo_int1_events, 55)
        self.assertEqual(stats.fifo_drdy_events, 66)
        self.assertEqual(stats.rx_overflow_count, 10)
        self.assertEqual(stats.packet_overwrite_count, 11)

    def test_parse_stats_v5(self) -> None:
        payload = struct.pack(
            "<BBQ" + ("I" * 17) + "Q" + ("I" * 9),
            0x43,
            0,
            1001,
            1000,
            3,
            4,
            5000,
            100,
            7,
            8,
            90,
            91,
            92,
            55,
            66,
            10,
            11,
            999,
            1234,
            2,
            1,
            12,
            13,
            14,
            15,
            1600,
            1700,
            303,
            404,
            0x01021E01,
        )
        stats = parse_stats(payload)
        self.assertEqual(stats.gpio_int1_edges, 303)
        self.assertEqual(stats.gpio_drdy_edges, 404)
        self.assertEqual(stats.debug_config_snapshot, 0x01021E01)

    def test_parse_stats_v6(self) -> None:
        payload = struct.pack(
            "<BBQ" + ("I" * 17) + "Q" + ("I" * 13),
            0x43,
            0,
            1001,
            1000,
            3,
            4,
            5000,
            100,
            7,
            8,
            90,
            91,
            92,
            55,
            66,
            10,
            11,
            999,
            1234,
            2,
            1,
            12,
            13,
            14,
            15,
            1600,
            1700,
            303,
            404,
            0x01021E01,
            33,
            44,
            55,
            0x08071E02,
        )
        stats = parse_stats(payload)
        self.assertEqual(stats.irq_status_not_full, 33)
        self.assertEqual(stats.irq_fifo_entries_lt_3, 44)
        self.assertEqual(stats.irq_fifo_entries_lt_watermark, 55)
        self.assertEqual(stats.debug_irq_snapshot, 0x08071E02)

    def test_parse_stats_v7_includes_loss_accounting(self) -> None:
        payload = struct.pack(
            STATS_FORMAT_V7,
            0x43, 0, 1001,
            1000, 3, 4, 5000, 100, 7, 8, 90, 91, 92, 55, 66, 10, 11,
            999,
            1234, 2, 1, 12, 13, 14, 15, 1600, 1700, 303, 404,
            0x01021E01, 33, 44, 55, 0x08071E02,
            77, 2, 4, 1,
        )
        stats = parse_stats(payload)
        self.assertEqual(stats.spurious_int1_events, 77)
        self.assertEqual(stats.fifo_overrun_events, 2)
        self.assertEqual(stats.fifo_discarded_samples, 4)
        self.assertEqual(stats.fifo_uncertain_loss_events, 1)

    def test_dump_diagnostics_parser_accepts_limit(self) -> None:
        args = build_parser().parse_args(["dump-diagnostics", "--start-event-id", "7", "--limit", "12"])
        self.assertEqual(args.command, "dump-diagnostics")
        self.assertEqual(args.start_event_id, 7)
        self.assertEqual(args.limit, 12)

    def test_parse_persistent_diagnostic_record(self) -> None:
        payload = struct.pack(
            GET_PERSISTENT_DIAGNOSTIC_RECORD_FORMAT_V3,
            CMD_GET_PERSISTENT_DIAGNOSTIC_RECORD,
            0,
            3,
            9,
            0x00030201,
            41,
            2222,
            34,
            2,
            7,
            3,
            0,
            0,
            123456,
            50,
            11,
            12,
            13,
            14,
            15,
            303,
            404,
            0x01021E01,
            0x08071E02,
            16,
            17,
        )
        record = parse_persistent_diagnostic_record_view(payload)
        self.assertEqual(record.generation, 3)
        self.assertEqual(record.boot_counter, 9)
        self.assertEqual(record.firmware_version, 0x00030201)
        self.assertEqual(record.event_code, 34)
        self.assertEqual(record.repeat_count, 7)
        self.assertEqual(record.reset_cause, 3)
        self.assertEqual(record.sample_seq, 123456)
        self.assertEqual(record.arg1, 17)
        self.assertEqual(record.debug_gpio_int1_edges, 303)
        self.assertEqual(record.debug_gpio_drdy_edges, 404)
        self.assertEqual(record.debug_config_snapshot, 0x01021E01)
        self.assertEqual(record.debug_irq_snapshot, 0x08071E02)

    def test_parse_fault_snapshot_matches_current_firmware_field_order(self) -> None:
        payload = struct.pack(
            GET_FAULT_SNAPSHOT_FORMAT_V3,
            CMD_GET_FAULT_SNAPSHOT, 0, 41, 2222, 36, 3, 2, 123456,
            50, 11, 12, 13, 14, 15,
            303, 404, 0x01021E01, 0x08071E02,
            16, 17,
        )
        snapshot = parse_fault_snapshot_view(payload)
        self.assertEqual(snapshot.arg0, 16)
        self.assertEqual(snapshot.arg1, 17)
        self.assertEqual(snapshot.debug_gpio_int1_edges, 303)
        self.assertEqual(snapshot.debug_gpio_drdy_edges, 404)
        self.assertEqual(snapshot.debug_config_snapshot, 0x01021E01)
        self.assertEqual(snapshot.debug_irq_snapshot, 0x08071E02)

    def test_format_diagnostic_detail_decodes_fifo_no_data_snapshot(self) -> None:
        packed_snapshot = (
            0x12 |
            (6 << 8) |
            (8 << 16) |
            ((0x80 | 0x01) << 24)
        )
        detail = format_diagnostic_detail(34, packed_snapshot, 16)
        self.assertIsNotNone(detail)
        assert detail is not None
        self.assertIn("status_reg=0x12", detail)
        self.assertIn("fifo_entries=6", detail)
        self.assertIn("fifo_read_status=NoData", detail)
        self.assertIn("irq_seen=True", detail)
        self.assertIn("empty_entry=True", detail)
        self.assertIn("no_data_streak=16", detail)


if __name__ == "__main__":
    unittest.main()
