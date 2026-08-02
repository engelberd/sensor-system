from __future__ import annotations

from pathlib import Path
from argparse import Namespace
from datetime import datetime, timezone
import tempfile
from types import SimpleNamespace
import unittest

from host.recorder.capture_reader import CaptureV1Reader
from host.recorder.capture_v1 import CaptureV1Writer, SIGNED24_MAX
from host.recorder.capture_windowing import CaptureV1WindowedWriter
from host.recorder.contracts import SensorIdentity
from host.recorder.model import QualityEventRecord, RecorderNode, SampleRecord


class CaptureV1Tests(unittest.TestCase):
    def make_node(self) -> RecorderNode:
        return RecorderNode(
            node_id=1,
            firmware_version="v0.4.0",
            config=SimpleNamespace(
                odr_hz=250,
                range_g=2,
                high_pass_corner=0,
                offset_x=1,
                offset_y=-2,
                offset_z=3,
                filter_profile=1,
                decimation_factor=2,
                config_revision=7,
                config_effective_sample_seq=100,
            ),
        )

    def make_samples(self) -> list[SampleRecord]:
        return [
            SampleRecord(
                node_id=1,
                sample_seq=100 + index,
                raw_x=10 + index,
                raw_y=-20 - index,
                raw_z=30 + index,
                packet_seq=5 if index < 2 else 6,
                range_g=2,
                device_time_us=1_000_000 + index * 8_000,
                boot_epoch=77,
                timing_segment_id=3,
                timing_quality_flags=1,
                timing_format_version=2,
                timestamp_source=1,
                sample_period_q16_us=8_000 << 16,
                max_fit_residual_us=2,
                acquisition_utc_ns=2_000_000_000 + index * 8_000_000,
                timing_uncertainty_ns=20_000_000,
            )
            for index in range(4)
        ]

    def test_round_trip_raw_identity_and_si_view(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "capture.h5"
            writer = CaptureV1Writer(
                path,
                SensorIdentity.temporary(1, node_address=1),
                compression="gzip",
            )
            writer.add_node(self.make_node())
            samples = self.make_samples()
            writer.write_samples(1, samples)
            writer.write_samples(1, samples)
            writer.write_quality_event(
                1,
                QualityEventRecord(
                    boot_epoch=77,
                    sample_seq_anchor=103,
                    observed_utc_ns=2_100_000_000,
                    event_code=2,
                    severity=2,
                    count=4,
                ),
            )
            writer.finalize()
            writer.close()

            with CaptureV1Reader(path) as reader:
                report = reader.validate()
                self.assertTrue(report.valid, report.errors)
                self.assertEqual(report.sample_count, 4)
                self.assertEqual(report.block_count, 2)
                self.assertEqual(reader.file["quality/events"].shape[0], 1)
                self.assertEqual(reader.raw_xyz().tolist(), [
                    [10, -20, 30],
                    [11, -21, 31],
                    [12, -22, 32],
                    [13, -23, 33],
                ])
                boots, sequences = reader.sample_identity()
                self.assertEqual(boots.tolist(), [77, 77, 77, 77])
                self.assertEqual(sequences.tolist(), [100, 101, 102, 103])
                self.assertAlmostEqual(
                    float(reader.acceleration_m_s2()[0, 0]),
                    samples[0].x,
                    places=12,
                )

    def test_append_recovers_uncommitted_raw_tail(self) -> None:
        import h5py

        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "capture.h5.partial"
            identity = SensorIdentity.temporary(1, node_address=1)
            writer = CaptureV1Writer(path, identity, compression="none")
            writer.add_node(self.make_node())
            writer.write_samples(1, self.make_samples()[:2])
            writer.close()

            with h5py.File(path, "a") as handle:
                raw = handle["measurement/raw_xyz"]
                raw.resize((3, 3))
                raw[2] = (999, 999, 999)

            recovered = CaptureV1Writer(
                path,
                identity,
                compression="none",
                append=True,
            )
            recovered.add_node(self.make_node())
            self.assertEqual(recovered.raw_xyz.shape, (2, 3))
            recovered.write_samples(1, self.make_samples()[2:])
            recovered.finalize()
            recovered.close()

            with CaptureV1Reader(path) as reader:
                self.assertTrue(reader.validate().valid)
                self.assertEqual(reader.raw_xyz().shape, (4, 3))

    def test_rejects_out_of_range_signed24_value(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            writer = CaptureV1Writer(
                Path(tmp) / "capture.h5.partial",
                SensorIdentity.temporary(1, node_address=1),
                compression="none",
            )
            writer.add_node(self.make_node())
            sample = self.make_samples()[0]
            sample.raw_x = SIGNED24_MAX + 1
            with self.assertRaisesRegex(ValueError, "signed-24"):
                writer.write_samples(1, [sample])
            writer.close()

    def test_reader_rejects_unfinalized_file_by_default(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "capture.h5.partial"
            writer = CaptureV1Writer(
                path,
                SensorIdentity.temporary(1, node_address=1),
                compression="none",
            )
            writer.add_node(self.make_node())
            writer.close()
            with self.assertRaisesRegex(RuntimeError, "not complete"):
                CaptureV1Reader(path)

    def test_windowing_stores_every_sample_once_without_reason_copies(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            args = Namespace(
                output_dir=tmp,
                window_seconds=600,
                compression="none",
            )
            identity = SensorIdentity.temporary(1, node_address=1)
            node = self.make_node()
            writer = CaptureV1WindowedWriter(args, {}, [node], identity)
            boundary_ns = int(
                datetime(2026, 8, 2, 12, 10, tzinfo=timezone.utc).timestamp()
                * 1_000_000_000
            )
            samples = self.make_samples()[:3]
            samples[0].acquisition_utc_ns = boundary_ns - 1_000_000
            samples[0].timing_uncertainty_ns = 2_000_000
            samples[1].acquisition_utc_ns = boundary_ns + 1_000_000
            samples[1].timing_uncertainty_ns = 2_000_000
            samples[2].acquisition_utc_ns = None
            samples[2].timing_uncertainty_ns = None
            writer.write_samples(1, samples)
            writer.close()

            paths = sorted(Path(tmp).rglob("*.capture.h5"))
            self.assertEqual(len(paths), 3)
            total = 0
            for path in paths:
                with CaptureV1Reader(path) as reader:
                    report = reader.validate()
                    self.assertTrue(report.valid, report.errors)
                    total += report.sample_count
            self.assertEqual(total, 3)

    def test_retransmission_after_window_publication_is_not_unresolved_copy(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            args = Namespace(output_dir=tmp, window_seconds=600, compression="none")
            identity = SensorIdentity.temporary(1, node_address=1)
            node = self.make_node()
            first = CaptureV1WindowedWriter(args, {}, [node], identity)
            sample = self.make_samples()[0]
            first.write_samples(1, [sample])
            first.close()

            retry = CaptureV1WindowedWriter(args, {}, [node], identity)
            retry.write_samples(1, [sample])
            retry.close()

            paths = sorted(Path(tmp).rglob("*.capture.h5"))
            self.assertEqual(len(paths), 1)
            with CaptureV1Reader(paths[0]) as reader:
                self.assertEqual(reader.validate().sample_count, 1)


if __name__ == "__main__":
    unittest.main()
