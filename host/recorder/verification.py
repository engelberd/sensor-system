"""Full-file validation and timing/ODR diagnostics for HDF5 products."""

from __future__ import annotations

from dataclasses import asdict, dataclass
from pathlib import Path
from statistics import median
from typing import Any

from host.recorder.archive_reader import ArchiveV1Reader
from host.recorder.capture_reader import CaptureV1Reader
from host.recorder.contracts import ArchiveQualityFlag, DataProduct


def _text(value: object) -> str:
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace").rstrip("\0")
    return str(value)


def _rate_summary(rates: list[float], nominal_rates: list[float]) -> dict[str, Any]:
    result: dict[str, Any] = {
        "interval_count": len(rates),
        "observed_odr_hz": None,
        "observed_odr_min_hz": None,
        "observed_odr_max_hz": None,
        "nominal_odr_hz": nominal_rates[0] if len(nominal_rates) == 1 else None,
        "odr_shift_ppm": None,
    }
    if not rates:
        return result
    observed = float(median(rates))
    result.update({
        "observed_odr_hz": observed,
        "observed_odr_min_hz": min(rates),
        "observed_odr_max_hz": max(rates),
    })
    nominal = result["nominal_odr_hz"]
    if nominal:
        result["odr_shift_ppm"] = (observed / float(nominal) - 1.0) * 1e6
    return result


@dataclass(frozen=True)
class HdfVerificationReport:
    path: str
    product: str
    schema_major: int
    complete: bool
    valid: bool
    sample_count: int
    errors: tuple[str, ...]
    warnings: tuple[str, ...]
    diagnostics: dict[str, Any]

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


def _capture_rates(reader: CaptureV1Reader) -> list[float]:
    rates: list[float] = []
    for block in reader.blocks_dataset:
        count = int(block["sample_count"])
        first_us = int(block["first_device_time_us"])
        last_us = int(block["last_device_time_us"])
        if count < 2 or first_us <= 0 or last_us <= first_us:
            continue
        rates.append((count - 1) * 1_000_000.0 / (last_us - first_us))
    return rates


def _archive_rates(reader: ArchiveV1Reader) -> list[float]:
    controls = reader.file["timing/control_points"]
    segments = reader.file["stream/segments"][:]
    rates: list[float] = []

    def segment_at(offset: int):
        for index, segment in enumerate(segments):
            start = int(segment["sample_offset"])
            if start <= offset < start + int(segment["sample_count"]):
                return index
        return None

    for left, right in zip(controls[:-1], controls[1:]):
        start = int(left["sample_offset"])
        stop = int(right["sample_offset"])
        elapsed_ns = int(right["utc_ns"]) - int(left["utc_ns"])
        if stop <= start or elapsed_ns <= 0:
            continue
        first_index = segment_at(start)
        last_index = segment_at(stop)
        if first_index is None or last_index is None:
            continue
        run = segments[first_index:last_index + 1]
        if len(run) == 0:
            continue
        boot = int(run[0]["boot_epoch"])
        timing_segment = int(run[0]["timing_segment_id"])
        contiguous = True
        for previous, current in zip(run[:-1], run[1:]):
            contiguous = contiguous and (
                int(current["sample_offset"])
                == int(previous["sample_offset"]) + int(previous["sample_count"])
                and int(current["boot_epoch"]) == boot
                and int(current["timing_segment_id"]) == timing_segment
                and int(current["first_sample_seq"])
                == int(previous["first_sample_seq"]) + int(previous["sample_count"])
            )
        if not contiguous:
            continue
        if any(
            int(segment["boot_epoch"]) != boot
            or int(segment["timing_segment_id"]) != timing_segment
            for segment in run
        ):
            continue
        rates.append((stop - start) * 1_000_000_000.0 / elapsed_ns)
    return rates


def verify_hdf5(
    path: str | Path,
    *,
    allow_partial: bool = False,
    verify_manifest: bool = True,
    odr_warning_ppm: float = 1000.0,
    resample_threshold_ppm: float = 50.0,
) -> HdfVerificationReport:
    """Read every measurement chunk and return structural plus timing diagnostics."""
    import h5py  # type: ignore

    resolved = Path(path)
    with h5py.File(resolved, "r") as handle:
        product = _text(handle.attrs.get("format_name", ""))
        schema_major = int(handle.attrs.get("schema_major", -1))
        complete = bool(handle.attrs.get("complete", False))

    warnings: list[str] = []
    if product == DataProduct.CAPTURE.value:
        with CaptureV1Reader(resolved, allow_partial=allow_partial) as reader:
            validation = reader.validate()
            configurations = reader.file["configuration/intervals"]
            nominal = sorted({float(row["output_odr_hz"]) for row in configurations})
            timing = _rate_summary(_capture_rates(reader), nominal)
            timed_samples = sum(
                int(block["sample_count"])
                for block in reader.blocks_dataset
                if int(block["first_utc_ns"]) >= 0
            )
            gaps = reader.file["stream/gaps"]
            quality_events = reader.file["quality/events"]
            diagnostics = {
                "block_count": validation.block_count,
                "gap_count": int(gaps.shape[0]),
                "quality_event_count": int(quality_events.shape[0]),
                "timed_sample_count": timed_samples,
                "timed_sample_fraction": (
                    timed_samples / validation.sample_count
                    if validation.sample_count else 0.0
                ),
                "timing": timing,
            }
    elif product == DataProduct.ARCHIVE.value:
        with ArchiveV1Reader(resolved, verify_manifest=verify_manifest) as reader:
            validation = reader.validate()
            configurations = reader.file["configuration/intervals"]
            nominal = sorted({float(row["output_odr_hz"]) for row in configurations})
            timing = _rate_summary(_archive_rates(reader), nominal)
            gaps = reader.file["stream/gaps"]
            qualities = reader.file["quality/intervals"]
            degraded = sum(
                int(row["sample_count"])
                for row in qualities
                if int(row["quality_flags"]) != int(ArchiveQualityFlag.GOOD)
            )
            diagnostics = {
                "segment_count": validation.segment_count,
                "source_count": validation.source_count,
                "gap_count": int(gaps.shape[0]),
                "timing_control_point_count": int(
                    reader.file["timing/control_points"].shape[0]
                ),
                "degraded_sample_count": degraded,
                "degraded_sample_fraction": (
                    degraded / validation.sample_count
                    if validation.sample_count else 0.0
                ),
                "timing": timing,
            }
    else:
        raise RuntimeError(f"unsupported HDF5 data product: {product or '<missing>'}")

    shift = diagnostics["timing"]["odr_shift_ppm"]
    if shift is None:
        warnings.append("effective ODR could not be estimated unambiguously")
    elif abs(float(shift)) > odr_warning_ppm:
        warnings.append(
            f"effective ODR differs from nominal by {float(shift):.1f} ppm"
        )
    diagnostics["resampling"] = {
        "source_is_immutable": True,
        "layer": "derived-only",
        "target_odr_hz": diagnostics["timing"]["nominal_odr_hz"],
        "recommended": shift is not None and abs(float(shift)) > resample_threshold_ppm,
        "reason": (
            "use timing control points to map source samples onto a uniform grid"
            if shift is not None
            else "insufficient timing evidence; do not resample"
        ),
    }
    return HdfVerificationReport(
        path=str(resolved),
        product=product,
        schema_major=schema_major,
        complete=complete,
        valid=validation.valid,
        sample_count=validation.sample_count,
        errors=validation.errors,
        warnings=tuple(warnings),
        diagnostics=diagnostics,
    )
