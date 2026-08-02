"""Deterministic Capture v1 to Archive v1 compaction.

The compactor is intentionally independent from the live recorder.  It only
accepts sealed, structurally valid Capture files and never removes its inputs.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import date, datetime, timezone
import hashlib
import os
from pathlib import Path
import time
from uuid import uuid4

from host.recorder.capture_reader import CaptureV1Reader
from host.recorder.contracts import (
    ARCHIVE_SCHEMA_MAJOR,
    ARCHIVE_SCHEMA_MINOR,
    ArchiveQualityFlag,
    CaptureRoutingFlag,
    DataProduct,
    GapKind,
    QualityEventCode,
    TimeControlMethod,
)


@dataclass(frozen=True)
class ArchiveBuildReport:
    output: Path
    manifest: Path
    sha256: str
    sample_count: int
    source_count: int
    control_point_count: int


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def _append(dataset, rows, np) -> None:
    if len(rows) == 0:
        return
    offset = int(dataset.shape[0])
    dataset.resize((offset + len(rows),) + dataset.shape[1:])
    dataset[offset:] = np.asarray(rows, dtype=dataset.dtype)


def _as_text(value: object) -> str:
    if isinstance(value, bytes):
        return value.decode("ascii", errors="strict").rstrip("\0")
    return str(value)


def _overlay_quality_flag(
    rows: list[tuple[int, int, int]],
    start: int,
    count: int,
    flag: int,
) -> list[tuple[int, int, int]]:
    """Split RLE rows only where an event changes sample quality."""
    if count <= 0:
        return rows
    stop = start + count
    result: list[tuple[int, int, int]] = []
    for row_start, row_count, row_flags in rows:
        row_stop = row_start + row_count
        overlap_start = max(start, row_start)
        overlap_stop = min(stop, row_stop)
        if overlap_start >= overlap_stop:
            result.append((row_start, row_count, row_flags))
            continue
        if row_start < overlap_start:
            result.append((row_start, overlap_start - row_start, row_flags))
        result.append((
            overlap_start, overlap_stop - overlap_start, row_flags | flag
        ))
        if overlap_stop < row_stop:
            result.append((overlap_stop, row_stop - overlap_stop, row_flags))
    merged: list[tuple[int, int, int]] = []
    for row in result:
        if merged and merged[-1][2] == row[2] and (
            merged[-1][0] + merged[-1][1] == row[0]
        ):
            prior = merged[-1]
            merged[-1] = (prior[0], prior[1] + row[1], prior[2])
        else:
            merged.append(row)
    return merged


class ArchiveV1Compactor:
    """Build one immutable daily Archive from sealed Capture inputs."""

    COMPACTOR_VERSION = "1.0"

    def __init__(self, *, max_timing_residual_ns: int = 1_000_000) -> None:
        if max_timing_residual_ns < 0:
            raise ValueError("max_timing_residual_ns must be non-negative")
        self.max_timing_residual_ns = max_timing_residual_ns

    @staticmethod
    def _source_order(reader: CaptureV1Reader) -> tuple[int, str]:
        attrs = reader.file.attrs
        return int(attrs.get("window_start_utc_ns", -1)), reader.path.name

    @staticmethod
    def _identity(reader: CaptureV1Reader) -> tuple[int, str, str, int, str, int]:
        attrs = reader.file.attrs
        return (
            int(attrs["channel_id"]),
            _as_text(attrs["sensor_label"]),
            _as_text(attrs.get("sensor_id", "")),
            int(attrs["node_address"]),
            _as_text(attrs.get("hardware_id", "")),
            int(attrs.get("board_revision", 0)),
        )

    @staticmethod
    def _configuration_key(row) -> tuple[object, ...]:
        return tuple(
            _as_text(row[name]) if row.dtype[name].kind == "S" else row[name].item()
            for name in row.dtype.names
            if name not in ("start_boot_epoch", "start_sample_seq")
        )

    @staticmethod
    def _quality_from_block(block) -> int:
        flags = ArchiveQualityFlag.GOOD
        routing = CaptureRoutingFlag(int(block["routing_flags"]))
        if int(block["first_utc_ns"]) < 0 or not (
            routing & CaptureRoutingFlag.UTC_VALID
        ):
            flags |= (
                ArchiveQualityFlag.TIME_UNCERTAIN
                | ArchiveQualityFlag.TIME_UNSYNCED
            )
        if routing & CaptureRoutingFlag.INITIALLY_UNSYNCED:
            flags |= ArchiveQualityFlag.TIME_UNSYNCED
        if routing & CaptureRoutingFlag.BOUNDARY_UNCERTAIN:
            flags |= ArchiveQualityFlag.BOUNDARY_UNCERTAIN
        # Firmware TIMING_QUALITY_INVALID is bit 2.
        if int(block["timing_flags"]) & 0x0004:
            flags |= (
                ArchiveQualityFlag.TIME_UNCERTAIN
                | ArchiveQualityFlag.MEASUREMENT_INVALID
            )
        return int(flags)

    @staticmethod
    def _simplify_control_points(candidates, tolerance_ns: int):
        """RDP-like simplification in sample-offset/UTC space per clock run."""
        if len(candidates) <= 2:
            return candidates
        keep = {0, len(candidates) - 1}
        stack = [(0, len(candidates) - 1)]
        while stack:
            left, right = stack.pop()
            a = candidates[left]
            b = candidates[right]
            span = b[0] - a[0]
            if span <= 0:
                continue
            worst_index = -1
            worst_error = -1
            for index in range(left + 1, right):
                point = candidates[index]
                predicted = a[2] + ((b[2] - a[2]) * (point[0] - a[0])) // span
                error = abs(point[2] - predicted)
                if error > worst_error:
                    worst_error = error
                    worst_index = index
            if worst_error > tolerance_ns:
                keep.add(worst_index)
                stack.append((left, worst_index))
                stack.append((worst_index, right))
        return [candidates[index] for index in sorted(keep)]

    def build(
        self,
        capture_paths: list[str | Path],
        output_path: str | Path,
        *,
        archive_day: date,
        compression: str = "gzip",
    ) -> ArchiveBuildReport:
        if not capture_paths:
            raise ValueError("at least one Capture file is required")
        output = Path(output_path)
        if output.exists():
            raise FileExistsError(output)
        manifest_path = Path(str(output) + ".sha256")
        if manifest_path.exists():
            raise FileExistsError(manifest_path)
        partial = Path(str(output) + ".partial")
        if partial.exists():
            raise FileExistsError(partial)
        output.parent.mkdir(parents=True, exist_ok=True)

        readers: list[CaptureV1Reader] = []
        try:
            for path in capture_paths:
                reader = CaptureV1Reader(path)
                report = reader.validate()
                if not report.valid:
                    raise RuntimeError(
                        f"invalid Capture {path}: {'; '.join(report.errors)}"
                    )
                if _as_text(reader.file.attrs.get("time_assignment", "")) != "utc_window":
                    raise RuntimeError(f"Capture {path} is not assigned to UTC")
                start_ns = int(reader.file.attrs.get("window_start_utc_ns", -1))
                start_day = datetime.fromtimestamp(
                    start_ns / 1e9, tz=timezone.utc
                ).date()
                if start_ns < 0 or start_day != archive_day:
                    raise RuntimeError(f"Capture {path} is outside {archive_day}")
                readers.append(reader)
            readers.sort(key=self._source_order)
            identities = [self._identity(reader) for reader in readers]
            identity_base = identities[0][:-1]
            if any(candidate[:-1] != identity_base for candidate in identities[1:]):
                raise RuntimeError("Capture inputs do not describe one sensor")
            known_board_revisions = {
                candidate[-1] for candidate in identities if candidate[-1] != 0
            }
            if len(known_board_revisions) > 1:
                raise RuntimeError("Capture inputs disagree on board_revision")
            identity = identity_base + (
                next(iter(known_board_revisions), 0),
            )
            return self._write(readers, partial, output, archive_day, identity, compression)
        except Exception:
            # A failed conversion is intentionally left unpublished. Remove only
            # the private file created by this invocation.
            if partial.exists():
                partial.unlink()
            raise
        finally:
            for reader in readers:
                reader.close()

    def _write(self, readers, partial, output, archive_day, identity, compression):
        import h5py  # type: ignore
        import numpy as np  # type: ignore

        options = {
            "compression": None if compression == "none" else compression,
            "shuffle": compression != "none",
            "fletcher32": True,
        }
        file = h5py.File(partial, "w")
        try:
            channel_id, sensor_label, sensor_id, node_address, hardware_id, board_revision = identity
            attrs = file.attrs
            attrs["format_name"] = DataProduct.ARCHIVE.value
            attrs["schema_major"] = ARCHIVE_SCHEMA_MAJOR
            attrs["schema_minor"] = ARCHIVE_SCHEMA_MINOR
            attrs["file_id"] = str(uuid4())
            attrs["channel_id"] = channel_id
            attrs["sensor_label"] = sensor_label
            attrs["sensor_id"] = sensor_id
            attrs["node_address"] = node_address
            attrs["hardware_id"] = hardware_id
            attrs["board_revision"] = board_revision
            firmwares = sorted({_as_text(r.file.attrs.get("firmware_version", "")) for r in readers})
            attrs["firmware_version"] = ",".join(firmwares)
            attrs["archive_day_utc"] = archive_day.isoformat()
            attrs["time_assignment"] = "utc_day"
            attrs["created_utc_ns"] = time.time_ns()
            attrs["finalized_utc_ns"] = -1
            attrs["complete"] = False
            attrs["compactor_version"] = self.COMPACTOR_VERSION
            attrs["validation_profile"] = "strict-v1"
            attrs["max_timing_residual_ns"] = self.max_timing_residual_ns

            measurement = file.create_group("measurement")
            for name, value in {
                "axes": "X,Y,Z", "encoding": "signed_integer",
                "valid_bits": 24, "unit": "sensor_lsb",
                "processing_stage": "firmware_filtered_decimated",
            }.items():
                measurement.attrs[name] = value
            raw = measurement.create_dataset(
                "raw_xyz", shape=(0, 3), maxshape=(None, 3),
                chunks=(16384, 3), dtype="<i4", **options,
            )
            stream = file.create_group("stream")
            segments = stream.create_dataset(
                "segments", shape=(0,), maxshape=(None,), chunks=(512,),
                dtype=np.dtype([
                    ("sample_offset", "<u8"), ("sample_count", "<u8"),
                    ("boot_epoch", "<u8"), ("first_sample_seq", "<u8"),
                    ("timing_segment_id", "<u4"),
                    ("configuration_index", "<u4"),
                ]), **options,
            )
            gaps = stream.create_dataset(
                "gaps", shape=(0,), maxshape=(None,), chunks=(256,),
                dtype=np.dtype([
                    ("after_sample_offset", "<u8"), ("boot_epoch", "<u8"),
                    ("expected_sample_seq", "<u8"), ("received_sample_seq", "<u8"),
                    ("missing_sample_count", "<u8"), ("missing_count_known", "u1"),
                    ("gap_kind", "<u2"), ("estimated_duration_ns", "<u8"),
                ]), **options,
            )
            controls = file.create_group("timing").create_dataset(
                "control_points", shape=(0,), maxshape=(None,), chunks=(512,),
                dtype=np.dtype([
                    ("sample_offset", "<u8"), ("device_time_us", "<u8"),
                    ("utc_ns", "<i8"), ("uncertainty_ns", "<u8"),
                    ("method", "<u2"), ("flags", "<u2"),
                ]), **options,
            )
            qualities = file.create_group("quality").create_dataset(
                "intervals", shape=(0,), maxshape=(None,), chunks=(256,),
                dtype=np.dtype([
                    ("sample_offset", "<u8"), ("sample_count", "<u8"),
                    ("quality_flags", "<u4"),
                ]), **options,
            )
            temperatures = file.create_group("environment").create_dataset(
                "temperature", shape=(0,), maxshape=(None,), chunks=(256,),
                dtype=readers[0].file["environment/temperature"].dtype, **options,
            )
            configurations = file.create_group("configuration").create_dataset(
                "intervals", shape=(0,), maxshape=(None,), chunks=(32,),
                dtype=readers[0].file["configuration/intervals"].dtype, **options,
            )
            sources = file.create_group("provenance").create_dataset(
                "sources", shape=(0,), maxshape=(None,), chunks=(32,),
                dtype=np.dtype([
                    ("capture_file_id", "S36"), ("relative_name", "S256"),
                    ("sha256", "S64"), ("sample_count_read", "<u8"),
                ]), **options,
            )

            seen: set[tuple[int, int]] = set()
            config_map: dict[tuple[object, ...], int] = {}
            segment_rows = []
            quality_rows: list[tuple[int, int, int]] = []
            gap_rows = []
            source_rows = []
            quality_overlays: list[tuple[int, int, int]] = []
            candidate_runs: list[list[tuple[int, int, int, int, int, int]]] = []
            current_run_key = None
            current_run = []
            total = 0

            def add_quality(offset: int, count: int, flags: int) -> None:
                if quality_rows and quality_rows[-1][2] == flags and (
                    quality_rows[-1][0] + quality_rows[-1][1] == offset
                ):
                    start, old_count, old_flags = quality_rows[-1]
                    quality_rows[-1] = (start, old_count + count, old_flags)
                else:
                    quality_rows.append((offset, count, flags))

            for reader in readers:
                source_configs = []
                for config in reader.file["configuration/intervals"]:
                    key = self._configuration_key(config)
                    if key not in config_map:
                        config_map[key] = len(config_map)
                        _append(configurations, [config], np)
                    source_configs.append((config, config_map[key]))
                if not source_configs:
                    raise RuntimeError(f"Capture {reader.path} has no configuration")

                source_start = total
                input_raw = reader.raw_xyz_dataset
                raw.resize((total + input_raw.shape[0], 3))
                for start in range(0, input_raw.shape[0], 65536):
                    stop = min(input_raw.shape[0], start + 65536)
                    raw[total + start:total + stop] = input_raw[start:stop]

                block_offsets = []
                for block in reader.blocks_dataset:
                    count = int(block["sample_count"])
                    boot = int(block["boot_epoch"])
                    first_seq = int(block["first_sample_seq"])
                    keys = {(boot, first_seq + index) for index in range(count)}
                    duplicate = seen.intersection(keys)
                    if duplicate:
                        raise RuntimeError(f"duplicate sample identity {min(duplicate)}")
                    seen.update(keys)
                    offset = source_start + int(block["sample_offset"])
                    block_offsets.append((boot, first_seq, count, offset))
                    eligible_configs = [
                        item for item in source_configs
                        if int(item[0]["start_sample_seq"]) <= first_seq
                        and int(item[0]["start_boot_epoch"]) in (0, boot)
                    ]
                    if not eligible_configs:
                        raise RuntimeError(
                            f"no configuration covers sample {(boot, first_seq)}"
                        )
                    configuration_index = max(
                        eligible_configs,
                        key=lambda item: int(item[0]["start_sample_seq"]),
                    )[1]
                    segment_rows.append((
                        offset, count, boot, first_seq,
                        int(block["timing_segment_id"]), configuration_index,
                    ))
                    add_quality(offset, count, self._quality_from_block(block))

                    first_utc = int(block["first_utc_ns"])
                    last_utc = int(block["last_utc_ns"])
                    if first_utc >= 0:
                        run_key = (boot, int(block["timing_segment_id"]))
                        if run_key != current_run_key:
                            if current_run:
                                candidate_runs.append(current_run)
                            current_run = []
                            current_run_key = run_key
                        method = int(TimeControlMethod.DRDY_DEVICE_CLOCK)
                        flags = int(block["timing_flags"])
                        uncertainty = int(block["uncertainty_ns"])
                        current_run.append((
                            offset, int(block["first_device_time_us"]),
                            first_utc, uncertainty, method, flags,
                        ))
                        if count > 1 and last_utc >= 0:
                            current_run.append((
                                offset + count - 1,
                                int(block["last_device_time_us"]), last_utc,
                                uncertainty, method, flags,
                            ))

                for gap in reader.file["stream/gaps"]:
                    expected = int(gap["expected_sample_seq"])
                    received = int(gap["received_sample_seq"])
                    match = next((item for item in block_offsets if (
                        item[1] <= received < item[1] + item[2]
                    )), None)
                    received_offset = (
                        match[3] + received - match[1] if match else source_start
                    )
                    missing = max(0, received - expected)
                    gap_rows.append((
                        max(0, received_offset - 1), match[0] if match else 0,
                        expected, received, missing, 1,
                        int(GapKind.SEQUENCE), 0,
                    ))

                for event in reader.file["quality/events"]:
                    event_code = int(event["event_code"])
                    if event_code in (
                        int(QualityEventCode.SENSOR_FIFO_LOSS),
                        int(QualityEventCode.SENSOR_RECOVERY),
                        int(QualityEventCode.CONFIG_CHANGED),
                        int(QualityEventCode.CALIBRATION_CHANGED),
                    ):
                        anchor = int(event["sample_seq_anchor"])
                        match = next((item for item in reversed(block_offsets) if (
                            item[1] <= anchor < item[1] + item[2]
                        )), None)
                        anchor_offset = (
                            match[3] + anchor - match[1] if match else source_start
                        )
                        if event_code == int(QualityEventCode.SENSOR_FIFO_LOSS):
                            gap_rows.append((
                                anchor_offset, int(event["boot_epoch"]), anchor, anchor,
                                0, 0, int(GapKind.SENSOR_FIFO), 0,
                            ))
                            quality_overlays.append((
                                max(0, anchor_offset - 1), 3,
                                int(ArchiveQualityFlag.SENSOR_LOSS_ADJACENT),
                            ))
                        elif event_code == int(QualityEventCode.SENSOR_RECOVERY):
                            quality_overlays.append((
                                anchor_offset, 1,
                                int(ArchiveQualityFlag.SENSOR_RECOVERY),
                            ))
                        else:
                            quality_overlays.append((
                                anchor_offset, 1,
                                int(ArchiveQualityFlag.CONFIG_CHANGED),
                            ))

                _append(temperatures, reader.file["environment/temperature"][:], np)
                source_count = int(input_raw.shape[0])
                source_rows.append((
                    _as_text(reader.file.attrs["file_id"]).encode("ascii"),
                    reader.path.name.encode("utf-8"),
                    _sha256(reader.path).encode("ascii"), source_count,
                ))
                total += source_count

            if current_run:
                candidate_runs.append(current_run)
            control_rows = []
            for run in candidate_runs:
                # Consecutive blocks share endpoints; retain a single copy.
                unique = []
                for point in run:
                    if unique and unique[-1][0] == point[0]:
                        unique[-1] = point
                    else:
                        unique.append(point)
                control_rows.extend(self._simplify_control_points(
                    unique, self.max_timing_residual_ns
                ))

            for start, count, flag in quality_overlays:
                quality_rows = _overlay_quality_flag(
                    quality_rows, start, min(count, max(0, total - start)), flag
                )

            _append(segments, segment_rows, np)
            _append(gaps, sorted(set(gap_rows)), np)
            _append(controls, control_rows, np)
            _append(qualities, quality_rows, np)
            _append(sources, source_rows, np)
            attrs["sample_count"] = total
            attrs["source_count"] = len(readers)
            attrs["control_point_count"] = len(control_rows)
            attrs["finalized_utc_ns"] = time.time_ns()
            attrs["complete"] = True
            file.flush()
            handle = file.id.get_vfd_handle()
            if isinstance(handle, int):
                os.fsync(handle)
        finally:
            file.close()

        os.replace(partial, output)
        directory_fd = os.open(output.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
        digest = _sha256(output)
        manifest = Path(str(output) + ".sha256")
        manifest_partial = Path(str(manifest) + ".partial")
        with manifest_partial.open("w", encoding="ascii") as stream:
            stream.write(f"{digest}  {output.name}\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(manifest_partial, manifest)
        directory_fd = os.open(
            output.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
        )
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
        return ArchiveBuildReport(
            output=output, manifest=manifest, sha256=digest,
            sample_count=total, source_count=len(readers),
            control_point_count=len(control_rows),
        )
