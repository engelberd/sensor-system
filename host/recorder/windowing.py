"""Time-window routing decorators for Capture writers."""

from __future__ import annotations

import argparse
from datetime import datetime, timedelta, timezone
import os
from pathlib import Path
from typing import Optional

from host.common.clock_models import TimeSyncObservation
from host.recorder.capture_writers import CsvWriter, Hdf5Writer
from host.recorder.model import (
    QualityEventRecord,
    RecorderNode,
    SampleRecord,
    TemperatureRecord,
)
from host.recorder.ports import BaseWriter


def make_single_writer(
    args: argparse.Namespace,
    metadata: dict[str, object],
    output: Path,
    append: bool = False,
) -> BaseWriter:
    if output.exists() and not append and not args.overwrite:
        raise RuntimeError(
            f"output file already exists: {output}; "
            "pass --overwrite to replace it"
        )
    if args.format == "hdf5":
        return Hdf5Writer(output, metadata, args.compression, append=append)
    if args.format == "csv":
        return CsvWriter(output, metadata, append=append)
    raise ValueError(f"unsupported format: {args.format}")


class WindowedWriter(BaseWriter):
    def __init__(
        self,
        args: argparse.Namespace,
        metadata: dict[str, object],
        nodes: list[RecorderNode],
    ) -> None:
        self.args = args
        self.metadata = metadata
        self.nodes = nodes
        self.window_timezone = args.window_timezone
        self.current_window_start: Optional[datetime] = None
        self.current_window_end: Optional[datetime] = None
        self.current_path: Optional[Path] = None
        self.writer: Optional[BaseWriter] = None

    def aligned_window_start(self, when_utc: datetime) -> datetime:
        when_local = when_utc.astimezone(self.window_timezone)
        offset = when_local.utcoffset() or timedelta(0)
        aligned_ts = ((when_utc.timestamp() + offset.total_seconds()) // self.args.window_seconds) * self.args.window_seconds
        return datetime.fromtimestamp(aligned_ts - offset.total_seconds(), tz=timezone.utc)

    def current_window(self, now_utc: Optional[datetime] = None) -> datetime:
        resolved_now = now_utc or datetime.now(timezone.utc)
        if resolved_now.tzinfo is None:
            resolved_now = resolved_now.replace(tzinfo=timezone.utc)
        if (
            self.current_window_start is not None
            and self.current_window_end is not None
            and resolved_now < self.current_window_end
        ):
            return self.current_window_start
        return self.aligned_window_start(resolved_now)

    def window_end_for(self, created_at: datetime) -> datetime:
        return self.aligned_window_start(created_at) + timedelta(seconds=self.args.window_seconds)

    def window_path_for(self, created_at: datetime) -> Path:
        suffix = ".h5" if self.args.format == "hdf5" else ".csv"
        created_at_local = created_at.astimezone(self.window_timezone)
        file_label = created_at_local.strftime("%Y-%m-%d_%H-%M-%S")
        day_dir = Path(self.args.output_dir) / created_at_local.strftime("%Y-%m-%d")
        return day_dir / f"{self.args.channel_name}_{file_label}{suffix}"

    def resolve_new_path(self, created_at: datetime) -> Path:
        candidate = self.window_path_for(created_at)
        if not candidate.exists():
            return candidate
        suffix = candidate.suffix
        stem = candidate.stem
        for index in range(2, 1000):
            next_candidate = candidate.with_name(f"{stem}_{index:02d}{suffix}")
            if not next_candidate.exists():
                return next_candidate
        raise RuntimeError(f"cannot allocate unique output path for '{candidate}'")

    def ensure_writer(self, now_utc: Optional[datetime] = None) -> BaseWriter:
        resolved_now = now_utc or datetime.now(timezone.utc)
        if resolved_now.tzinfo is None:
            resolved_now = resolved_now.replace(tzinfo=timezone.utc)
        if (
            self.writer is not None
            and self.current_window_end is not None
            and resolved_now < self.current_window_end
        ):
            return self.writer

        if self.writer is not None:
            if isinstance(self.writer, Hdf5Writer):
                self.writer.finalize()
            self.writer.close()

        window_start = self.aligned_window_start(resolved_now)
        window_end = self.window_end_for(window_start)
        path = self.resolve_new_path(window_start)
        window_metadata = dict(self.metadata)
        window_start_local = window_start.astimezone(self.window_timezone)
        window_end_local = window_end.astimezone(self.window_timezone)
        window_metadata["window_start_utc"] = window_start.isoformat()
        window_metadata["window_end_utc"] = window_end.isoformat()
        window_metadata["window_timezone"] = self.args.window_timezone_name
        window_metadata["window_start_local"] = window_start_local.isoformat()
        window_metadata["window_end_local"] = window_end_local.isoformat()
        window_metadata["file_created_utc"] = datetime.now(timezone.utc).isoformat()
        self.writer = make_single_writer(self.args, window_metadata, path, append=False)
        self.current_window_start = window_start
        self.current_window_end = window_end
        self.current_path = path
        for node in self.nodes:
            self.writer.add_node(node)
        print(f"[FILE] {path} (new)")
        return self.writer

    def add_node(self, node: RecorderNode) -> None:
        del node
        self.ensure_writer()

    def write_samples(self, node_id: int, samples: list[SampleRecord]) -> None:
        writer = self.ensure_writer()
        writer.write_samples(node_id, samples)

    def write_temperature(self, node_id: int, records: list[TemperatureRecord]) -> None:
        writer = self.ensure_writer()
        writer.write_temperature(node_id, records)

    def write_gap(
        self,
        node_id: int,
        expected_sample_seq: int,
        received_sample_seq: int,
        packet_seq: int,
        boot_epoch: int = 0,
    ) -> None:
        writer = self.ensure_writer()
        writer.write_gap(
            node_id,
            expected_sample_seq,
            received_sample_seq,
            packet_seq,
            boot_epoch,
        )

    def flush(self) -> None:
        if self.writer is not None:
            self.writer.flush()

    def close(self) -> None:
        if self.writer is not None:
            if isinstance(self.writer, Hdf5Writer):
                self.writer.finalize()
            self.writer.close()
            self.writer = None
            self.current_window_start = None
            self.current_window_end = None

    def write_clock_sync(
        self,
        node_id: int,
        observation: TimeSyncObservation,
        accepted: bool,
    ) -> None:
        writer = self.ensure_writer()
        writer.write_clock_sync(node_id, observation, accepted)

    def write_quality_event(
        self,
        node_id: int,
        record: QualityEventRecord,
    ) -> None:
        self.ensure_writer().write_quality_event(node_id, record)


class AcquisitionWindowedWriter(WindowedWriter):
    def __init__(
        self,
        args: argparse.Namespace,
        metadata: dict[str, object],
        nodes: list[RecorderNode],
    ) -> None:
        super().__init__(args, metadata, nodes)
        self.active_windows: dict[
            datetime,
            tuple[Path, Path, Hdf5Writer],
        ] = {}
        self.finalized_windows: set[datetime] = set()
        self.max_open_windows = 3
        self.pending_clock_sync: dict[
            int,
            list[tuple[TimeSyncObservation, bool]],
        ] = {}
        self.pending_temperatures: dict[int, list[TemperatureRecord]] = {}
        self.pending_gaps: list[tuple[int, int, int, int, int]] = []
        self.quarantine_writers: dict[
            str,
            tuple[Path, Path, Hdf5Writer],
        ] = {}
        self.quarantine_session_id = (
            datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
            + f"-{os.getpid()}"
        )

    def add_node(self, node: RecorderNode) -> None:
        del node

    def _window_metadata(
        self,
        window_start: datetime,
        window_end: datetime,
    ) -> dict[str, object]:
        result = dict(self.metadata)
        result["window_start_utc"] = window_start.isoformat()
        result["window_end_utc"] = window_end.isoformat()
        result["nominal_window_start_utc_ns"] = round(
            window_start.timestamp() * 1_000_000_000
        )
        result["nominal_window_end_utc_ns"] = round(
            window_end.timestamp() * 1_000_000_000
        )
        result["window_timezone"] = self.args.window_timezone_name
        result["window_start_local"] = (
            window_start.astimezone(self.window_timezone).isoformat()
        )
        result["window_end_local"] = (
            window_end.astimezone(self.window_timezone).isoformat()
        )
        result["file_created_utc"] = (
            datetime.now(timezone.utc).isoformat()
        )
        return result

    def _ensure_window(self, window_start: datetime) -> Hdf5Writer:
        existing = self.active_windows.get(window_start)
        if existing is not None:
            return existing[2]
        if window_start in self.finalized_windows:
            raise RuntimeError(
                f"late data targets finalized window {window_start.isoformat()}"
            )

        window_end = self.window_end_for(window_start)
        final_path = self.window_path_for(window_start)
        partial_path = Path(str(final_path) + ".partial")
        if final_path.exists():
            raise RuntimeError(
                f"acquisition window is already finalized: {final_path}"
            )
        writer = Hdf5Writer(
            partial_path,
            self._window_metadata(window_start, window_end),
            self.args.compression,
            append=partial_path.exists(),
        )
        for node in self.nodes:
            writer.add_node(node)
            for observation, accepted in self.pending_clock_sync.get(
                node.node_id,
                [],
            ):
                writer.write_clock_sync(
                    node.node_id,
                    observation,
                    accepted,
                )
            pending_temperatures = self.pending_temperatures.pop(
                node.node_id,
                [],
            )
            if pending_temperatures:
                writer.write_temperature(
                    node.node_id,
                    pending_temperatures,
                )
        for (
            gap_node_id,
            expected_sample_seq,
            received_sample_seq,
            packet_seq,
            boot_epoch,
        ) in self.pending_gaps:
            writer.write_gap(
                gap_node_id,
                expected_sample_seq,
                received_sample_seq,
                packet_seq,
                boot_epoch,
            )
        self.pending_gaps.clear()
        self.active_windows[window_start] = (
            final_path,
            partial_path,
            writer,
        )
        self.current_window_start = window_start
        self.current_window_end = window_end
        self.current_path = final_path
        self.writer = writer
        print(f"[FILE] {partial_path} (active)")

        while len(self.active_windows) > self.max_open_windows:
            oldest = min(self.active_windows)
            if oldest == window_start:
                raise RuntimeError("open-window limit would finalize incoming late data")
            self._finalize_window(oldest)
        return writer

    def _finalize_window(self, window_start: datetime) -> None:
        final_path, partial_path, writer = self.active_windows.pop(
            window_start
        )
        self._publish_writer(final_path, partial_path, writer)
        self.finalized_windows.add(window_start)
        print(f"[FILE] {final_path} (complete)")

    def _publish_writer(
        self,
        final_path: Path,
        partial_path: Path,
        writer: Hdf5Writer,
    ) -> None:
        writer.finalize()
        writer.close()
        os.replace(partial_path, final_path)
        try:
            directory_fd = os.open(
                final_path.parent,
                os.O_RDONLY | getattr(os, "O_DIRECTORY", 0),
            )
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
        except OSError:
            pass

    def _ensure_quarantine(self, reason: str) -> Hdf5Writer:
        existing = self.quarantine_writers.get(reason)
        if existing is not None:
            return existing[2]
        created = datetime.now(timezone.utc)
        directory = (
            Path(self.args.output_dir)
            / "quarantine"
            / created.strftime("%Y-%m-%d")
        )
        filename = (
            f"{self.args.channel_name}_timing-{reason}_"
            f"{self.quarantine_session_id}.h5"
        )
        final_path = directory / filename
        partial_path = Path(str(final_path) + ".partial")
        metadata = dict(self.metadata)
        metadata.update({
            "timing_quarantine": True,
            "timing_quarantine_reason": reason,
            "normal_measurement_window": False,
            "file_created_utc": created.isoformat(),
        })
        writer = Hdf5Writer(
            partial_path,
            metadata,
            self.args.compression,
            append=partial_path.exists(),
        )
        for node in self.nodes:
            writer.add_node(node)
            for observation, accepted in self.pending_clock_sync.get(
                node.node_id,
                [],
            ):
                writer.write_clock_sync(
                    node.node_id,
                    observation,
                    accepted,
                )
        self.quarantine_writers[reason] = (
            final_path,
            partial_path,
            writer,
        )
        print(f"[QUARANTINE] {partial_path} reason={reason}")
        return writer

    def _classify_sample(
        self,
        sample: SampleRecord,
    ) -> tuple[Optional[datetime], Optional[str]]:
        if (
            sample.acquisition_utc_ns is None
            or sample.timing_uncertainty_ns is None
        ):
            return None, "unsynced"
        when = datetime.fromtimestamp(
            sample.acquisition_utc_ns / 1_000_000_000,
            tz=timezone.utc,
        )
        window_start = self.aligned_window_start(when)
        window_end = self.window_end_for(window_start)
        start_ns = round(window_start.timestamp() * 1_000_000_000)
        end_ns = round(window_end.timestamp() * 1_000_000_000)
        lower = sample.acquisition_utc_ns - sample.timing_uncertainty_ns
        upper = sample.acquisition_utc_ns + sample.timing_uncertainty_ns
        if lower < start_ns or upper >= end_ns:
            return None, "ambiguous"
        if (
            window_start in self.finalized_windows
            or self.window_path_for(window_start).exists()
        ):
            return None, "late"
        return window_start, None

    def write_samples(
        self,
        node_id: int,
        samples: list[SampleRecord],
    ) -> None:
        grouped: dict[datetime, list[SampleRecord]] = {}
        quarantined: dict[str, list[SampleRecord]] = {}
        for sample in samples:
            window_start, reason = self._classify_sample(sample)
            if reason is not None:
                quarantined.setdefault(reason, []).append(sample)
            else:
                assert window_start is not None
                grouped.setdefault(window_start, []).append(sample)
        for window_start in sorted(grouped):
            self._ensure_window(window_start).write_samples(
                node_id,
                grouped[window_start],
            )
        for reason, reason_samples in sorted(quarantined.items()):
            self._ensure_quarantine(reason).write_samples(
                node_id,
                reason_samples,
            )

    def _latest_writer(self) -> Hdf5Writer:
        if not self.active_windows:
            return self._ensure_window(
                self.aligned_window_start(datetime.now(timezone.utc))
            )
        return self.active_windows[max(self.active_windows)][2]

    def write_temperature(
        self,
        node_id: int,
        records: list[TemperatureRecord],
    ) -> None:
        if not self.active_windows:
            self.pending_temperatures.setdefault(node_id, []).extend(records)
            return
        self._latest_writer().write_temperature(node_id, records)

    def write_gap(
        self,
        node_id: int,
        expected_sample_seq: int,
        received_sample_seq: int,
        packet_seq: int,
        boot_epoch: int = 0,
    ) -> None:
        if not self.active_windows:
            self.pending_gaps.append((
                node_id,
                expected_sample_seq,
                received_sample_seq,
                packet_seq,
                boot_epoch,
            ))
            return
        self._latest_writer().write_gap(
            node_id,
            expected_sample_seq,
            received_sample_seq,
            packet_seq,
            boot_epoch,
        )

    def write_clock_sync(
        self,
        node_id: int,
        observation: TimeSyncObservation,
        accepted: bool,
    ) -> None:
        self.pending_clock_sync.setdefault(node_id, []).append(
            (observation, accepted)
        )
        self.pending_clock_sync[node_id] = self.pending_clock_sync[node_id][-64:]
        if not self.active_windows:
            pass
        else:
            self._latest_writer().write_clock_sync(
                node_id,
                observation,
                accepted,
            )
        for _, _, writer in self.quarantine_writers.values():
            writer.write_clock_sync(node_id, observation, accepted)

    def write_quality_event(
        self,
        node_id: int,
        record: QualityEventRecord,
    ) -> None:
        if self.active_windows:
            self._latest_writer().write_quality_event(node_id, record)

    def flush(self) -> None:
        for _, _, writer in self.active_windows.values():
            writer.flush()
        for _, _, writer in self.quarantine_writers.values():
            writer.flush()

    def close(self) -> None:
        for window_start in sorted(tuple(self.active_windows)):
            self._finalize_window(window_start)
        for reason in sorted(tuple(self.quarantine_writers)):
            final_path, partial_path, writer = (
                self.quarantine_writers.pop(reason)
            )
            self._publish_writer(final_path, partial_path, writer)
            print(
                f"[QUARANTINE] {final_path} "
                f"reason={reason} (complete)"
            )
        self.writer = None
        self.current_window_start = None
        self.current_window_end = None
        self.current_path = None
