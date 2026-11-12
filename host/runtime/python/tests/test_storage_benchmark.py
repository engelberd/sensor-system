import tempfile
from pathlib import Path
import unittest

from tools.storage_benchmark import (
    HEADER,
    recover_wal,
    sample_blocks,
    source_digest,
    unshuffle_i32,
    shuffle_i32,
    write_wal,
)


class StorageBenchmarkTests(unittest.TestCase):
    def test_source_stream_does_not_depend_on_block_size(self) -> None:
        self.assertEqual(
            source_digest(10_003, 257, "vibration"),
            source_digest(10_003, 4096, "vibration"),
        )

    def test_i32_shuffle_round_trips_exact_values(self) -> None:
        samples = next(sample_blocks(1024, 1024, "noise"))
        decoded = unshuffle_i32(shuffle_i32(samples), len(samples))
        self.assertTrue((decoded == samples).all())

    def test_wal_recovery_stops_before_truncated_record(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "capture.wal"
            write_wal(path, 1000, 48, "vibration", fsync_blocks=8)
            complete_digest, complete_cursor, complete_end = recover_wal(path, 1000)
            self.assertEqual(complete_cursor, 1000)
            self.assertEqual(complete_end, path.stat().st_size)

            with path.open("r+b") as stream:
                stream.truncate(path.stat().st_size - 7)
            recovered_digest, recovered_cursor, valid_end = recover_wal(path, 1000)
            self.assertLess(recovered_cursor, complete_cursor)
            self.assertGreater(recovered_cursor, 0)
            self.assertLess(valid_end, path.stat().st_size)
            self.assertNotEqual(recovered_digest, complete_digest)
            self.assertGreaterEqual(valid_end, HEADER.size)


if __name__ == "__main__":
    unittest.main()
