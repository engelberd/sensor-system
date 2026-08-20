"""Public reader and structural validator for Capture HDF5 v1."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from host.recorder.capture_v1 import SIGNED24_MAX, SIGNED24_MIN
from host.recorder.contracts import CAPTURE_SCHEMA_MAJOR, DataProduct
from host.recorder.model import scale_m_s2_per_lsb


@dataclass(frozen=True)
class CaptureValidationReport:
    valid: bool
    errors: tuple[str, ...]
    sample_count: int
    block_count: int


class CaptureV1Reader:
    def __init__(self, path: str | Path, *, allow_partial: bool = False) -> None:
        try:
            import h5py  # type: ignore
            import numpy as np  # type: ignore
        except ImportError as exc:
            raise RuntimeError("Capture HDF5 requires h5py and numpy") from exc
        self.np = np
        self.path = Path(path)
        self.file = h5py.File(self.path, "r")
        try:
            self._verify_header(allow_partial=allow_partial)
            self.raw_xyz_dataset = self.file["measurement/raw_xyz"]
            self.blocks_dataset = self.file["capture/blocks"]
        except Exception:
            self.file.close()
            raise

    def _verify_header(self, *, allow_partial: bool) -> None:
        attrs = self.file.attrs
        if attrs.get("format_name") != DataProduct.CAPTURE.value:
            raise RuntimeError("not a Capture HDF5 v1 file")
        if int(attrs.get("schema_major", -1)) != CAPTURE_SCHEMA_MAJOR:
            raise RuntimeError("unsupported Capture schema major version")
        if not allow_partial and not bool(attrs.get("complete", False)):
            raise RuntimeError("Capture file is not complete")

    @property
    def range_g(self) -> int:
        configurations = self.file["configuration/intervals"]
        if configurations.shape[0] == 0:
            raise RuntimeError("Capture file has no configuration")
        values = {int(record["range_g"]) for record in configurations}
        if len(values) != 1:
            raise RuntimeError(
                "one-shot SI conversion requires a constant range"
            )
        return values.pop()

    def raw_xyz(self, start: int = 0, stop: int | None = None):
        resolved_stop = self.raw_xyz_dataset.shape[0] if stop is None else stop
        return self.raw_xyz_dataset[start:resolved_stop]

    def acceleration_m_s2(self, start: int = 0, stop: int | None = None):
        raw = self.raw_xyz(start, stop).astype(self.np.float64)
        return raw * scale_m_s2_per_lsb(self.range_g)

    def sample_identity(self):
        """Return `(boot_epoch, sample_seq)` arrays reconstructed from blocks."""
        count = int(self.raw_xyz_dataset.shape[0])
        boots = self.np.empty(count, dtype="<u8")
        sequences = self.np.empty(count, dtype="<u8")
        for block in self.blocks_dataset:
            offset = int(block["sample_offset"])
            block_count = int(block["sample_count"])
            boots[offset:offset + block_count] = int(block["boot_epoch"])
            first = int(block["first_sample_seq"])
            sequences[offset:offset + block_count] = self.np.arange(
                first,
                first + block_count,
                dtype="<u8",
            )
        return boots, sequences

    def validate(self) -> CaptureValidationReport:
        errors: list[str] = []
        sample_count = int(self.raw_xyz_dataset.shape[0])
        block_count = int(self.blocks_dataset.shape[0])
        if self.raw_xyz_dataset.ndim != 2 or self.raw_xyz_dataset.shape[1] != 3:
            errors.append("raw_xyz must have shape (N, 3)")
        if self.raw_xyz_dataset.dtype != self.np.dtype("<i4"):
            errors.append("raw_xyz must use little-endian int32")

        expected_offset = 0
        last_sequence_by_boot: dict[int, int] = {}
        for index, block in enumerate(self.blocks_dataset):
            offset = int(block["sample_offset"])
            count = int(block["sample_count"])
            if count <= 0:
                errors.append(f"block {index} has zero samples")
            if offset != expected_offset:
                errors.append(
                    f"block {index} starts at {offset}, expected {expected_offset}"
                )
            expected_offset = offset + count
            boot = int(block["boot_epoch"])
            first = int(block["first_sample_seq"])
            previous_last = last_sequence_by_boot.get(boot)
            if previous_last is not None and first <= previous_last:
                errors.append(f"block {index} overlaps or is not stream-ordered")
            last_sequence_by_boot[boot] = max(
                previous_last if previous_last is not None else -1,
                first + max(0, count) - 1,
            )
            first_device = int(block["first_device_time_us"])
            last_device = int(block["last_device_time_us"])
            if count > 1 and first_device > 0 and last_device <= first_device:
                errors.append(f"block {index} has non-increasing device time")
            first_utc = int(block["first_utc_ns"])
            last_utc = int(block["last_utc_ns"])
            if (first_utc < 0) != (last_utc < 0):
                errors.append(f"block {index} has incomplete UTC endpoints")
            if count > 1 and first_utc >= 0 and last_utc <= first_utc:
                errors.append(f"block {index} has non-increasing UTC")
        if expected_offset != sample_count:
            errors.append(
                f"blocks cover {expected_offset} samples, raw_xyz has {sample_count}"
            )

        chunk_rows = 65536
        for start in range(0, sample_count, chunk_rows):
            raw = self.raw_xyz_dataset[
                start:min(sample_count, start + chunk_rows)
            ]
            if raw.size and (
                int(raw.min()) < SIGNED24_MIN
                or int(raw.max()) > SIGNED24_MAX
            ):
                errors.append("raw_xyz contains a value outside signed-24")
                break
        if self.file["configuration/intervals"].shape[0] == 0:
            errors.append("configuration intervals are empty")
        return CaptureValidationReport(
            valid=not errors,
            errors=tuple(errors),
            sample_count=sample_count,
            block_count=block_count,
        )

    def close(self) -> None:
        self.file.close()

    def __enter__(self) -> "CaptureV1Reader":
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()
