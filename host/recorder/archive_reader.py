"""Reader and strict structural validation for Archive HDF5 v1."""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
from pathlib import Path

from host.recorder.capture_v1 import SIGNED24_MAX, SIGNED24_MIN
from host.recorder.contracts import ARCHIVE_SCHEMA_MAJOR, DataProduct
from host.recorder.model import scale_m_s2_per_lsb


@dataclass(frozen=True)
class ArchiveValidationReport:
    valid: bool
    errors: tuple[str, ...]
    sample_count: int
    segment_count: int
    source_count: int


class ArchiveV1Reader:
    def __init__(self, path: str | Path, *, verify_manifest: bool = True) -> None:
        try:
            import h5py  # type: ignore
            import numpy as np  # type: ignore
        except ImportError as exc:
            raise RuntimeError("Archive HDF5 requires h5py and numpy") from exc
        self.path = Path(path)
        self.np = np
        self.file = h5py.File(self.path, "r")
        try:
            attrs = self.file.attrs
            if attrs.get("format_name") != DataProduct.ARCHIVE.value:
                raise RuntimeError("not an Archive HDF5 v1 file")
            if int(attrs.get("schema_major", -1)) != ARCHIVE_SCHEMA_MAJOR:
                raise RuntimeError("unsupported Archive schema major version")
            if not bool(attrs.get("complete", False)):
                raise RuntimeError("Archive file is not complete")
            if verify_manifest:
                self._verify_manifest()
            self.raw_xyz_dataset = self.file["measurement/raw_xyz"]
        except Exception:
            self.file.close()
            raise

    def _verify_manifest(self) -> None:
        manifest = Path(str(self.path) + ".sha256")
        if not manifest.is_file():
            raise RuntimeError("Archive SHA-256 manifest is missing")
        parts = manifest.read_text(encoding="ascii").strip().split()
        if len(parts) != 2 or parts[1] != self.path.name:
            raise RuntimeError("Archive SHA-256 manifest is malformed")
        digest = hashlib.sha256()
        with self.path.open("rb") as stream:
            while chunk := stream.read(1024 * 1024):
                digest.update(chunk)
        if digest.hexdigest() != parts[0]:
            raise RuntimeError("Archive SHA-256 mismatch")

    def raw_xyz(self, start: int = 0, stop: int | None = None):
        resolved_stop = self.raw_xyz_dataset.shape[0] if stop is None else stop
        return self.raw_xyz_dataset[start:resolved_stop]

    def acceleration_m_s2(self, start: int = 0, stop: int | None = None):
        """Return an SI view while preserving raw samples as the source."""
        resolved_stop = self.raw_xyz_dataset.shape[0] if stop is None else stop
        if start < 0 or resolved_stop < start:
            raise ValueError("invalid sample slice")
        result = self.raw_xyz(start, resolved_stop).astype(self.np.float64)
        configurations = self.file["configuration/intervals"]
        covered = self.np.zeros(result.shape[0], dtype=bool)
        for segment in self.file["stream/segments"]:
            segment_start = int(segment["sample_offset"])
            segment_stop = segment_start + int(segment["sample_count"])
            overlap_start = max(start, segment_start)
            overlap_stop = min(resolved_stop, segment_stop)
            if overlap_start >= overlap_stop:
                continue
            config_index = int(segment["configuration_index"])
            range_g = int(configurations[config_index]["range_g"])
            local_start = overlap_start - start
            local_stop = overlap_stop - start
            result[local_start:local_stop] *= scale_m_s2_per_lsb(range_g)
            covered[local_start:local_stop] = True
        if covered.size and not bool(covered.all()):
            raise RuntimeError("configuration does not cover requested samples")
        return result

    def validate(self) -> ArchiveValidationReport:
        errors: list[str] = []
        raw = self.raw_xyz_dataset
        sample_count = int(raw.shape[0])
        if raw.ndim != 2 or raw.shape[1] != 3:
            errors.append("raw_xyz must have shape (N, 3)")
        if raw.dtype != self.np.dtype("<i4"):
            errors.append("raw_xyz must use little-endian int32")
        for start in range(0, sample_count, 65536):
            values = raw[start:min(sample_count, start + 65536)]
            if values.size and (
                int(values.min()) < SIGNED24_MIN
                or int(values.max()) > SIGNED24_MAX
            ):
                errors.append("raw_xyz contains a value outside signed-24")
                break

        segments = self.file["stream/segments"]
        configurations = self.file["configuration/intervals"]
        expected = 0
        last_sequence_by_boot: dict[int, int] = {}
        for index, segment in enumerate(segments):
            offset = int(segment["sample_offset"])
            count = int(segment["sample_count"])
            if offset != expected or count == 0:
                errors.append(
                    f"segment {index} does not continue at sample {expected}"
                )
            expected = offset + count
            if int(segment["configuration_index"]) >= len(configurations):
                errors.append(f"segment {index} has invalid configuration index")
            boot = int(segment["boot_epoch"])
            first = int(segment["first_sample_seq"])
            previous_last = last_sequence_by_boot.get(boot)
            if previous_last is not None and first <= previous_last:
                errors.append(
                    f"segment {index} overlaps sample identity "
                    f"{(boot, first)}"
                )
            last_sequence_by_boot[boot] = max(
                previous_last if previous_last is not None else -1,
                first + max(0, count) - 1,
            )
        if expected != sample_count:
            errors.append(
                f"segments cover {expected} samples, raw_xyz has {sample_count}"
            )

        qualities = self.file["quality/intervals"]
        expected = 0
        for index, interval in enumerate(qualities):
            offset = int(interval["sample_offset"])
            count = int(interval["sample_count"])
            if offset != expected or count == 0:
                errors.append(
                    f"quality interval {index} does not continue at sample {expected}"
                )
            expected = offset + count
        if expected != sample_count:
            errors.append(
                f"quality intervals cover {expected} samples, expected {sample_count}"
            )

        previous = -1
        previous_utc = -1
        for index, control in enumerate(self.file["timing/control_points"]):
            offset = int(control["sample_offset"])
            utc_ns = int(control["utc_ns"])
            if offset < 0 or offset >= sample_count:
                errors.append(f"control point {index} is outside measurement")
            if offset <= previous:
                errors.append("timing control points are not strictly ordered")
                break
            if utc_ns <= previous_utc:
                errors.append("timing control UTC values are not strictly ordered")
                break
            previous = offset
            previous_utc = utc_ns

        for index, gap in enumerate(self.file["stream/gaps"]):
            after = int(gap["after_sample_offset"])
            if sample_count == 0 or after >= sample_count:
                errors.append(f"gap {index} is outside measurement")

        sources = self.file["provenance/sources"]
        source_samples = sum(int(row["sample_count_read"]) for row in sources)
        if source_samples != sample_count:
            errors.append(
                f"provenance accounts for {source_samples} of {sample_count} samples"
            )
        if int(self.file.attrs.get("sample_count", -1)) != sample_count:
            errors.append("sample_count summary does not match measurement")
        return ArchiveValidationReport(
            valid=not errors,
            errors=tuple(errors),
            sample_count=sample_count,
            segment_count=int(segments.shape[0]),
            source_count=int(sources.shape[0]),
        )

    def close(self) -> None:
        self.file.close()

    def __enter__(self) -> "ArchiveV1Reader":
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()
