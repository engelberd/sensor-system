#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import logging
from logging.handlers import RotatingFileHandler
import time
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Watch supervisor status and emit soak alerts")
    parser.add_argument(
        "--status-file",
        default="/run/sensor-system/supervisor.status.json",
        help="Supervisor JSON status snapshot",
    )
    parser.add_argument(
        "--log-file",
        default="logs/sensor-system/soak_watch.log",
        help="Continuous soak watch log",
    )
    parser.add_argument(
        "--alert-file",
        default="logs/sensor-system/soak_alerts.log",
        help="Alert-only soak log",
    )
    parser.add_argument(
        "--interval",
        type=float,
        default=10.0,
        help="Polling interval in seconds",
    )
    parser.add_argument("--max-log-bytes", type=int, default=5 * 1024 * 1024)
    parser.add_argument("--log-backups", type=int, default=3)
    return parser.parse_args()


def rotating_logger(name: str, path: Path, max_bytes: int, backups: int) -> logging.Logger:
    path.parent.mkdir(parents=True, exist_ok=True)
    logger = logging.getLogger(name)
    logger.setLevel(logging.INFO)
    logger.propagate = False
    handler = RotatingFileHandler(
        path,
        maxBytes=max(0, max_bytes),
        backupCount=max(0, backups),
        encoding="utf-8",
    )
    handler.setFormatter(logging.Formatter("%(message)s"))
    logger.handlers.clear()
    logger.addHandler(handler)
    return logger


def main() -> int:
    args = parse_args()
    status_path = Path(args.status_file)
    log_path = Path(args.log_file)
    alert_path = Path(args.alert_file)

    last_restart: dict[str, int] = {}
    last_running: dict[str, bool] = {}
    last_online: dict[str, bool] = {}

    log = rotating_logger("sensor-system-soak", log_path, args.max_log_bytes, args.log_backups)
    alerts = rotating_logger("sensor-system-soak-alerts", alert_path, args.max_log_bytes, args.log_backups)
    log.info("=== watch started %s ===", time.strftime("%Y-%m-%d %H:%M:%S %z"))
    try:
        while True:
            now = time.strftime("%Y-%m-%d %H:%M:%S %z")
            try:
                status = json.loads(status_path.read_text(encoding="utf-8"))
            except Exception as exc:
                message = f"[{now}] ALERT status read failed: {exc}\n"
                log.info(message.rstrip())
                alerts.info(message.rstrip())
                time.sleep(max(1.0, args.interval / 2.0))
                continue

            summaries: list[str] = []
            for channel in status.get("channels", []):
                name = str(channel["name"])
                node = (channel.get("nodes") or [{}])[0]
                running = bool(channel.get("running"))
                restart_count = int(channel.get("restart_count") or 0)
                online = bool(node.get("online"))
                samples = node.get("samples_written")
                gaps = node.get("gaps_detected")
                no_data = node.get("bursts_no_data")
                temperature = node.get("last_temperature_c")
                summaries.append(
                    f"{name}:run={int(running)} rst={restart_count} on={int(online)} "
                    f"samp={samples} gap={gaps} nodata={no_data} temp={temperature}"
                )

                if name in last_restart and restart_count > last_restart[name]:
                    message = f"[{now}] ALERT {name} restart_count {last_restart[name]} -> {restart_count}\n"
                    log.info(message.rstrip())
                    alerts.info(message.rstrip())
                if name in last_running and running != last_running[name]:
                    message = f"[{now}] ALERT {name} running {last_running[name]} -> {running}\n"
                    log.info(message.rstrip())
                    alerts.info(message.rstrip())
                if name in last_online and online != last_online[name]:
                    message = f"[{now}] ALERT {name} online {last_online[name]} -> {online}\n"
                    log.info(message.rstrip())
                    alerts.info(message.rstrip())

                last_restart[name] = restart_count
                last_running[name] = running
                last_online[name] = online

            log.info("[%s] %s", now, " | ".join(summaries))
            time.sleep(max(1.0, args.interval))
    finally:
        logging.shutdown()


if __name__ == "__main__":
    raise SystemExit(main())
