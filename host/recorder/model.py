"""Domain records and runtime state used by the capture pipeline."""

from __future__ import annotations

from collections import deque
from dataclasses import dataclass, field
from datetime import datetime
from typing import Optional

from host.common.clock_models import (
    NodeHostClockModel,
    SampleClockModel,
    UtcCorrelationModel,
)
from host.host_lab import ConfigView, NodeStats


STANDARD_GRAVITY_M_S2 = 9.80665


def scale_m_s2_per_lsb(range_g: int) -> float:
    scale_g_per_lsb = {2: 3.9e-6, 4: 7.8e-6, 8: 15.6e-6}.get(range_g)
    if scale_g_per_lsb is None:
        raise ValueError(f"unsupported acceleration range: {range_g}g")
    return scale_g_per_lsb * STANDARD_GRAVITY_M_S2


@dataclass
class SampleRecord:
    node_id: int
    sample_seq: int
    raw_x: int
    raw_y: int
    raw_z: int
    packet_seq: int
    range_g: int = 2
    device_time_us: Optional[int] = None
    boot_epoch: int = 0
    timing_segment_id: int = 0
    timing_quality_flags: int = 0
    timing_format_version: int = 0
    timestamp_source: int = 0
    sample_period_q16_us: int = 0
    max_fit_residual_us: int = 0
    routing_flags: int = 0
    acquisition_utc_ns: Optional[int] = None
    timing_uncertainty_ns: Optional[int] = None

    @property
    def x(self) -> float:
        return self.raw_x * scale_m_s2_per_lsb(self.range_g)

    @property
    def y(self) -> float:
        return self.raw_y * scale_m_s2_per_lsb(self.range_g)

    @property
    def z(self) -> float:
        return self.raw_z * scale_m_s2_per_lsb(self.range_g)

    @property
    def raw_xyz(self) -> tuple[int, int, int]:
        return self.raw_x, self.raw_y, self.raw_z


@dataclass
class TemperatureRecord:
    node_id: int
    sample_seq_anchor: int
    temp_raw: int
    temp_celsius: float
    boot_epoch: int = 0
    observed_utc_ns: int = -1


@dataclass(frozen=True)
class QualityEventRecord:
    boot_epoch: int
    sample_seq_anchor: int
    observed_utc_ns: int
    event_code: int
    severity: int
    flags: int = 0
    count: int = 0
    value_a: int = 0
    value_b: int = 0


@dataclass
class RecorderNode:
    node_id: int
    config: ConfigView
    firmware_version: str | None = None
    committed_sample_seq: int = 0
    expected_sample_seq: int = 0
    last_written_seq: int = 0
    samples_written: int = 0
    bursts_ok: int = 0
    bursts_no_data: int = 0
    bursts_failed: int = 0
    gaps_detected: int = 0
    empty_polls: int = 0
    last_stats: Optional[NodeStats] = None
    next_temperature_at: float = 0.0
    online: bool = False
    last_temperature_c: Optional[float] = None
    last_temperature_unix_ns: Optional[int] = None
    last_temperature_window_start: Optional[datetime] = None
    next_window_temperature_retry_at: float = 0.0
    baseline_sensor_loss: int = 0
    baseline_rx_overflow_count: int = 0
    baseline_packet_overwrite_count: int = 0
    instant_samples_per_second_5s: Optional[float] = None
    rate_stability_percent_5s: Optional[float] = None
    rate_history: deque[tuple[float, int]] | None = None
    sample_flow_state: str = "unknown"
    sample_flow_zero_since: Optional[float] = None
    next_diagnostic_event_id: int = 1
    diagnostic_dropped_events: int = 0
    diagnostic_clock_offset_ms: int | None = None
    capabilities: object | None = None
    sample_clock: SampleClockModel = field(default_factory=SampleClockModel)
    node_host_clock: NodeHostClockModel = field(
        default_factory=NodeHostClockModel
    )
    utc_clock: UtcCorrelationModel = field(
        default_factory=UtcCorrelationModel
    )
    next_time_sync_at: float = 0.0
    next_sync_id: int = 1
