"""UTC window routing for one-sensor Capture HDF5 v1 files."""

from __future__ import annotations

import argparse
from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
from functools import wraps
from typing import Callable, TypeVar
from uuid import uuid4

from host.common.clock_models import TimeSyncObservation
from host.recorder.capture_v1 import CaptureV1Writer
from host.recorder.capture_reader import CaptureV1Reader
from host.recorder.contracts import CaptureRoutingFlag, SensorIdentity
from host.recorder.model import (
    QualityEventRecord,
    RecorderNode,
    SampleRecord,
    TemperatureRecord,
)
from host.recorder.ports import BaseWriter, StorageError


ResultT = TypeVar("ResultT")


def storage_boundary(operation: Callable[..., ResultT]) -> Callable[..., ResultT]:
    """Keep HDF5/filesystem failures distinct from node protocol failures."""
    @wraps(operation)
    def wrapped(*args, **kwargs):
        try:
            return operation(*args, **kwargs)
        except StorageError:
            raise
        except (OSError, RuntimeError) as exc:
            raise StorageError(
                f"Capture storage operation '{operation.__name__}' failed: {exc}"
            ) from exc
    return wrapped


class CaptureV1WindowedWriter(BaseWriter):
    def __init__(
        self,
        args: argparse.Namespace,
        metadata: dict[str, object],
        nodes: list[RecorderNode],
        identity: SensorIdentity,
    ) -> None:
        if len(nodes) != 1:
            raise RuntimeError("Capture v1 requires exactly one node per channel")
        self.args = args
        self.metadata = metadata
        self.node = nodes[0]
        self.identity = identity
        self.active_windows: dict[
            datetime, tuple[Path, Path, CaptureV1Writer]
        ] = {}
        self.finalized_windows: set[datetime] = set()
        self.max_open_windows = 3
        self.pending_clock_sync: list[tuple[TimeSyncObservation, bool]] = []
        self.pending_temperatures: list[TemperatureRecord] = []
        self.pending_gaps: list[tuple[int, int, int, int]] = []
        self.seen_sample_keys: set[tuple[int, int]] = set()
        self.finalized_key_cache: dict[datetime, set[tuple[int, int]]] = {}
        self.unresolved: tuple[Path, Path, CaptureV1Writer] | None = None
        self.session_id = str(uuid4())
        self.recovery_events: list[dict[str, object]] = []
        self.current_path: Path | None = None
        self.current_window_start: datetime | None = None
        self.current_window_end: datetime | None = None
        self._recover_partial_windows()

    def aligned_window_start(self, when_utc: datetime) -> datetime:
        timestamp = when_utc.astimezone(timezone.utc).timestamp()
        aligned = (int(timestamp) // self.args.window_seconds) * self.args.window_seconds
        return datetime.fromtimestamp(aligned, tz=timezone.utc)

    def current_window(self, now_utc: datetime | None = None) -> datetime:
        resolved = now_utc or datetime.now(timezone.utc)
        if self.current_window_start is not None and self.current_window_end is not None:
            if resolved < self.current_window_end:
                return self.current_window_start
        return self.aligned_window_start(resolved)

    def _paths_for(self, window_start: datetime) -> tuple[Path, Path]:
        label = window_start.strftime("%Y%m%dT%H%M%SZ")
        directory = Path(self.args.output_dir) / window_start.strftime("%Y-%m-%d")
        final = directory / (
            f"sensor-{self.identity.sensor_label}_{label}.capture.h5"
        )
        return final, Path(str(final) + ".partial")

    def _metadata_for(self, window_start: datetime) -> dict[str, object]:
        result = dict(self.metadata)
        end = window_start + timedelta(seconds=self.args.window_seconds)
        result.update({
            "capture_session_id": self.session_id,
            "time_assignment": "utc_window",
            "window_start_utc_ns": round(window_start.timestamp() * 1e9),
            "window_end_utc_ns": round(end.timestamp() * 1e9),
        })
        return result

    def _prepare_writer(self, writer: CaptureV1Writer) -> None:
        writer.add_node(self.node)
        for observation, accepted in self.pending_clock_sync:
            writer.write_clock_sync(self.node.node_id, observation, accepted)
        if self.pending_temperatures:
            writer.write_temperature(
                self.node.node_id,
                self.pending_temperatures,
            )
            self.pending_temperatures.clear()
        for expected, received, packet, boot_epoch in self.pending_gaps:
            writer.write_gap(
                self.node.node_id, expected, received, packet, boot_epoch
            )
        self.pending_gaps.clear()

    @staticmethod
    def _fsync_directory(path: Path) -> None:
        try:
            directory_fd = os.open(
                path,
                os.O_RDONLY | getattr(os, "O_DIRECTORY", 0),
            )
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
        except OSError:
            pass

    def _record_recovery_event(
        self,
        event: str,
        *,
        original: Path,
        result: Path,
        reason: str,
        error: str | None = None,
    ) -> None:
        payload: dict[str, object] = {
            "event": event,
            "original_path": str(original),
            "result_path": str(result),
            "reason": reason,
            "observed_utc": datetime.now(timezone.utc).isoformat(),
        }
        if error:
            payload["error"] = error
        self.recovery_events.append(payload)

    def _quarantine_partial(
        self,
        partial: Path,
        *,
        reason: str,
        error: Exception | str,
    ) -> Path:
        observed = datetime.now(timezone.utc)
        directory = (
            Path(self.args.output_dir)
            / "quarantine"
            / "partial-recovery"
            / observed.strftime("%Y-%m-%d")
        )
        directory.mkdir(parents=True, exist_ok=True)
        target = directory / (
            f"{partial.name}.{observed.strftime('%H%M%S%f')}Z-{uuid4().hex[:8]}"
        )
        try:
            os.replace(partial, target)
        except OSError as exc:
            raise StorageError(
                f"cannot quarantine unreadable Capture partial '{partial}': {exc}"
            ) from exc
        self._fsync_directory(partial.parent)
        self._fsync_directory(directory)

        payload = {
            "event": "partial_window_unrecoverable",
            "original_path": str(partial),
            "quarantine_path": str(target),
            "reason": reason,
            "error": str(error),
            "size_bytes": target.stat().st_size,
            "observed_utc": observed.isoformat(),
        }
        sidecar = Path(str(target) + ".recovery.json")
        sidecar_partial = Path(str(sidecar) + ".partial")
        try:
            with sidecar_partial.open("w", encoding="utf-8") as stream:
                json.dump(payload, stream, indent=2, sort_keys=True)
                stream.write("\n")
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(sidecar_partial, sidecar)
            self._fsync_directory(directory)
        except OSError:
            # The measurement has already been made safe. Runtime diagnostics
            # still carry the same metadata if the sidecar cannot be written.
            try:
                sidecar_partial.unlink(missing_ok=True)
            except OSError:
                pass
        self.recovery_events.append(payload)
        return target

    def _promote_completed_partial(
        self,
        final: Path,
        partial: Path,
    ) -> bool:
        """Publish a file that reached complete=true before a crash/rename."""
        try:
            with CaptureV1Reader(partial, allow_partial=True) as reader:
                if not bool(reader.file.attrs.get("complete", False)):
                    return False
                validation = reader.validate()
        except Exception:
            return False
        if not validation.valid:
            self._quarantine_partial(
                partial,
                reason="completed_partial_failed_validation",
                error="; ".join(validation.errors),
            )
            return True
        os.replace(partial, final)
        self._fsync_directory(final.parent)
        self._record_recovery_event(
            "partial_window_published",
            original=partial,
            result=final,
            reason="complete_before_interrupted_rename",
        )
        return True

    def _recover_partial_windows(self) -> None:
        """Resume the current UTC window and publish windows that already ended."""
        output_dir = Path(self.args.output_dir)
        prefix = f"sensor-{self.identity.sensor_label}_"
        suffix = ".capture.h5.partial"
        pattern = f"????-??-??/{prefix}????????T??????Z{suffix}"
        now = datetime.now(timezone.utc)
        for partial in sorted(output_dir.glob(pattern)):
            label = partial.name[len(prefix):-len(suffix)]
            try:
                window_start = datetime.strptime(
                    label,
                    "%Y%m%dT%H%M%SZ",
                ).replace(tzinfo=timezone.utc)
            except ValueError as exc:
                raise RuntimeError(
                    f"invalid Capture partial window name: {partial}"
                ) from exc
            final = Path(str(partial)[:-len(".partial")])
            if final.exists():
                self._quarantine_partial(
                    partial,
                    reason="published_file_already_exists",
                    error=f"final file already exists: {final}",
                )
                continue
            if self._promote_completed_partial(final, partial):
                self.finalized_windows.add(window_start)
                continue
            writer: CaptureV1Writer | None = None
            try:
                writer = CaptureV1Writer(
                    partial,
                    self.identity,
                    self._metadata_for(window_start),
                    compression=self.args.compression,
                    append=True,
                )
                self._prepare_writer(writer)
            except Exception as exc:
                if writer is not None:
                    try:
                        writer.close()
                    except Exception:
                        pass
                self._quarantine_partial(
                    partial,
                    reason="resume_failed",
                    error=exc,
                )
                continue
            self.seen_sample_keys.update(writer.committed_keys())
            window_end = window_start + timedelta(seconds=self.args.window_seconds)
            if window_end <= now:
                self._publish(final, partial, writer)
                self.finalized_windows.add(window_start)
                continue
            self.active_windows[window_start] = (final, partial, writer)
            self.current_window_start = window_start
            self.current_window_end = window_end
            self.current_path = final

    def _ensure_window(self, window_start: datetime) -> CaptureV1Writer:
        existing = self.active_windows.get(window_start)
        if existing is not None:
            return existing[2]
        final, partial = self._paths_for(window_start)
        if final.exists() or window_start in self.finalized_windows:
            raise RuntimeError("Capture window is already finalized")
        writer = CaptureV1Writer(
            partial,
            self.identity,
            self._metadata_for(window_start),
            compression=self.args.compression,
            append=partial.exists(),
        )
        self._prepare_writer(writer)
        self.seen_sample_keys.update(writer.committed_keys())
        self.active_windows[window_start] = (final, partial, writer)
        self.current_window_start = window_start
        self.current_window_end = window_start + timedelta(
            seconds=self.args.window_seconds
        )
        self.current_path = final
        while len(self.active_windows) > self.max_open_windows:
            self._finalize_window(min(self.active_windows))
        return writer

    def _ensure_unresolved(self) -> CaptureV1Writer:
        if self.unresolved is not None:
            return self.unresolved[2]
        created = datetime.now(timezone.utc)
        directory = (
            Path(self.args.output_dir)
            / "unresolved"
            / created.strftime("%Y-%m-%d")
        )
        final = directory / (
            f"sensor-{self.identity.sensor_label}_session-{self.session_id}.capture.h5"
        )
        partial = Path(str(final) + ".partial")
        metadata = dict(self.metadata)
        metadata.update({
            "capture_session_id": self.session_id,
            "time_assignment": "unresolved",
        })
        writer = CaptureV1Writer(
            partial,
            self.identity,
            metadata,
            compression=self.args.compression,
            append=partial.exists(),
        )
        self._prepare_writer(writer)
        self.unresolved = (final, partial, writer)
        return writer

    @staticmethod
    def _publish(final: Path, partial: Path, writer: CaptureV1Writer) -> None:
        writer.finalize()
        writer.close()
        os.replace(partial, final)
        try:
            directory_fd = os.open(
                final.parent,
                os.O_RDONLY | getattr(os, "O_DIRECTORY", 0),
            )
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
        except OSError:
            pass

    def _finalize_window(self, window_start: datetime) -> None:
        final, partial, writer = self.active_windows.pop(window_start)
        self._publish(final, partial, writer)
        self.finalized_windows.add(window_start)

    def _route(self, sample: SampleRecord) -> tuple[datetime | None, int]:
        if sample.acquisition_utc_ns is None:
            return None, int(CaptureRoutingFlag.INITIALLY_UNSYNCED)
        flags = CaptureRoutingFlag.UTC_VALID
        when = datetime.fromtimestamp(
            sample.acquisition_utc_ns / 1e9,
            tz=timezone.utc,
        )
        start = self.aligned_window_start(when)
        end = start + timedelta(seconds=self.args.window_seconds)
        uncertainty = sample.timing_uncertainty_ns or 0
        start_ns = round(start.timestamp() * 1e9)
        end_ns = round(end.timestamp() * 1e9)
        if (
            sample.acquisition_utc_ns - uncertainty < start_ns
            or sample.acquisition_utc_ns + uncertainty >= end_ns
        ):
            flags |= CaptureRoutingFlag.BOUNDARY_UNCERTAIN
        final, _ = self._paths_for(start)
        if start in self.finalized_windows or final.exists():
            flags |= CaptureRoutingFlag.LATE_ARRIVAL
            return None, int(flags)
        return start, int(flags)

    def _exists_in_finalized_window(
        self,
        window_start: datetime,
        sample: SampleRecord,
    ) -> bool:
        keys = self.finalized_key_cache.get(window_start)
        if keys is None:
            final, _ = self._paths_for(window_start)
            keys = set()
            if final.exists():
                with CaptureV1Reader(final) as reader:
                    boots, sequences = reader.sample_identity()
                    keys.update(
                        (int(boot), int(sequence))
                        for boot, sequence in zip(boots, sequences)
                    )
            self.finalized_key_cache[window_start] = keys
        return (sample.boot_epoch, sample.sample_seq) in keys

    @storage_boundary
    def add_node(self, node: RecorderNode) -> None:
        if node.node_id != self.node.node_id:
            raise RuntimeError("unexpected node for Capture v1 channel")

    @storage_boundary
    def write_samples(self, node_id: int, samples: list[SampleRecord]) -> None:
        grouped: dict[datetime, list[SampleRecord]] = {}
        unresolved: list[SampleRecord] = []
        for sample in samples:
            key = (sample.boot_epoch, sample.sample_seq)
            if key in self.seen_sample_keys:
                continue
            estimated_window = None
            if sample.acquisition_utc_ns is not None:
                estimated_window = self.aligned_window_start(
                    datetime.fromtimestamp(
                        sample.acquisition_utc_ns / 1e9,
                        tz=timezone.utc,
                    )
                )
                final, _ = self._paths_for(estimated_window)
                if final.exists() and self._exists_in_finalized_window(
                    estimated_window, sample
                ):
                    self.seen_sample_keys.add(key)
                    continue
            window, flags = self._route(sample)
            sample.routing_flags = flags
            if window is None:
                unresolved.append(sample)
            else:
                grouped.setdefault(window, []).append(sample)
        for window in sorted(grouped):
            self._ensure_window(window).write_samples(node_id, grouped[window])
            self.seen_sample_keys.update(
                (sample.boot_epoch, sample.sample_seq)
                for sample in grouped[window]
            )
        if unresolved:
            self._ensure_unresolved().write_samples(node_id, unresolved)
            self.seen_sample_keys.update(
                (sample.boot_epoch, sample.sample_seq)
                for sample in unresolved
            )

    def _latest(self) -> CaptureV1Writer:
        if self.active_windows:
            return self.active_windows[max(self.active_windows)][2]
        return self._ensure_unresolved()

    @storage_boundary
    def write_temperature(
        self,
        node_id: int,
        records: list[TemperatureRecord],
    ) -> None:
        if not self.active_windows and self.unresolved is None:
            self.pending_temperatures.extend(records)
            return
        self._latest().write_temperature(node_id, records)

    @storage_boundary
    def write_gap(
        self,
        node_id: int,
        expected_sample_seq: int,
        received_sample_seq: int,
        packet_seq: int,
        boot_epoch: int = 0,
    ) -> None:
        if not self.active_windows and self.unresolved is None:
            self.pending_gaps.append(
                (expected_sample_seq, received_sample_seq, packet_seq, boot_epoch)
            )
            return
        self._latest().write_gap(
            node_id,
            expected_sample_seq,
            received_sample_seq,
            packet_seq,
            boot_epoch,
        )

    @storage_boundary
    def write_clock_sync(
        self,
        node_id: int,
        observation: TimeSyncObservation,
        accepted: bool,
    ) -> None:
        self.pending_clock_sync.append((observation, accepted))
        self.pending_clock_sync = self.pending_clock_sync[-64:]
        for _, _, writer in self.active_windows.values():
            writer.write_clock_sync(node_id, observation, accepted)
        if self.unresolved is not None:
            self.unresolved[2].write_clock_sync(node_id, observation, accepted)

    @storage_boundary
    def write_quality_event(
        self,
        node_id: int,
        record: QualityEventRecord,
    ) -> None:
        self._latest().write_quality_event(node_id, record)

    @storage_boundary
    def flush(self) -> None:
        for _, _, writer in self.active_windows.values():
            writer.flush()
        if self.unresolved is not None:
            self.unresolved[2].flush()

    @storage_boundary
    def close(self) -> None:
        now = datetime.now(timezone.utc)
        for start in sorted(tuple(self.active_windows)):
            final, partial, writer = self.active_windows.pop(start)
            if start + timedelta(seconds=self.args.window_seconds) <= now:
                self._publish(final, partial, writer)
                self.finalized_windows.add(start)
            else:
                writer.flush()
                writer.close()
        if self.unresolved is not None:
            final, partial, writer = self.unresolved
            self._publish(final, partial, writer)
            self.unresolved = None
        self.current_path = None
