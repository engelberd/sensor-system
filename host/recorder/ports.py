"""Narrow output ports for capture-domain data."""

from __future__ import annotations

from abc import ABC, abstractmethod

from host.common.clock_models import TimeSyncObservation
from host.recorder.model import (
    QualityEventRecord,
    RecorderNode,
    SampleRecord,
    TemperatureRecord,
)


class BaseWriter(ABC):
    """Composite capture sink used by the recorder application.

    Concrete storage implementations own serialization only. Window routing
    may decorate this port without knowing the underlying file format.
    """

    @abstractmethod
    def add_node(self, node: RecorderNode) -> None:
        raise NotImplementedError

    @abstractmethod
    def write_samples(self, node_id: int, samples: list[SampleRecord]) -> None:
        raise NotImplementedError

    @abstractmethod
    def write_temperature(
        self,
        node_id: int,
        records: list[TemperatureRecord],
    ) -> None:
        raise NotImplementedError

    @abstractmethod
    def write_gap(
        self,
        node_id: int,
        expected_sample_seq: int,
        received_sample_seq: int,
        packet_seq: int,
        boot_epoch: int = 0,
    ) -> None:
        raise NotImplementedError

    def write_clock_sync(
        self,
        node_id: int,
        observation: TimeSyncObservation,
        accepted: bool,
    ) -> None:
        del node_id, observation, accepted

    def write_quality_event(
        self,
        node_id: int,
        record: QualityEventRecord,
    ) -> None:
        del node_id, record

    @abstractmethod
    def flush(self) -> None:
        raise NotImplementedError

    @abstractmethod
    def close(self) -> None:
        raise NotImplementedError
