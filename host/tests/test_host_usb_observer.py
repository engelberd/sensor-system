from __future__ import annotations

import struct
import unittest

from host.host_usb_observer import (
    CAPABILITIES_FORMAT,
    CMD_GET_CAPABILITIES,
    CMD_READ_SNAPSHOT,
    CMD_GET_STATS,
    SAMPLE_FORMAT,
    SNAPSHOT_HEADER_FORMAT,
    STATS_FORMAT,
    parse_capabilities,
    parse_snapshot,
    parse_stats,
)


class UsbObserverParserTests(unittest.TestCase):
    def test_capabilities(self) -> None:
        payload = struct.pack(
            CAPABILITIES_FORMAT,
            CMD_GET_CAPABILITIES, 0, 1, 4, 2, 1, 7, 0,
            0, 4, 1, 0, 32, 27, bytes.fromhex("0102030405060708"),
        )
        result = parse_capabilities(payload)
        self.assertEqual(result.node_id, 7)
        self.assertEqual(result.firmware, "0.4.1")
        self.assertEqual(result.max_wire_samples, 27)
        self.assertEqual(result.hardware_id, "0102030405060708")

    def test_snapshot(self) -> None:
        sample = struct.pack(SAMPLE_FORMAT, 123, -1, 2, -3, 4567, 8, 1, 1, 0)
        header = struct.pack(
            SNAPSHOT_HEADER_FORMAT,
            CMD_READ_SNAPSHOT, 0, 1, 0, 9, 1, 1, 0,
            struct.calcsize(SAMPLE_FORMAT), 10, 0,
        )
        result = parse_snapshot(header + sample)
        self.assertEqual(result["generation"], 9)
        self.assertEqual(result["samples"][0]["sample_seq"], 123)
        self.assertEqual(result["samples"][0]["x"], -1)
        self.assertEqual(result["samples"][0]["device_time_us"], 4567)

    def test_stats(self) -> None:
        payload = struct.pack(STATS_FORMAT, CMD_GET_STATS, 0, 1, 0, 10, 2, 3, 32)
        result = parse_stats(payload)
        self.assertEqual(result["published_batches"], 10)
        self.assertEqual(result["read_retries"], 3)

    def test_non_ok_status_is_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "status 6"):
            parse_snapshot(bytes([CMD_READ_SNAPSHOT, 6]))


if __name__ == "__main__":
    unittest.main()
