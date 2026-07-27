from __future__ import annotations

from collections import deque
from dataclasses import dataclass
from enum import Enum
import math
from typing import Iterable


class ClockState(str, Enum):
    UNSYNCED = "UNSYNCED"
    LOCKED = "LOCKED"
    HOLDOVER = "HOLDOVER"
    INVALID = "INVALID"


@dataclass(frozen=True)
class AffineEstimate:
    slope: float
    intercept: float
    max_residual: float
    uncertainty: float
    observation_count: int

    def predict(self, x: float) -> float:
        return self.slope * x + self.intercept


@dataclass(frozen=True)
class WeightedPoint:
    x: float
    y: float
    uncertainty: float


def fit_affine(points: Iterable[WeightedPoint]) -> AffineEstimate:
    values = tuple(points)
    if len(values) < 2:
        raise ValueError("at least two points are required")
    if any(
        not math.isfinite(point.x)
        or not math.isfinite(point.y)
        or not math.isfinite(point.uncertainty)
        or point.uncertainty < 0
        for point in values
    ):
        raise ValueError("clock observations must be finite")

    x_origin = values[0].x
    y_origin = values[0].y
    weights = tuple(
        1.0 / max(point.uncertainty, 1.0) ** 2
        for point in values
    )
    total_weight = sum(weights)
    mean_x = sum(
        weight * (point.x - x_origin)
        for point, weight in zip(values, weights)
    ) / total_weight
    mean_y = sum(
        weight * (point.y - y_origin)
        for point, weight in zip(values, weights)
    ) / total_weight
    denominator = sum(
        weight * ((point.x - x_origin) - mean_x) ** 2
        for point, weight in zip(values, weights)
    )
    if denominator <= 0:
        raise ValueError("clock observations have no x span")
    numerator = sum(
        weight
        * ((point.x - x_origin) - mean_x)
        * ((point.y - y_origin) - mean_y)
        for point, weight in zip(values, weights)
    )
    slope = numerator / denominator
    intercept = (
        y_origin + mean_y - slope * (x_origin + mean_x)
    )
    residuals = tuple(
        abs(point.y - (slope * point.x + intercept))
        for point in values
    )
    max_residual = max(residuals)
    uncertainty = max(
        max_residual,
        max(point.uncertainty for point in values),
    )
    return AffineEstimate(
        slope=slope,
        intercept=intercept,
        max_residual=max_residual,
        uncertainty=uncertainty,
        observation_count=len(values),
    )


class SampleClockModel:
    def __init__(
        self,
        *,
        max_points: int = 128,
        min_lock_points: int = 6,
        max_holdover_samples: int = 4096,
    ) -> None:
        self.max_points = max_points
        self.min_lock_points = min_lock_points
        self.max_holdover_samples = max_holdover_samples
        self.node_id: int | None = None
        self.boot_epoch: int | None = None
        self.timing_segment_id: int | None = None
        self.points: deque[WeightedPoint] = deque(maxlen=max_points)
        self.estimate: AffineEstimate | None = None
        self.state = ClockState.UNSYNCED
        self.last_sample_seq: int | None = None

    def reset(
        self,
        *,
        node_id: int,
        boot_epoch: int,
        timing_segment_id: int,
    ) -> None:
        self.node_id = node_id
        self.boot_epoch = boot_epoch
        self.timing_segment_id = timing_segment_id
        self.points.clear()
        self.estimate = None
        self.state = ClockState.UNSYNCED
        self.last_sample_seq = None

    def add_anchor(
        self,
        *,
        node_id: int,
        boot_epoch: int,
        timing_segment_id: int,
        sample_seq: int,
        device_time_us: int,
        uncertainty_us: float,
    ) -> bool:
        if boot_epoch == 0 or device_time_us <= 0 or uncertainty_us < 0:
            self.state = ClockState.INVALID
            return False
        identity = (node_id, boot_epoch, timing_segment_id)
        current = (self.node_id, self.boot_epoch, self.timing_segment_id)
        if self.node_id is None:
            self.reset(
                node_id=node_id,
                boot_epoch=boot_epoch,
                timing_segment_id=timing_segment_id,
            )
        elif identity != current:
            self.reset(
                node_id=node_id,
                boot_epoch=boot_epoch,
                timing_segment_id=timing_segment_id,
            )
        if (
            self.last_sample_seq is not None
            and sample_seq <= self.last_sample_seq
        ):
            known = next(
                (
                    point
                    for point in reversed(self.points)
                    if int(point.x) == sample_seq
                ),
                None,
            )
            identical = (
                known is not None
                and int(known.y) == device_time_us
            )
            if not identical:
                self.state = ClockState.INVALID
            return identical
        if self.points and device_time_us <= self.points[-1].y:
            self.state = ClockState.INVALID
            return False

        self.points.append(
            WeightedPoint(
                x=float(sample_seq),
                y=float(device_time_us),
                uncertainty=max(float(uncertainty_us), 1.0),
            )
        )
        self.last_sample_seq = sample_seq
        if len(self.points) >= 2:
            self.estimate = fit_affine(self.points)
        self.state = (
            ClockState.LOCKED
            if len(self.points) >= self.min_lock_points
            else ClockState.UNSYNCED
        )
        return True

    def predict_device_time_us(
        self,
        sample_seq: int,
    ) -> tuple[int, float, ClockState]:
        if self.estimate is None or self.last_sample_seq is None:
            raise RuntimeError("sample clock is not estimable")
        distance = max(0, sample_seq - self.last_sample_seq)
        state = self.state
        if distance > 0:
            state = (
                ClockState.HOLDOVER
                if distance <= self.max_holdover_samples
                else ClockState.INVALID
            )
        uncertainty = (
            self.estimate.uncertainty
            + abs(distance) * self.estimate.max_residual
        )
        return (
            round(self.estimate.predict(float(sample_seq))),
            uncertainty,
            state,
        )


