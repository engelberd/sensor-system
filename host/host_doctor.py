#!/usr/bin/env python3
"""Read-only health checks for one sensor-system host installation."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import stat
import sys
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

PROJECT_ROOT = Path(__file__).resolve().parents[1]
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from host.common.system_config import HostSystemConfig
from host.common.version import PROJECT_VERSION


@dataclass(frozen=True)
class Finding:
    severity: str
    code: str
    message: str


def resolve_path(value: str, project_root: Path) -> Path:
    path = Path(value).expanduser()
    return path if path.is_absolute() else project_root / path


def read_json(path: Path) -> dict[str, Any] | None:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    return payload if isinstance(payload, dict) else None


def is_fresh(path: Path, max_age_s: float, now: float) -> bool:
    try:
        return now - path.stat().st_mtime <= max_age_s
    except OSError:
        return False


def nearest_existing(path: Path) -> Path | None:
    candidate = path
    while not candidate.exists() and candidate != candidate.parent:
        candidate = candidate.parent
    return candidate if candidate.exists() else None


def run_checks(
    config_path: Path,
    *,
    project_root: Path = PROJECT_ROOT,
    min_free_gb: float = 5.0,
    status_max_age_s: float = 15.0,
    now: float | None = None,
) -> list[Finding]:
    findings: list[Finding] = []
    current_time = time.time() if now is None else now

    try:
        config = HostSystemConfig.load(config_path)
    except Exception as exc:  # noqa: BLE001 - doctor must report malformed config
        return [Finding("ERROR", "config.invalid", f"{config_path}: {exc}")]

    findings.append(
        Finding(
            "OK",
            "config.loaded",
            f"{config.system.name}: {len(config.channels)} channels from {config_path}",
        )
    )

    enabled_channels = [channel for channel in config.channels if channel.enabled]
    names = [channel.name for channel in config.channels]
    if len(names) != len(set(names)):
        findings.append(Finding("ERROR", "config.duplicate-channel", "duplicate channel names"))

    enabled_ports = [channel.port for channel in enabled_channels]
    if len(enabled_ports) != len(set(enabled_ports)):
        findings.append(
            Finding("ERROR", "config.duplicate-port", "multiple enabled channels use one port")
        )

    for channel in config.channels:
        port = Path(channel.port)
        severity = "ERROR" if channel.enabled else "WARN"
        if not port.exists():
            findings.append(
                Finding(severity, "port.missing", f"{channel.name}: {channel.port} does not exist")
            )
        else:
            try:
                is_device = stat.S_ISCHR(port.stat().st_mode)
            except OSError:
                is_device = False
            if not is_device:
                findings.append(
                    Finding(severity, "port.not-device", f"{channel.name}: {channel.port} is not a character device")
                )
            elif not os.access(port, os.R_OK | os.W_OK):
                findings.append(
                    Finding(severity, "port.permissions", f"{channel.name}: no read/write access to {channel.port}")
                )
            else:
                findings.append(Finding("OK", "port.ready", f"{channel.name}: {channel.port}"))

        for node in channel.nodes:
            if node.board_revision not in {1, 2}:
                findings.append(
                    Finding(
                        "ERROR",
                        "node.board-revision",
                        f"{channel.name}/node-{node.node_id}: board_revision is not set",
                    )
                )

    if any(channel.port.startswith("/dev/ttyCH9344") for channel in config.channels):
        if Path("/sys/module/ch9344").exists():
            findings.append(Finding("OK", "driver.ch9344", "CH9344 kernel module is loaded"))
        else:
            findings.append(
                Finding("ERROR", "driver.ch9344", "CH9344 ports configured but kernel module is not loaded")
            )

    storage_paths = {
        "recordings": resolve_path(config.storage.root_dir, project_root),
        "archive": resolve_path(config.storage.archive_dir, project_root),
        "logs": resolve_path(config.supervisor.log_dir, project_root),
    }
    for label, path in storage_paths.items():
        if not path.exists():
            findings.append(Finding("WARN", "path.missing", f"{label}: {path}"))
            continue
        if not path.is_dir():
            findings.append(Finding("ERROR", "path.not-directory", f"{label}: {path}"))
            continue
        if not os.access(path, os.W_OK | os.X_OK):
            findings.append(Finding("ERROR", "path.not-writable", f"{label}: {path}"))
        else:
            findings.append(Finding("OK", "path.ready", f"{label}: {path}"))

    recordings = storage_paths["recordings"]
    disk_path = nearest_existing(recordings)
    if disk_path is not None:
        free_gb = shutil.disk_usage(disk_path).free / (1024 ** 3)
        severity = "OK" if free_gb >= min_free_gb else "ERROR"
        findings.append(
            Finding(severity, "storage.free", f"{free_gb:.1f} GiB free at {disk_path}")
        )

    supervisor_status_path = resolve_path(config.supervisor.status_file, project_root)
    supervisor_status = read_json(supervisor_status_path)
    supervisor_channels: dict[str, dict[str, Any]] = {}
    if supervisor_status is None:
        findings.append(
            Finding("ERROR", "supervisor.status", f"cannot read {supervisor_status_path}")
        )
    elif not is_fresh(supervisor_status_path, status_max_age_s, current_time):
        findings.append(
            Finding("ERROR", "supervisor.stale", f"stale status: {supervisor_status_path}")
        )
    else:
        findings.append(Finding("OK", "supervisor.fresh", f"fresh status: {supervisor_status_path}"))
        supervisor_channels = {
            str(item.get("name")): item
            for item in supervisor_status.get("channels", [])
            if isinstance(item, dict) and item.get("name") is not None
        }

    runtime_dir = resolve_path(config.supervisor.channel_runtime_dir, project_root)
    for channel in config.channels:
        channel_errors_before = sum(item.severity == "ERROR" for item in findings)
        supervisor_channel = supervisor_channels.get(channel.name)
        if not channel.enabled:
            findings.append(
                Finding(
                    "WARN",
                    "channel.disabled",
                    f"{channel.name}: disabled; device flow is not monitored",
                )
            )
        if channel.enabled and (
            supervisor_channel is None or not bool(supervisor_channel.get("running", False))
        ):
            findings.append(
                Finding("ERROR", "channel.not-running", f"{channel.name}: enabled but not running")
            )
        elif not channel.enabled and supervisor_channel is not None and bool(
            supervisor_channel.get("running", False)
        ):
            findings.append(
                Finding("ERROR", "channel.unexpected-running", f"{channel.name}: disabled but running")
            )

        worker_path = runtime_dir / f"{channel.name}.status.json"
        worker = read_json(worker_path)
        if not channel.enabled:
            continue
        if worker is None or not is_fresh(worker_path, status_max_age_s, current_time):
            findings.append(
                Finding("ERROR", "worker.stale", f"{channel.name}: missing or stale worker status")
            )
            continue
        runtime_nodes = {
            int(item.get("node_id", -1)): item
            for item in worker.get("nodes", [])
            if isinstance(item, dict)
        }
        for node in channel.nodes:
            runtime_node = runtime_nodes.get(node.node_id)
            prefix = f"{channel.name}/node-{node.node_id}"
            if runtime_node is None:
                findings.append(Finding("ERROR", "node.missing", f"{prefix}: absent from worker status"))
                continue
            if not bool(runtime_node.get("online", False)):
                findings.append(Finding("ERROR", "node.offline", f"{prefix}: offline"))
            flow = str(runtime_node.get("sample_flow_state", "unknown"))
            if flow != "flowing":
                findings.append(Finding("ERROR", "node.flow", f"{prefix}: flow={flow}"))
            firmware = runtime_node.get("firmware_version")
            if firmware not in {PROJECT_VERSION, f"v{PROJECT_VERSION}"}:
                findings.append(
                    Finding("WARN", "node.firmware", f"{prefix}: firmware={firmware}, host={PROJECT_VERSION}")
                )
            reported_revision = runtime_node.get("board_revision")
            if reported_revision != node.board_revision:
                findings.append(
                    Finding(
                        "ERROR",
                        "node.revision-mismatch",
                        f"{prefix}: reported={reported_revision}, configured={node.board_revision}",
                    )
                )
            if node.sensor_odr_hz is not None and runtime_node.get("sensor_odr_hz") != node.sensor_odr_hz:
                findings.append(
                    Finding(
                        "ERROR",
                        "node.odr-mismatch",
                        f"{prefix}: sensor_odr={runtime_node.get('sensor_odr_hz')}, configured={node.sensor_odr_hz}",
                    )
                )
        channel_errors_after = sum(item.severity == "ERROR" for item in findings)
        if channel_errors_after == channel_errors_before:
            findings.append(Finding("OK", "channel.healthy", f"{channel.name}: runtime status is healthy"))

    return findings


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", default="host/system_config.json")
    parser.add_argument("--json", action="store_true", help="Emit machine-readable JSON")
    parser.add_argument("--strict", action="store_true", help="Return non-zero for warnings")
    parser.add_argument("--min-free-gb", type=float, default=5.0)
    parser.add_argument("--status-max-age", type=float, default=15.0)
    args = parser.parse_args()

    config_path = resolve_path(args.config, PROJECT_ROOT).resolve()
    findings = run_checks(
        config_path,
        min_free_gb=max(0.0, args.min_free_gb),
        status_max_age_s=max(1.0, args.status_max_age),
    )
    if args.json:
        print(json.dumps([asdict(item) for item in findings], indent=2, sort_keys=True))
    else:
        for finding in findings:
            print(f"[{finding.severity:5}] {finding.code:26} {finding.message}")
        totals = {severity: sum(item.severity == severity for item in findings) for severity in ("OK", "WARN", "ERROR")}
        print(f"Summary: {totals['OK']} ok, {totals['WARN']} warnings, {totals['ERROR']} errors")

    if any(item.severity == "ERROR" for item in findings):
        return 2
    if args.strict and any(item.severity == "WARN" for item in findings):
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
