"""Versioned domain contracts shared by Capture and Archive components."""

from __future__ import annotations

from dataclasses import dataclass
from enum import IntEnum, IntFlag, StrEnum


CAPTURE_SCHEMA_MAJOR = 1
CAPTURE_SCHEMA_MINOR = 0
ARCHIVE_SCHEMA_MAJOR = 1
ARCHIVE_SCHEMA_MINOR = 0


class DataProduct(StrEnum):
    CAPTURE = "sensor-system-capture"
    ARCHIVE = "sensor-system-archive"


class CaptureRoutingFlag(IntFlag):
    NONE = 0
    UTC_VALID = 1 << 0
    BOUNDARY_UNCERTAIN = 1 << 1
    INITIALLY_UNSYNCED = 1 << 2
    LATE_ARRIVAL = 1 << 3
    RECOVERED_AFTER_RESTART = 1 << 4


class ArchiveQualityFlag(IntFlag):
    GOOD = 0
    TIME_UNCERTAIN = 1 << 0
    TIME_UNSYNCED = 1 << 1
    BOUNDARY_UNCERTAIN = 1 << 2
    SENSOR_LOSS_ADJACENT = 1 << 3
    SENSOR_RECOVERY = 1 << 4
    CONFIG_CHANGED = 1 << 5
    CALIBRATION_UNKNOWN = 1 << 6
    MEASUREMENT_INVALID = 1 << 7


class GapKind(IntEnum):
    UNKNOWN = 0
    SEQUENCE = 1
    SENSOR_FIFO = 2
    TRANSPORT = 3
    HOST_DISCONTINUITY = 4


class QualityEventCode(IntEnum):
    UNKNOWN = 0
    LOGICAL_GAP = 1
    SENSOR_FIFO_LOSS = 2
    TIMING_INVALIDATION = 3
    SENSOR_RECOVERY = 4
    CONFIG_CHANGED = 5
    CALIBRATION_CHANGED = 6
    HOST_RESTART = 7


class EventSeverity(IntEnum):
    INFO = 0
    WARNING = 1
    ERROR = 2
    CRITICAL = 3


class DecimationFilterProfile(IntEnum):
    LIGHT = 0
    BALANCED = 1
    AGGRESSIVE = 2
    UNKNOWN = 255


class TimeControlMethod(IntEnum):
    UNKNOWN = 0
    DRDY_DEVICE_CLOCK = 1
    INTERPOLATED = 2
    HOST_RECEIVE_TIME = 3


@dataclass(frozen=True)
class SensorIdentity:
    channel_id: int
    sensor_label: str
    node_address: int
    sensor_id: str | None = None
    hardware_id: str | None = None
    board_revision: int | None = None

    def __post_init__(self) -> None:
        if not 1 <= self.channel_id <= 8:
            raise ValueError("channel_id must be in range 1..8")
        if self.sensor_label not in tuple("ABCDEFGH"):
            raise ValueError("sensor_label must be in range A..H")
        if not 0 <= self.node_address <= 255:
            raise ValueError("node_address must be in range 0..255")
        if self.sensor_id is not None and not self.sensor_id.strip():
            raise ValueError("sensor_id must be non-empty when provided")
        if self.hardware_id is not None and not self.hardware_id.strip():
            raise ValueError("hardware_id must be non-empty when provided")
        if self.board_revision not in {None, 1, 2}:
            raise ValueError("board_revision must be 1 or 2 when provided")

    @classmethod
    def temporary(cls, channel_id: int, node_address: int) -> "SensorIdentity":
        if not 1 <= channel_id <= 8:
            raise ValueError("channel_id must be in range 1..8")
        return cls(
            channel_id=channel_id,
            sensor_label=chr(ord("A") + channel_id - 1),
            node_address=node_address,
        )
