from __future__ import annotations

from pathlib import Path
from argparse import Namespace
from datetime import datetime, timedelta, timezone
import tempfile
from types import SimpleNamespace
import unittest

from host.recorder.capture_reader import CaptureV1Reader
from host.recorder.capture_v1 import CaptureV1Writer, SIGNED24_MAX
from host.recorder.capture_windowing import CaptureV1WindowedWriter
from host.recorder.contracts import SensorIdentity
from host.recorder.model import QualityEventRecord, RecorderNode, SampleRecord
from host.recorder.verification import verify_hdf5
from host.recorder.ports import StorageError


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
            verification = verify_hdf5(path)
            self.assertTrue(verification.valid, verification.errors)
            self.assertAlmostEqual(
                verification.diagnostics["timing"]["observed_odr_hz"],
                125.0,
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

    def test_graceful_restart_resumes_current_partial_window(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            args = Namespace(output_dir=tmp, window_seconds=600, compression="none")
            identity = SensorIdentity.temporary(1, node_address=1)
            node = self.make_node()
            now = datetime.now(timezone.utc)
            samples = self.make_samples()[:2]
            samples[0].acquisition_utc_ns = int(now.timestamp() * 1_000_000_000)
            samples[1].acquisition_utc_ns = samples[0].acquisition_utc_ns + 8_000_000

            first = CaptureV1WindowedWriter(args, {}, [node], identity)
            first.write_samples(1, [samples[0]])
            first.close()

            partials = list(Path(tmp).rglob("*.capture.h5.partial"))
            self.assertEqual(len(partials), 1)
            self.assertEqual(list(Path(tmp).rglob("*.capture.h5")), [])

            resumed = CaptureV1WindowedWriter(args, {}, [node], identity)
            self.assertIsNotNone(resumed.current_path)
            resumed.write_samples(1, samples)
            resumed.close()

            with CaptureV1Reader(partials[0], allow_partial=True) as reader:
                report = reader.validate()
                self.assertTrue(report.valid, report.errors)
                self.assertEqual(report.sample_count, 2)

    def test_startup_publishes_stale_partial_window(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            args = Namespace(output_dir=tmp, window_seconds=600, compression="none")
            identity = SensorIdentity.temporary(1, node_address=1)
            node = self.make_node()
            stale = datetime.now(timezone.utc) - timedelta(minutes=20)
            stale = stale.replace(
                minute=(stale.minute // 10) * 10,
                second=0,
                microsecond=0,
            )
            directory = Path(tmp) / stale.strftime("%Y-%m-%d")
            final = directory / (
                f"sensor-A_{stale.strftime('%Y%m%dT%H%M%SZ')}.capture.h5"
            )
            partial = Path(str(final) + ".partial")
            raw = CaptureV1Writer(partial, identity, compression="none")
            raw.add_node(node)
            raw.close()

            recovered = CaptureV1WindowedWriter(args, {}, [node], identity)
            recovered.close()

            self.assertTrue(final.exists())
            self.assertFalse(partial.exists())
            with CaptureV1Reader(final) as reader:
                self.assertTrue(reader.validate().valid)

    def test_startup_quarantines_truncated_partial_and_continues(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            args = Namespace(output_dir=tmp, window_seconds=600, compression="none")
            identity = SensorIdentity.temporary(1, node_address=1)
            node = self.make_node()
            now = datetime.now(timezone.utc)
            window = now.replace(
                minute=(now.minute // 10) * 10,
                second=0,
                microsecond=0,
            )
            directory = Path(tmp) / window.strftime("%Y-%m-%d")
            final = directory / (
                f"sensor-A_{window.strftime('%Y%m%dT%H%M%SZ')}.capture.h5"
            )
            partial = Path(str(final) + ".partial")
            raw = CaptureV1Writer(partial, identity, compression="none")
            raw.add_node(node)
            raw.write_samples(1, self.make_samples())
            raw.close()
            with partial.open("r+b") as stream:
                stream.truncate(max(1, partial.stat().st_size - 163))

            recovered = CaptureV1WindowedWriter(args, {}, [node], identity)
            self.assertEqual(len(recovered.recovery_events), 1)
            event = recovered.recovery_events[0]
            self.assertEqual(event["event"], "partial_window_unrecoverable")
            self.assertEqual(event["reason"], "resume_failed")
            self.assertFalse(partial.exists())
            quarantine = Path(str(event["quarantine_path"]))
            self.assertTrue(quarantine.is_file())
            self.assertTrue(
                Path(str(quarantine) + ".recovery.json").is_file()
            )

            sample = self.make_samples()[0]
            sample.acquisition_utc_ns = int(now.timestamp() * 1_000_000_000)
            recovered.write_samples(1, [sample])
            recovered.close()

    def test_startup_publishes_completed_partial_without_reopening_for_append(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            args = Namespace(output_dir=tmp, window_seconds=600, compression="none")
            identity = SensorIdentity.temporary(1, node_address=1)
            node = self.make_node()
            now = datetime.now(timezone.utc)
            window = now.replace(
                minute=(now.minute // 10) * 10,
                second=0,
                microsecond=0,
            )
            directory = Path(tmp) / window.strftime("%Y-%m-%d")
            final = directory / (
                f"sensor-A_{window.strftime('%Y%m%dT%H%M%SZ')}.capture.h5"
            )
            partial = Path(str(final) + ".partial")
            raw = CaptureV1Writer(partial, identity, compression="none")
            raw.add_node(node)
            raw.write_samples(1, self.make_samples())
            raw.finalize()
            raw.close()

            recovered = CaptureV1WindowedWriter(args, {}, [node], identity)
            recovered.close()

            self.assertTrue(final.is_file())
            self.assertFalse(partial.exists())
            self.assertEqual(
                recovered.recovery_events[0]["event"],
                "partial_window_published",
            )
            with CaptureV1Reader(final) as reader:
                self.assertTrue(reader.validate().valid)

    def test_storage_failure_is_not_misclassified_as_node_failure(self) -> None:
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as tmp:
            args = Namespace(output_dir=tmp, window_seconds=600, compression="none")
            identity = SensorIdentity.temporary(1, node_address=1)
            writer = CaptureV1WindowedWriter(
                args,
                {},
                [self.make_node()],
                identity,
            )
            sample = self.make_samples()[0]
            sample.acquisition_utc_ns = int(
                datetime.now(timezone.utc).timestamp() * 1_000_000_000
            )
            with patch.object(
                writer,
                "_ensure_window",
                side_effect=OSError("disk full"),
            ):
                with self.assertRaisesRegex(StorageError, "disk full"):
                    writer.write_samples(1, [sample])
            writer.close()

    def test_identity_mismatch_partial_is_quarantined_not_retried(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            args = Namespace(output_dir=tmp, window_seconds=600, compression="none")
            now = datetime.now(timezone.utc)
            window = now.replace(
                minute=(now.minute // 10) * 10,
                second=0,
                microsecond=0,
            )
            directory = Path(tmp) / window.strftime("%Y-%m-%d")
            final = directory / (
                f"sensor-A_{window.strftime('%Y%m%dT%H%M%SZ')}.capture.h5"
            )
            partial = Path(str(final) + ".partial")
            original = CaptureV1Writer(
                partial,
                SensorIdentity.temporary(1, node_address=1),
                compression="none",
            )
            original.add_node(self.make_node())
            original.close()

            replacement_node = self.make_node()
            replacement_node.node_id = 2
            recovered = CaptureV1WindowedWriter(
                args,
                {},
                [replacement_node],
                SensorIdentity.temporary(1, node_address=2),
            )
            self.assertFalse(partial.exists())
            self.assertEqual(
                recovered.recovery_events[0]["reason"],
                "resume_failed",
            )
            self.assertIn(
                "identity",
                str(recovered.recovery_events[0]["error"]),
            )
            recovered.close()

    def test_quarantine_move_failure_is_a_fatal_storage_error(self) -> None:
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as tmp:
            args = Namespace(output_dir=tmp, window_seconds=600, compression="none")
            writer = CaptureV1WindowedWriter(
                args,
                {},
                [self.make_node()],
                SensorIdentity.temporary(1, node_address=1),
            )
            partial = Path(tmp) / "broken.capture.h5.partial"
            partial.write_bytes(b"broken")
            with patch(
                "host.recorder.capture_windowing.os.replace",
                side_effect=PermissionError("read-only filesystem"),
            ):
                with self.assertRaisesRegex(StorageError, "cannot quarantine"):
                    writer._quarantine_partial(
                        partial,
                        reason="resume_failed",
                        error="truncated",
                    )
            writer.close()


if __name__ == "__main__":
    unittest.main()