@dataclass(frozen=True)
class TimeSyncObservation:
    boot_epoch: int
    sync_id: int
    t1_host_monotonic_ns: int
    t2_node_rx_us: int
    t3_node_tx_us: int
    t4_host_monotonic_ns: int

    @property
    def node_mid_us(self) -> float:
        return (self.t2_node_rx_us + self.t3_node_tx_us) / 2.0

    @property
    def host_mid_ns(self) -> float:
        return (
            self.t1_host_monotonic_ns + self.t4_host_monotonic_ns
        ) / 2.0

    @property
    def network_rtt_ns(self) -> int:
        return (
            self.t4_host_monotonic_ns
            - self.t1_host_monotonic_ns
            - (self.t3_node_tx_us - self.t2_node_rx_us) * 1000
        )


class NodeHostClockModel:
    def __init__(
        self,
        *,
        max_observations: int = 64,
        min_lock_observations: int = 5,
        max_holdover_ns: int = 120_000_000_000,
    ) -> None:
        self.observations: deque[TimeSyncObservation] = deque(
            maxlen=max_observations
        )
        self.min_lock_observations = min_lock_observations
        self.max_holdover_ns = max_holdover_ns
        self.boot_epoch: int | None = None
        self.estimate: AffineEstimate | None = None
        self.state = ClockState.UNSYNCED
        self.last_host_monotonic_ns: int | None = None

    def add(self, observation: TimeSyncObservation) -> bool:
        if (
            observation.boot_epoch == 0
            or observation.t3_node_tx_us < observation.t2_node_rx_us
            or observation.t4_host_monotonic_ns
            <= observation.t1_host_monotonic_ns
            or observation.network_rtt_ns < 0
        ):
            self.state = ClockState.INVALID
            return False
        if (
            self.boot_epoch is not None
            and observation.boot_epoch != self.boot_epoch
        ):
            self.observations.clear()
            self.estimate = None
            self.state = ClockState.UNSYNCED
        self.boot_epoch = observation.boot_epoch
        self.observations.append(observation)
        self.last_host_monotonic_ns = observation.t4_host_monotonic_ns

        ordered = sorted(
            self.observations,
            key=lambda item: item.network_rtt_ns,
        )
        keep = max(
            min(len(ordered), self.min_lock_observations),
            (len(ordered) + 1) // 2,
        )
        candidates = ordered[:keep]
        if len(candidates) >= 2:
            self.estimate = fit_affine(
                WeightedPoint(
                    x=item.node_mid_us,
                    y=item.host_mid_ns,
                    uncertainty=max(item.network_rtt_ns / 2.0, 1.0),
                )
                for item in candidates
            )
        self.state = (
            ClockState.LOCKED
            if len(self.observations) >= self.min_lock_observations
            else ClockState.UNSYNCED
        )
        return True

    def predict_host_monotonic_ns(
        self,
        device_time_us: int,
        *,
        now_monotonic_ns: int,
    ) -> tuple[int, float, ClockState]:
        if self.estimate is None or self.last_host_monotonic_ns is None:
            raise RuntimeError("node/host clock is not estimable")
        age_ns = max(0, now_monotonic_ns - self.last_host_monotonic_ns)
        state = self.state
        if age_ns > 0 and state == ClockState.LOCKED:
            state = (
                ClockState.HOLDOVER
                if age_ns <= self.max_holdover_ns
                else ClockState.INVALID
            )
        uncertainty = self.estimate.uncertainty
        if age_ns > 0:
            uncertainty += (
                age_ns / 1_000_000_000.0
            ) * max(self.estimate.max_residual, 1.0)
        return (
            round(self.estimate.predict(float(device_time_us))),
            uncertainty,
            state,
        )


class UtcCorrelationModel:
    def __init__(self, *, step_threshold_ns: int = 100_000_000) -> None:
        self.step_threshold_ns = step_threshold_ns
        self.offset_ns: int | None = None
        self.uncertainty_ns: int = 0
        self.segment_id = 0
        self.state = ClockState.UNSYNCED

    def observe(
        self,
        *,
        monotonic_before_ns: int,
        utc_ns: int,
        monotonic_after_ns: int,
    ) -> None:
        if monotonic_after_ns < monotonic_before_ns:
            self.state = ClockState.INVALID
            return
        monotonic_mid = (
            monotonic_before_ns + monotonic_after_ns
        ) // 2
        candidate_offset = utc_ns - monotonic_mid
        uncertainty = (
            monotonic_after_ns - monotonic_before_ns
        ) // 2
        if (
            self.offset_ns is not None
            and abs(candidate_offset - self.offset_ns)
            > self.step_threshold_ns
        ):
            self.segment_id += 1
        self.offset_ns = candidate_offset
        self.uncertainty_ns = uncertainty
        self.state = ClockState.LOCKED

    def to_utc_ns(
        self,
        host_monotonic_ns: int,
    ) -> tuple[int, int, ClockState]:
        if self.offset_ns is None:
            raise RuntimeError("UTC correlation is not available")
        return (
            host_monotonic_ns + self.offset_ns,
            self.uncertainty_ns,
            self.state,
        )
