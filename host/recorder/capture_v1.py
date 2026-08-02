"""Crash-recoverable Capture HDF5 v1 writer."""

from __future__ import annotations

from datetime import datetime, timezone
import os
from pathlib import Path
from uuid import uuid4

from host.common.clock_models import TimeSyncObservation
from host.recorder.contracts import (
    CAPTURE_SCHEMA_MAJOR,
    CAPTURE_SCHEMA_MINOR,
    DataProduct,
    SensorIdentity,
)
from host.recorder.model import (
    QualityEventRecord,
    RecorderNode,
    SampleRecord,
    TemperatureRecord,
)
from host.recorder.ports import BaseWriter


SIGNED24_MIN = -(1 << 23)
SIGNED24_MAX = (1 << 23) - 1


class CaptureV1Writer(BaseWriter):
    """Append one sensor stream to a versioned Capture file."""

    def __init__(
        self,
        path: Path,
        identity: SensorIdentity,
        metadata: dict[str, object] | None = None,
        *,
        compression: str = "gzip",
        append: bool = False,
    ) -> None:
        try:
            import h5py  # type: ignore
            import numpy as np  # type: ignore
        except ImportError as exc:
            raise RuntimeError("Capture HDF5 requires h5py and numpy") from exc

        self.h5py = h5py
        self.np = np
        self.path = Path(path)
        self.identity = identity
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.file = h5py.File(self.path, "a" if append else "w")
        self.node_id: int | None = None
        self.compression = None if compression == "none" else compression
        self._dataset_options = {
            "compression": self.compression,
            "shuffle": self.compression is not None,
            "fletcher32": True,
        }

        self._initialize_attributes(metadata or {}, append=append)
        self._initialize_datasets()
        if append:
            self._recover_uncommitted_tail()
        self._seen_keys = self._load_committed_keys()

    def _initialize_attributes(
        self,
        metadata: dict[str, object],
        *,
        append: bool,
    ) -> None:
        attrs = self.file.attrs
        if append:
            if attrs.get("format_name") != DataProduct.CAPTURE.value:
                raise RuntimeError("not a Capture HDF5 v1 file")
            if int(attrs.get("schema_major", -1)) != CAPTURE_SCHEMA_MAJOR:
                raise RuntimeError("unsupported Capture schema major version")
            if bool(attrs.get("complete", False)):
                raise RuntimeError("cannot append to a completed Capture file")
            if (
                int(attrs.get("channel_id", -1)) != self.identity.channel_id
                or str(attrs.get("sensor_label", ""))
                != self.identity.sensor_label
                or int(attrs.get("node_address", -1))
                != self.identity.node_address
            ):
                raise RuntimeError("Capture identity does not match existing file")
            return

        attrs["format_name"] = DataProduct.CAPTURE.value
        attrs["schema_major"] = CAPTURE_SCHEMA_MAJOR
        attrs["schema_minor"] = CAPTURE_SCHEMA_MINOR
        attrs["file_id"] = str(uuid4())
        attrs["channel_id"] = self.identity.channel_id
        attrs["sensor_label"] = self.identity.sensor_label
        attrs["sensor_id"] = self.identity.sensor_id or ""
        attrs["node_address"] = self.identity.node_address
        attrs["hardware_id"] = self.identity.hardware_id or ""
        attrs["board_revision"] = self.identity.board_revision or 0
        attrs["created_utc_ns"] = int(
            datetime.now(timezone.utc).timestamp() * 1_000_000_000
        )
        attrs["finalized_utc_ns"] = -1
        attrs["complete"] = False
        for key, value in metadata.items():
            attrs[key] = value

    def _initialize_datasets(self) -> None:
        np = self.np
        measurement = self.file.require_group("measurement")
        measurement.attrs["axes"] = "X,Y,Z"
        measurement.attrs["encoding"] = "signed_integer"
        measurement.attrs["valid_bits"] = 24
        measurement.attrs["unit"] = "sensor_lsb"
        measurement.attrs["processing_stage"] = (
            "firmware_filtered_decimated"
        )
        self.raw_xyz = measurement.require_dataset(
            "raw_xyz",
            shape=(0, 3),
            maxshape=(None, 3),
            chunks=(8192, 3),
            dtype="<i4",
            **self._dataset_options,
        )

        block_dtype = np.dtype([
            ("sample_offset", "<u8"),
            ("sample_count", "<u4"),
            ("boot_epoch", "<u8"),
            ("first_sample_seq", "<u8"),
            ("timing_segment_id", "<u4"),
            ("source_packet_seq", "<u4"),
            ("timing_format_version", "u1"),
            ("timestamp_source", "u1"),
            ("first_device_time_us", "<u8"),
            ("last_device_time_us", "<u8"),
            ("sample_period_q16_us", "<u4"),
            ("max_fit_residual_us", "<u4"),
            ("first_utc_ns", "<i8"),
            ("last_utc_ns", "<i8"),
            ("uncertainty_ns", "<u8"),
            ("timing_flags", "<u2"),
            ("routing_flags", "<u2"),
        ])
        self.blocks = self.file.require_group("capture").require_dataset(
            "blocks",
            shape=(0,),
            maxshape=(None,),
            chunks=(256,),
            dtype=block_dtype,
            **self._dataset_options,
        )

        self.temperature = self.file.require_group(
            "environment"
        ).require_dataset(
            "temperature",
            shape=(0,),
            maxshape=(None,),
            chunks=(256,),
            dtype=np.dtype([
                ("boot_epoch", "<u8"),
                ("sample_seq_anchor", "<u8"),
                ("raw", "<u2"),
                ("celsius", "<f4"),
                ("observed_utc_ns", "<i8"),
            ]),
            **self._dataset_options,
        )
        self.gaps = self.file.require_group("stream").require_dataset(
            "gaps",
            shape=(0,),
            maxshape=(None,),
            chunks=(256,),
            dtype=np.dtype([
                ("boot_epoch", "<u8"),
                ("expected_sample_seq", "<u8"),
                ("received_sample_seq", "<u8"),
                ("source_packet_seq", "<u4"),
            ]),
            **self._dataset_options,
        )
        self.clock_sync = self.file.require_group(
            "diagnostics"
        ).require_dataset(
            "clock_sync",
            shape=(0,),
            maxshape=(None,),
            chunks=(256,),
            dtype=np.dtype([
                ("boot_epoch", "<u8"),
                ("sync_id", "<u4"),
                ("t1_host_monotonic_ns", "<u8"),
                ("t2_node_rx_us", "<u8"),
                ("t3_node_tx_us", "<u8"),
                ("t4_host_monotonic_ns", "<u8"),
                ("network_rtt_ns", "<i8"),
                ("accepted", "u1"),
            ]),
            **self._dataset_options,
        )
        self.quality_events = self.file.require_group(
            "quality"
        ).require_dataset(
            "events",
            shape=(0,),
            maxshape=(None,),
            chunks=(256,),
            dtype=np.dtype([
                ("event_id", "<u8"),
                ("boot_epoch", "<u8"),
                ("sample_seq_anchor", "<u8"),
                ("observed_utc_ns", "<i8"),
                ("event_code", "<u2"),
                ("severity", "u1"),
                ("flags", "<u4"),
                ("count", "<u8"),
                ("value_a", "<i8"),
                ("value_b", "<i8"),
            ]),
            **self._dataset_options,
        )
        self.configurations = self.file.require_group(
            "configuration"
        ).require_dataset(
            "intervals",
            shape=(0,),
            maxshape=(None,),
            chunks=(32,),
            dtype=np.dtype([
                ("start_boot_epoch", "<u8"),
                ("start_sample_seq", "<u8"),
                ("sensor_odr_hz", "<u4"),
                ("output_odr_hz", "<f8"),
                ("range_g", "u1"),
                ("high_pass_corner", "u1"),
                ("offset_x", "<i4"),
                ("offset_y", "<i4"),
                ("offset_z", "<i4"),
                ("decimation_factor", "u1"),
                ("filter_profile", "u1"),
                ("config_revision", "<u4"),
                ("calibration_method", "S32"),
            ]),
            **self._dataset_options,
        )

    def _recover_uncommitted_tail(self) -> None:
        committed = 0
        for block in self.blocks:
            committed = max(
                committed,
                int(block["sample_offset"]) + int(block["sample_count"]),
            )
        if self.raw_xyz.shape[0] > committed:
            self.raw_xyz.resize((committed, 3))
            self.flush()

    def add_node(self, node: RecorderNode) -> None:
        if self.node_id is not None and self.node_id != node.node_id:
            raise RuntimeError("Capture v1 file accepts exactly one node")
        if node.node_id != self.identity.node_address:
            raise RuntimeError("node address does not match sensor identity")
        self.node_id = node.node_id
        self.file.attrs["firmware_version"] = node.firmware_version or ""
        if self.configurations.shape[0] != 0:
            return
        config = node.config
        decimation = int(getattr(config, "decimation_factor", 2))
        filter_profile = int(getattr(config, "filter_profile", 255))
        config_revision = int(getattr(config, "config_revision", 0))
        if filter_profile not in (0, 1, 2):
            raise RuntimeError(
                "Capture v1 requires firmware reporting filter_profile"
            )
        if decimation <= 0:
            raise RuntimeError("Capture v1 requires a valid decimation factor")
        if config_revision <= 0:
            raise RuntimeError("Capture v1 requires a valid config revision")
        offset = self.configurations.shape[0]
        self.configurations.resize((offset + 1,))
        self.configurations[offset] = (
            0,
            int(getattr(config, "config_effective_sample_seq", 0)),
            int(config.odr_hz),
            float(config.odr_hz) / decimation,
            int(config.range_g),
            int(config.high_pass_corner),
            int(config.offset_x),
            int(config.offset_y),
            int(config.offset_z),
            decimation,
            filter_profile,
            config_revision,
            b"sensor_offset_registers",
        )

    def _validate_samples(self, node_id: int, samples: list[SampleRecord]) -> None:
        if self.node_id is None:
            raise RuntimeError("add_node must be called before writing")
        if node_id != self.node_id:
            raise RuntimeError("sample node does not match Capture identity")
        for sample in samples:
            if sample.node_id != node_id:
                raise ValueError("mixed node ids in sample batch")
            if sample.range_g not in (2, 4, 8):
                raise ValueError("unsupported sample range")
            if any(
                value < SIGNED24_MIN or value > SIGNED24_MAX
                for value in sample.raw_xyz
            ):
                raise ValueError("raw acceleration exceeds signed-24 range")

    @staticmethod
    def _same_block(previous: SampleRecord, current: SampleRecord) -> bool:
        return (
            current.boot_epoch == previous.boot_epoch
            and current.sample_seq == previous.sample_seq + 1
            and current.packet_seq == previous.packet_seq
            and current.timing_segment_id == previous.timing_segment_id
            and current.timing_quality_flags == previous.timing_quality_flags
            and current.routing_flags == previous.routing_flags
        )

    def _load_committed_keys(self) -> set[tuple[int, int]]:
        keys: set[tuple[int, int]] = set()
        for block in self.blocks:
            boot = int(block["boot_epoch"])
            first = int(block["first_sample_seq"])
            count = int(block["sample_count"])
            keys.update((boot, first + index) for index in range(count))
        return keys

    def committed_keys(self) -> set[tuple[int, int]]:
        """Return a snapshot used by the session-level routing decorator."""
        return set(self._seen_keys)

    def write_samples(self, node_id: int, samples: list[SampleRecord]) -> None:
        if not samples:
            return
        self._validate_samples(node_id, samples)
        samples = [
            sample
            for sample in samples
            if (sample.boot_epoch, sample.sample_seq) not in self._seen_keys
        ]
        if not samples:
            return

        fragments: list[list[SampleRecord]] = []
        for sample in samples:
            if not fragments or not self._same_block(fragments[-1][-1], sample):
                fragments.append([sample])
            else:
                fragments[-1].append(sample)

        sample_offset = int(self.raw_xyz.shape[0])
        raw = self.np.asarray(
            [sample.raw_xyz for sample in samples],
            dtype="<i4",
        )
        self.raw_xyz.resize((sample_offset + len(samples), 3))
        self.raw_xyz[sample_offset:] = raw
        self.flush()

        block_offset = int(self.blocks.shape[0])
        self.blocks.resize((block_offset + len(fragments),))
        rows = []
        fragment_offset = sample_offset
        for fragment in fragments:
            first = fragment[0]
            last = fragment[-1]
            rows.append((
                fragment_offset,
                len(fragment),
                first.boot_epoch,
                first.sample_seq,
                first.timing_segment_id,
                first.packet_seq,
                first.timing_format_version,
                first.timestamp_source,
                first.device_time_us or 0,
                last.device_time_us or 0,
                first.sample_period_q16_us,
                max(sample.max_fit_residual_us for sample in fragment),
                first.acquisition_utc_ns
                if first.acquisition_utc_ns is not None else -1,
                last.acquisition_utc_ns
                if last.acquisition_utc_ns is not None else -1,
                max(sample.timing_uncertainty_ns or 0 for sample in fragment),
                first.timing_quality_flags,
                first.routing_flags,
            ))
            fragment_offset += len(fragment)
        self.blocks[block_offset:] = self.np.asarray(
            rows,
            dtype=self.blocks.dtype,
        )
        self._seen_keys.update(
            (sample.boot_epoch, sample.sample_seq) for sample in samples
        )
        self.flush()

    def write_temperature(
        self,
        node_id: int,
        records: list[TemperatureRecord],
    ) -> None:
        if not records:
            return
        if node_id != self.node_id:
            raise RuntimeError("temperature node does not match Capture identity")
        offset = int(self.temperature.shape[0])
        self.temperature.resize((offset + len(records),))
        self.temperature[offset:] = self.np.asarray(
            [
                (record.boot_epoch, record.sample_seq_anchor, record.temp_raw,
                 record.temp_celsius, record.observed_utc_ns)
                for record in records
            ],
            dtype=self.temperature.dtype,
        )

    def write_gap(
        self,
        node_id: int,
        expected_sample_seq: int,
        received_sample_seq: int,
        packet_seq: int,
        boot_epoch: int = 0,
    ) -> None:
        if node_id != self.node_id:
            raise RuntimeError("gap node does not match Capture identity")
        offset = int(self.gaps.shape[0])
        self.gaps.resize((offset + 1,))
        self.gaps[offset] = (boot_epoch, expected_sample_seq, received_sample_seq,
                             packet_seq)

    def write_clock_sync(
        self,
        node_id: int,
        observation: TimeSyncObservation,
        accepted: bool,
    ) -> None:
        if node_id != self.node_id:
            raise RuntimeError("clock node does not match Capture identity")
        offset = int(self.clock_sync.shape[0])
        self.clock_sync.resize((offset + 1,))
        self.clock_sync[offset] = (
            observation.boot_epoch,
            observation.sync_id,
            observation.t1_host_monotonic_ns,
            observation.t2_node_rx_us,
            observation.t3_node_tx_us,
            observation.t4_host_monotonic_ns,
            observation.network_rtt_ns,
            1 if accepted else 0,
        )

    def write_quality_event(
        self,
        node_id: int,
        record: QualityEventRecord,
    ) -> None:
        if node_id != self.node_id:
            raise RuntimeError("quality event node does not match Capture identity")
        offset = int(self.quality_events.shape[0])
        self.quality_events.resize((offset + 1,))
        self.quality_events[offset] = (
            offset + 1,
            record.boot_epoch,
            record.sample_seq_anchor,
            record.observed_utc_ns,
            record.event_code,
            record.severity,
            record.flags,
            record.count,
            record.value_a,
            record.value_b,
        )

    def flush(self) -> None:
        self.file.flush()
        try:
            handle = self.file.id.get_vfd_handle()
            if isinstance(handle, int):
                os.fsync(handle)
        except (AttributeError, OSError, TypeError):
            pass

    def finalize(self) -> None:
        self.file.attrs["complete"] = True
        self.file.attrs["finalized_utc_ns"] = int(
            datetime.now(timezone.utc).timestamp() * 1_000_000_000
        )
        self.flush()

    def close(self) -> None:
        self.flush()
        self.file.close()
