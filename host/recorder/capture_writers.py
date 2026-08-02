"""Concrete short-retention Capture storage writers."""

from __future__ import annotations

import csv
from datetime import datetime, timezone
import json
import os
from pathlib import Path

from host.common.clock_models import TimeSyncObservation
from host.host_lab import effective_output_odr_hz
from host.recorder.model import RecorderNode, SampleRecord, TemperatureRecord
from host.recorder.ports import BaseWriter


CAPTURE_SCHEMA_VERSION = 5

class CsvWriter(BaseWriter):
    def __init__(self, path: Path, metadata: dict[str, object], append: bool = False) -> None:
        self.path = path
        self.append_mode = append
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.metadata = metadata
        self.node_metadata: list[dict[str, object]] = []
        self.meta_path = self.path.with_suffix(self.path.suffix + ".meta.json")
        self.temperature_path = self.path.with_suffix(self.path.suffix + ".temperature.csv")
        self.gaps_path = self.path.with_suffix(self.path.suffix + ".gaps.csv")
        self.handle = self.path.open("a" if append else "w", newline="", encoding="utf-8")
        self.csv = csv.writer(self.handle)
        self.temperature_handle = self.temperature_path.open("a" if append else "w", newline="", encoding="utf-8")
        self.temperature_csv = csv.writer(self.temperature_handle)
        self.gaps_handle = self.gaps_path.open("a" if append else "w", newline="", encoding="utf-8")
        self.gaps_csv = csv.writer(self.gaps_handle)
        if not append or self.path.stat().st_size == 0:
            self.csv.writerow([
                "node_id",
                "sample_seq",
                "x",
                "y",
                "z",
                "packet_seq",
            ])
        if not append or self.temperature_path.stat().st_size == 0:
            self.temperature_csv.writerow([
                "node_id",
                "sample_seq_anchor",
                "temp_raw",
                "temp_celsius",
            ])
        if not append or self.gaps_path.stat().st_size == 0:
            self.gaps_csv.writerow([
                "node_id",
                "expected_sample_seq",
                "received_sample_seq",
                "packet_seq",
            ])

    def add_node(self, node: RecorderNode) -> None:
        self.node_metadata.append({
            "node_id": node.node_id,
            "sensor_odr_hz": node.config.odr_hz,
            "output_odr_hz": effective_output_odr_hz(node.config.odr_hz),
            "range_g": node.config.range_g,
            "accel_unit": "m/s^2",
            "high_pass_corner": node.config.high_pass_corner,
            "fifo_watermark": node.config.fifo_watermark,
            "offset_x": node.config.offset_x,
            "offset_y": node.config.offset_y,
            "offset_z": node.config.offset_z,
            "baseline_sensor_loss": node.baseline_sensor_loss,
            "baseline_rx_overflow_count": node.baseline_rx_overflow_count,
            "baseline_packet_overwrite_count": node.baseline_packet_overwrite_count,
        })
        self.write_metadata()

    def write_metadata(self) -> None:
        payload = dict(self.metadata)
        payload["nodes"] = self.node_metadata
        self.meta_path.write_text(
            json.dumps(payload, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )

    def write_samples(self, node_id: int, samples: list[SampleRecord]) -> None:
        del node_id
        for sample in samples:
            self.csv.writerow([
                sample.node_id,
                sample.sample_seq,
                sample.x,
                sample.y,
                sample.z,
                sample.packet_seq,
            ])

    def write_temperature(self, node_id: int, records: list[TemperatureRecord]) -> None:
        del node_id
        for record in records:
            self.temperature_csv.writerow([
                record.node_id,
                record.sample_seq_anchor,
                record.temp_raw,
                record.temp_celsius,
            ])

    def write_gap(
        self,
        node_id: int,
        expected_sample_seq: int,
        received_sample_seq: int,
        packet_seq: int,
        boot_epoch: int = 0,
    ) -> None:
        del boot_epoch
        self.gaps_csv.writerow([
            node_id,
            expected_sample_seq,
            received_sample_seq,
            packet_seq,
        ])

    def flush(self) -> None:
        self.handle.flush()
        self.temperature_handle.flush()
        self.gaps_handle.flush()
        os.fsync(self.handle.fileno())
        os.fsync(self.temperature_handle.fileno())
        os.fsync(self.gaps_handle.fileno())

    def close(self) -> None:
        self.flush()
        self.handle.close()
        self.temperature_handle.close()
        self.gaps_handle.close()


class Hdf5Writer(BaseWriter):
    def __init__(self, path: Path, metadata: dict[str, object], compression: str, append: bool = False) -> None:
        try:
            import h5py  # type: ignore
            import numpy as np  # type: ignore
        except ImportError as exc:
            raise RuntimeError(
                "HDF5 output requires numpy+h5py. Install them with: "
                "host/.venv/bin/python -m pip install -r host/requirements-recorder.txt"
            ) from exc

        self.h5py = h5py
        self.np = np
        self.path = path
        self.append_mode = append
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            self.file = h5py.File(self.path, "a" if append else "w")
        except PermissionError as exc:
            raise RuntimeError(
                f"cannot write HDF5 output to '{self.path}'. "
                "Choose a writable output directory."
            ) from exc
        except OSError as exc:
            raise RuntimeError(f"cannot prepare HDF5 output '{self.path}': {exc}") from exc
        self.nodes_group = self.file.require_group("nodes")
        self.datasets: dict[int, object] = {}
        self.temperature_datasets: dict[int, object] = {}
        self.gap_datasets: dict[int, object] = {}
        self.anchor_datasets: dict[int, object] = {}
        self.clock_sync_datasets: dict[int, object] = {}
        self.seen_sample_keys: dict[int, set[tuple[int, int]]] = {}
        self.compression = None if compression == "none" else compression
        ingest_dtype = self.np.dtype([
            ("node_id", "u1"),
            ("boot_epoch", "<u8"),
            ("first_sample_seq", "<u8"),
            ("last_sample_seq", "<u8"),
            ("sample_start", "<u8"),
            ("sample_end", "<u8"),
            ("anchor_start", "<u8"),
            ("anchor_end", "<u8"),
        ])
        self.ingest_batches = self.file.require_dataset(
            "ingest_batches",
            shape=(0,),
            maxshape=(None,),
            chunks=(256,),
            dtype=ingest_dtype,
            compression=self.compression,
        )

        self.file.attrs["schema_version"] = CAPTURE_SCHEMA_VERSION
        self.file.attrs["complete"] = False
        for key, value in metadata.items():
            self.file.attrs[key] = value

    def add_node(self, node: RecorderNode) -> None:
        group = self.nodes_group.require_group(str(node.node_id))
        group.attrs["node_id"] = node.node_id
        group.attrs["sensor_odr_hz"] = node.config.odr_hz
        group.attrs["output_odr_hz"] = effective_output_odr_hz(node.config.odr_hz)
        group.attrs["range_g"] = node.config.range_g
        group.attrs["accel_unit"] = "m/s^2"
        group.attrs["high_pass_corner"] = node.config.high_pass_corner
        group.attrs["fifo_watermark"] = node.config.fifo_watermark
        group.attrs["offset_x"] = node.config.offset_x
        group.attrs["offset_y"] = node.config.offset_y
        group.attrs["offset_z"] = node.config.offset_z
        group.attrs["baseline_sensor_loss"] = node.baseline_sensor_loss
        group.attrs["baseline_rx_overflow_count"] = node.baseline_rx_overflow_count
        group.attrs["baseline_packet_overwrite_count"] = node.baseline_packet_overwrite_count

        sample_dtype = self.np.dtype([
            ("sample_seq", "<u8"),
            ("boot_epoch", "<u8"),
            ("timing_segment_id", "<u4"),
            ("x", "<f4"),
            ("y", "<f4"),
            ("z", "<f4"),
            ("packet_seq", "<u4"),
        ])
        temperature_dtype = self.np.dtype([
            ("sample_seq_anchor", "<u8"),
            ("temp_raw", "<u2"),
            ("temp_celsius", "<f4"),
        ])
        gaps_dtype = self.np.dtype([
            ("expected_sample_seq", "<u8"),
            ("received_sample_seq", "<u8"),
            ("packet_seq", "<u4"),
        ])
        anchors_dtype = self.np.dtype([
            ("boot_epoch", "<u8"),
            ("timing_segment_id", "<u4"),
            ("packet_seq", "<u4"),
            ("first_sample_seq", "<u8"),
            ("sample_count", "<u2"),
            ("first_device_time_us", "<u8"),
            ("last_device_time_us", "<u8"),
            ("first_acquisition_utc_ns", "<u8"),
            ("last_acquisition_utc_ns", "<u8"),
            ("timing_quality_flags", "<u2"),
            ("max_uncertainty_ns", "<u8"),
        ])
        clock_sync_dtype = self.np.dtype([
            ("boot_epoch", "<u8"),
            ("sync_id", "<u4"),
            ("t1_host_monotonic_ns", "<u8"),
            ("t2_node_rx_us", "<u8"),
            ("t3_node_tx_us", "<u8"),
            ("t4_host_monotonic_ns", "<u8"),
            ("network_rtt_ns", "<i8"),
            ("accepted", "u1"),
        ])
        dataset = group.require_dataset(
            "samples",
            shape=(0,),
            maxshape=(None,),
            chunks=(4096,),
            dtype=sample_dtype,
            compression=self.compression,
        )
        temperature_dataset = group.require_dataset(
            "temperature",
            shape=(0,),
            maxshape=(None,),
            chunks=(256,),
            dtype=temperature_dtype,
            compression=self.compression,
        )
        gaps_dataset = group.require_dataset(
            "gaps",
            shape=(0,),
            maxshape=(None,),
            chunks=(256,),
            dtype=gaps_dtype,
            compression=self.compression,
        )
        anchors_dataset = group.require_dataset(
            "time_anchors",
            shape=(0,),
            maxshape=(None,),
            chunks=(256,),
            dtype=anchors_dtype,
            compression=self.compression,
        )
        clock_sync_dataset = group.require_dataset(
            "clock_sync",
            shape=(0,),
            maxshape=(None,),
            chunks=(256,),
            dtype=clock_sync_dtype,
            compression=self.compression,
        )
        if self.append_mode:
            sample_end = 0
            anchor_end = 0
            for batch in self.ingest_batches:
                if int(batch["node_id"]) != node.node_id:
                    continue
                sample_end = max(sample_end, int(batch["sample_end"]))
                anchor_end = max(anchor_end, int(batch["anchor_end"]))
            if dataset.shape[0] > sample_end:
                dataset.resize((sample_end,))
            if anchors_dataset.shape[0] > anchor_end:
                anchors_dataset.resize((anchor_end,))
        self.datasets[node.node_id] = dataset
        self.temperature_datasets[node.node_id] = temperature_dataset
        self.gap_datasets[node.node_id] = gaps_dataset
        self.anchor_datasets[node.node_id] = anchors_dataset
        self.clock_sync_datasets[node.node_id] = clock_sync_dataset
        self.seen_sample_keys[node.node_id] = {
            (int(record["boot_epoch"]), int(record["sample_seq"]))
            for record in dataset
        }

    def write_samples(self, node_id: int, samples: list[SampleRecord]) -> None:
        if not samples:
            return

        seen = self.seen_sample_keys[node_id]
        samples = [
            sample
            for sample in samples
            if (sample.boot_epoch, sample.sample_seq) not in seen
        ]
        if not samples:
            return
        dataset = self.datasets[node_id]
        offset = dataset.shape[0]
        dataset.resize((offset + len(samples),))

        arr = self.np.empty(len(samples), dtype=dataset.dtype)
        for i, sample in enumerate(samples):
            arr[i] = (
                sample.sample_seq,
                sample.boot_epoch,
                sample.timing_segment_id,
                sample.x,
                sample.y,
                sample.z,
                sample.packet_seq,
            )

        dataset[offset:offset + len(samples)] = arr
        seen.update(
            (sample.boot_epoch, sample.sample_seq)
            for sample in samples
        )

        anchors = self.anchor_datasets[node_id]
        anchor_offset = anchors.shape[0]
        timed_samples = [
            sample for sample in samples
            if sample.device_time_us is not None and sample.boot_epoch != 0
        ]
        timing_fragments: list[list[SampleRecord]] = []
        for sample in timed_samples:
            if not timing_fragments:
                timing_fragments.append([sample])
                continue
            previous = timing_fragments[-1][-1]
            same_fragment = (
                sample.boot_epoch == previous.boot_epoch
                and sample.timing_segment_id == previous.timing_segment_id
                and sample.packet_seq == previous.packet_seq
                and sample.timing_quality_flags
                == previous.timing_quality_flags
                and sample.sample_seq == previous.sample_seq + 1
            )
            if same_fragment:
                timing_fragments[-1].append(sample)
            else:
                timing_fragments.append([sample])
        if timing_fragments:
            anchors.resize((anchor_offset + len(timing_fragments),))
            anchor_records = []
            for fragment in timing_fragments:
                first = fragment[0]
                last = fragment[-1]
                max_uncertainty_ns = max(
                    sample.timing_uncertainty_ns or 0
                    for sample in fragment
                )
                anchor_records.append((
                    first.boot_epoch,
                    first.timing_segment_id,
                    first.packet_seq,
                    first.sample_seq,
                    len(fragment),
                    first.device_time_us,
                    last.device_time_us,
                    first.acquisition_utc_ns or 0,
                    last.acquisition_utc_ns or 0,
                    first.timing_quality_flags,
                    max_uncertainty_ns,
                ))
            anchors[
                anchor_offset:anchor_offset + len(timing_fragments)
            ] = self.np.array(
                anchor_records,
                dtype=anchors.dtype,
            )

        # Durably persist datasets before publishing the ingest marker.
        self.flush()
        marker_offset = self.ingest_batches.shape[0]
        self.ingest_batches.resize((marker_offset + 1,))
        self.ingest_batches[marker_offset:marker_offset + 1] = self.np.array(
            [(
                node_id,
                samples[0].boot_epoch,
                samples[0].sample_seq,
                samples[-1].sample_seq,
                offset,
                offset + len(samples),
                anchor_offset,
                anchors.shape[0],
            )],
            dtype=self.ingest_batches.dtype,
        )
        self.flush()

    def write_temperature(self, node_id: int, records: list[TemperatureRecord]) -> None:
        if not records:
            return

        dataset = self.temperature_datasets[node_id]
        offset = dataset.shape[0]
        dataset.resize((offset + len(records),))

        arr = self.np.empty(len(records), dtype=dataset.dtype)
        for i, record in enumerate(records):
            arr[i] = (
                record.sample_seq_anchor,
                record.temp_raw,
                record.temp_celsius,
            )

        dataset[offset:offset + len(records)] = arr

    def write_gap(
        self,
        node_id: int,
        expected_sample_seq: int,
        received_sample_seq: int,
        packet_seq: int,
        boot_epoch: int = 0,
    ) -> None:
        del boot_epoch
        dataset = self.gap_datasets[node_id]
        offset = dataset.shape[0]
        dataset.resize((offset + 1,))
        dataset[offset:offset + 1] = self.np.array(
            [(expected_sample_seq, received_sample_seq, packet_seq)],
            dtype=dataset.dtype,
        )

    def flush(self) -> None:
        self.file.flush()
        try:
            handle = self.file.id.get_vfd_handle()
            if isinstance(handle, int):
                os.fsync(handle)
        except (AttributeError, OSError, TypeError):
            pass

    def write_clock_sync(
        self,
        node_id: int,
        observation: TimeSyncObservation,
        accepted: bool,
    ) -> None:
        dataset = self.clock_sync_datasets[node_id]
        offset = dataset.shape[0]
        dataset.resize((offset + 1,))
        dataset[offset:offset + 1] = self.np.array(
            [(
                observation.boot_epoch,
                observation.sync_id,
                observation.t1_host_monotonic_ns,
                observation.t2_node_rx_us,
                observation.t3_node_tx_us,
                observation.t4_host_monotonic_ns,
                observation.network_rtt_ns,
                1 if accepted else 0,
            )],
            dtype=dataset.dtype,
        )

    def finalize(self) -> None:
        self.file.attrs["complete"] = True
        self.file.attrs["finalized_utc"] = (
            datetime.now(timezone.utc).isoformat()
        )
        self.flush()

    def close(self) -> None:
        self.flush()
        self.file.close()
