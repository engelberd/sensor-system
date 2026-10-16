from __future__ import annotations

from dataclasses import asdict, dataclass
from pathlib import Path
from threading import Lock
from typing import Any


@dataclass(frozen=True)
class HostMetrics:
    cpu_used_percent: float | None = None
    memory_total_bytes: int | None = None
    memory_available_bytes: int | None = None
    memory_used_bytes: int | None = None
    memory_used_percent: float | None = None

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


class HostMetricsSampler:
    """Read lightweight host-wide CPU and memory usage from Linux procfs."""

    def __init__(self, proc_root: str | Path = "/proc") -> None:
        self.proc_root = Path(proc_root)
        self._lock = Lock()
        self._previous_cpu = self._read_cpu_times()

    def sample(self) -> HostMetrics:
        with self._lock:
            cpu_used_percent = self._sample_cpu_percent()
            memory = self._read_memory()
        return HostMetrics(cpu_used_percent=cpu_used_percent, **memory)

    def _sample_cpu_percent(self) -> float | None:
        current = self._read_cpu_times()
        previous = self._previous_cpu
        self._previous_cpu = current
        if current is None or previous is None:
            return None
        total_delta = current[0] - previous[0]
        idle_delta = current[1] - previous[1]
        if total_delta <= 0 or idle_delta < 0:
            return None
        used_percent = 100.0 * (total_delta - idle_delta) / total_delta
        return round(max(0.0, min(100.0, used_percent)), 1)

    def _read_cpu_times(self) -> tuple[int, int] | None:
        try:
            first_line = (self.proc_root / "stat").read_text(encoding="utf-8").splitlines()[0]
            fields = first_line.split()
            if not fields or fields[0] != "cpu" or len(fields) < 5:
                return None
            values = [int(value) for value in fields[1:]]
        except (OSError, ValueError, IndexError):
            return None
        # guest and guest_nice are already included in user and nice.
        total = sum(values[:8])
        idle = values[3] + (values[4] if len(values) > 4 else 0)
        return total, idle

    def _read_memory(self) -> dict[str, int | float | None]:
        try:
            values_kib: dict[str, int] = {}
            for line in (self.proc_root / "meminfo").read_text(encoding="utf-8").splitlines():
                parts = line.split()
                if len(parts) >= 2 and parts[1].isdigit():
                    values_kib[parts[0].rstrip(":")] = int(parts[1])
            total_bytes = values_kib["MemTotal"] * 1024
            available_bytes = values_kib["MemAvailable"] * 1024
        except (OSError, KeyError, ValueError):
            return self._unavailable_memory()
        if total_bytes <= 0:
            return self._unavailable_memory()
        available_bytes = max(0, min(total_bytes, available_bytes))
        used_bytes = total_bytes - available_bytes
        return {
            "memory_total_bytes": total_bytes,
            "memory_available_bytes": available_bytes,
            "memory_used_bytes": used_bytes,
            "memory_used_percent": round(100.0 * used_bytes / total_bytes, 1),
        }

    @staticmethod
    def _unavailable_memory() -> dict[str, None]:
        return {
            "memory_total_bytes": None,
            "memory_available_bytes": None,
            "memory_used_bytes": None,
            "memory_used_percent": None,
        }
