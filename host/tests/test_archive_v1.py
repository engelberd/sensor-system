from __future__ import annotations

from datetime import date
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

from host.recorder.archive_reader import ArchiveV1Reader
from host.recorder.archive_v1 import ArchiveV1Compactor
from host.recorder.capture_reader import CaptureV1Reader
from host.recorder.capture_v1 import CaptureV1Writer
from host.recorder.contracts import SensorIdentity
from host.recorder.model import RecorderNode, SampleRecord
from host.recorder.verification import verify_hdf5


class ArchiveV1Tests(unittest.TestCase):
    def make_capture(
        self, path: Path, *, window_ns: int, first_seq: int, first_utc_ns: int
    ) -> None:
        node = RecorderNode(
            node_id=1,
            firmware_version="v0.4.0",
            config=SimpleNamespace(
                odr_hz=250, range_g=2, high_pass_corner=0,
                offset_x=1, offset_y=-2, offset_z=3,
                filter_profile=1, decimation_factor=2,
                config_revision=7, config_effective_sample_seq=0,
            ),
        )
        writer = CaptureV1Writer(
            path,
            SensorIdentity.temporary(1, 1),
            metadata={
                "time_assignment": "utc_window",
                "window_start_utc_ns": window_ns,
                "window_end_utc_ns": window_ns + 600_000_000_000,
            },
            compression="none",
        )
        writer.add_node(node)
        samples = [
            SampleRecord(
                node_id=1, sample_seq=first_seq + index,
                raw_x=first_seq + index, raw_y=-(first_seq + index),
                raw_z=100 + index, packet_seq=index // 2,
                range_g=2, device_time_us=1_000_000 + index * 8_000,
                boot_epoch=77, timing_segment_id=3,
                timing_quality_flags=1, timing_format_version=2,
                timestamp_source=1, sample_period_q16_us=8_000 << 16,
                max_fit_residual_us=2,
                acquisition_utc_ns=first_utc_ns + index * 8_000_000,
                timing_uncertainty_ns=20_000_000,
                routing_flags=1,
            )
            for index in range(4)
        ]
        writer.write_samples(1, samples)
        writer.finalize()
        writer.close()

    def test_compaction_is_bit_exact_complete_and_manifested(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            day_start = 1_775_347_200_000_000_000  # 2026-04-05 UTC
            first = root / "one.capture.h5"
            second = root / "two.capture.h5"
            self.make_capture(
                first, window_ns=day_start, first_seq=10,
                first_utc_ns=day_start + 1_000_000_000,
            )
            self.make_capture(
                second, window_ns=day_start + 600_000_000_000,
                first_seq=14,
                first_utc_ns=day_start + 601_000_000_000,
            )
            output = root / "sensor-A_2026-04-05.archive.h5"
            report = ArchiveV1Compactor().build(
                [second, first], output, archive_day=date(2026, 4, 5),
                compression="none",
            )
            self.assertEqual(report.sample_count, 8)
            self.assertTrue(report.manifest.is_file())

            with CaptureV1Reader(first) as a, CaptureV1Reader(second) as b:
                expected = a.np.concatenate((a.raw_xyz(), b.raw_xyz()))
            with ArchiveV1Reader(output) as reader:
                validation = reader.validate()
                self.assertTrue(validation.valid, validation.errors)
                self.assertEqual(validation.source_count, 2)
                self.assertTrue(reader.np.array_equal(reader.raw_xyz(), expected))
                self.assertAlmostEqual(
                    float(reader.acceleration_m_s2()[0, 0]),
                    float(expected[0, 0]) * 3.9e-6 * 9.80665,
                    places=12,
                )
                self.assertEqual(
                    sum(int(row["sample_count"]) for row in reader.file["quality/intervals"]),
                    8,
                )
            verification = verify_hdf5(output)
            self.assertTrue(verification.valid, verification.errors)
            self.assertAlmostEqual(
                verification.diagnostics["timing"]["observed_odr_hz"],
                125.0,
            )

    def test_rejects_duplicate_sample_identity_without_publishing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            day_start = 1_775_347_200_000_000_000
            first = root / "one.capture.h5"
            duplicate = root / "duplicate.capture.h5"
            self.make_capture(first, window_ns=day_start, first_seq=10,
                              first_utc_ns=day_start + 1_000_000_000)
            self.make_capture(duplicate, window_ns=day_start + 600_000_000_000,
                              first_seq=10,
                              first_utc_ns=day_start + 601_000_000_000)
            output = root / "bad.archive.h5"
            with self.assertRaisesRegex(RuntimeError, "duplicate sample"):
                ArchiveV1Compactor().build(
                    [first, duplicate], output,
                    archive_day=date(2026, 4, 5), compression="none",
                )
            self.assertFalse(output.exists())
            self.assertFalse(Path(str(output) + ".partial").exists())


if __name__ == "__main__":
    unittest.main()
