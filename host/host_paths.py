#!/usr/bin/env python3
"""Show and optionally create the directories used by one host installation."""

from __future__ import annotations

import argparse
from pathlib import Path
import sys

PROJECT_ROOT = Path(__file__).resolve().parents[1]
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from host.common.system_config import HostSystemConfig


def resolve_path(value: str) -> Path:
    path = Path(value).expanduser()
    return path if path.is_absolute() else PROJECT_ROOT / path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", default="host/system_config.json")
    parser.add_argument(
        "--init", action="store_true", help="Create writable data, archive and log directories"
    )
    args = parser.parse_args()

    config_path = resolve_path(args.config).resolve()
    config = HostSystemConfig.load(config_path)
    paths = {
        "config": config_path,
        "recordings": resolve_path(config.storage.root_dir).resolve(),
        "archive": resolve_path(config.storage.archive_dir).resolve(),
        "diagnostics": (PROJECT_ROOT / "var/diagnostics").resolve(),
        "logs": resolve_path(config.supervisor.log_dir).resolve(),
        "temporary": (PROJECT_ROOT / "var/tmp").resolve(),
        "runtime": resolve_path(config.supervisor.channel_runtime_dir).resolve(),
        "supervisor_status": resolve_path(config.supervisor.status_file).resolve(),
        "supervisor_events": resolve_path(config.supervisor.event_log).resolve(),
    }

    if args.init:
        for name in ("recordings", "archive", "diagnostics", "logs", "temporary"):
            paths[name].mkdir(parents=True, exist_ok=True)

    for name, path in paths.items():
        state = "exists" if path.exists() else "missing"
        print(f"{name:18} {path} [{state}]")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
