from __future__ import annotations

import argparse
import json
import os
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Operator console for the Sensor System recorder")
    parser.add_argument("--status-file")
    parser.add_argument("--event-log")
    parser.add_argument("--refresh", type=float, default=1.0)
    parser.add_argument("--tail-events", type=int, default=5)
    parser.add_argument("--once", action="store_true")
    return parser.parse_args()


def load_json(path: Path) -> dict[str, Any] | None:
    if not path.exists():
        return None
    return json.loads(path.read_text(encoding="utf-8"))


def load_tail_jsonl(path: Path, limit: int) -> list[dict[str, Any]]:
    if not path.exists() or limit <= 0:
        return []
    try:
        with path.open("rb") as handle:
            handle.seek(0, os.SEEK_END)
            pos = handle.tell()
            buffer = b""
            lines: list[bytes] = []
            while pos > 0 and len(lines) <= limit:
                read_size = min(4096, pos)
                pos -= read_size
                handle.seek(pos)
                buffer = handle.read(read_size) + buffer
                lines = buffer.splitlines()
    except OSError:
        return []

    tail = lines[-limit:]
    result: list[dict[str, Any]] = []
    for line in tail:
        if not line.strip():
            continue
        try:
            result.append(json.loads(line.decode("utf-8")))
        except json.JSONDecodeError:
            continue
    return result


def format_ns_as_local(ns_value: int | None) -> str:
    if ns_value is None:
        return "-"
    return datetime.fromtimestamp(ns_value / 1_000_000_000, tz=timezone.utc).astimezone().strftime("%Y-%m-%d %H:%M:%S")


def clear_screen() -> None:
    sys.stdout.write("\033[2J\033[H")
    sys.stdout.flush()


def resolve_default_paths(args: argparse.Namespace) -> tuple[Path, Path]:
    if args.status_file:
        status_path = Path(args.status_file)
    else:
        supervisor_path = Path("/tmp/sensor-system_supervisor_status.json")
        recorder_path = Path("/tmp/sensor-system_recorder_status.json")
        status_path = supervisor_path if supervisor_path.exists() else recorder_path

    if args.event_log:
        event_path = Path(args.event_log)
    else:
        supervisor_log = Path("/tmp/sensor-system_supervisor_events.jsonl")
        recorder_log = Path("/tmp/sensor-system_recorder_events.jsonl")
        event_path = supervisor_log if status_path.name.endswith("supervisor_status.json") else recorder_log

    return status_path, event_path


def render_node_line(node: dict[str, Any], prefix: str = "  ") -> str:
    temp = node.get("last_temperature_c")
    temp_text = f"{temp:.2f} C" if isinstance(temp, (int, float)) else "-"
    node_name = node.get("name")
    node_name_text = f" ({node_name})" if node_name else ""
    sensor_loss_session = node.get("sensor_loss_session", 0)
    sensor_loss_total = node.get("sensor_loss_total", 0)
    rx_ovf_session = node.get("rx_overflow_session", 0)
    rx_ovf_total = node.get("rx_overflow_total", 0)
    pkt_ovf_session = node.get("packet_overwrite_session", 0)
    pkt_ovf_total = node.get("packet_overwrite_total", 0)
    return (
        prefix +
        f"node={node.get('node_id')}{node_name_text} "
        f"{'ONLINE ' if node.get('online') else 'OFFLINE'} "
        f"sensor={node.get('sensor_odr_hz')}Hz "
        f"output={node.get('output_odr_hz')}Hz "
        f"written={node.get('samples_written')} "
        f"next={node.get('expected_sample_seq')} "
        f"gaps={node.get('gaps_detected')} "
        f"loss={sensor_loss_session}({sensor_loss_total}) "
        f"rx_ovf={rx_ovf_session}({rx_ovf_total}) "
        f"pkt_ovf={pkt_ovf_session}({pkt_ovf_total}) "
        f"temp={temp_text} "
        f"temp_at={format_ns_as_local(node.get('last_temperature_unix_ns'))}"
    )


def render(status: dict[str, Any] | None, events: list[dict[str, Any]], status_file: Path) -> str:
    lines: list[str] = []
    lines.append("Sensor System Host Console")
    lines.append("")
    if status is None:
        lines.append(f"Waiting for recorder status: {status_file}")
        return "\n".join(lines)

    if "channels" in status:
        lines.append(f"Updated:      {status.get('updated_utc', '-')}")
        lines.append(f"Started:      {status.get('started_utc', '-')}")
        lines.append(f"Supervisor:   {status.get('supervisor_version', '-')}")
        lines.append(f"Storage root: {status.get('storage_root', '-')}")
        lines.append("")
        lines.append("Channels")
        for channel in status.get("channels", []):
            channel_label = channel.get("label")
            channel_title = f"{channel.get('name')}"
            if channel_label:
                channel_title = f"{channel_title} ({channel_label})"
            if not channel.get("enabled", True):
                channel_state = "DISABLED"
            else:
                channel_state = "RUNNING" if channel.get("running") else "STOPPED"
            lines.append(
                "  "
                f"{channel_title} "
                f"{channel_state} "
                f"port={channel.get('port')}@{channel.get('baud')} "
                f"pid={channel.get('process_id', '-')}"
            )
            lines.append(f"    active_file={channel.get('active_file', '-')}")
            lines.append(
                "    "
                f"updated={channel.get('updated_utc', '-')} "
                f"restarts={channel.get('restart_count', 0)} "
                f"last_exit={channel.get('last_exit_code', '-')}"
            )
            for node in channel.get("nodes", []):
                lines.append(render_node_line(node, prefix="    "))
        if events:
            lines.append("")
            lines.append("Recent events")
            for event in events:
                channel_suffix = f" channel={event['channel_name']}" if "channel_name" in event else ""
                node_suffix = f" node={event['node_id']}" if "node_id" in event else ""
                lines.append(
                    f"  {event.get('utc', '-')} [{event.get('severity', '-')}] "
                    f"{event.get('event', '-')}{channel_suffix}{node_suffix}"
                )
        return "\n".join(lines)

    lines.append(f"Updated:     {status.get('updated_utc', '-')}")
    lines.append(f"Started:     {status.get('started_utc', '-')}")
    lines.append(f"Recorder:    {status.get('recorder_version', '-')}")
    lines.append(f"Port:        {status.get('port', '-')} @ {status.get('baud', '-')}")
    lines.append(f"Destination: {status.get('destination', '-')}")
    lines.append(f"Active file: {status.get('active_file', '-')}")
    lines.append("")
    lines.append("Nodes")
    for node in status.get("nodes", []):
        lines.append(render_node_line(node))
    if events:
        lines.append("")
        lines.append("Recent events")
        for event in events:
            node_suffix = f" node={event['node_id']}" if "node_id" in event else ""
            lines.append(
                f"  {event.get('utc', '-')} [{event.get('severity', '-')}] "
                f"{event.get('event', '-')}{node_suffix}"
            )
    return "\n".join(lines)


def main() -> int:
    args = parse_args()
    status_path, event_path = resolve_default_paths(args)

    while True:
        status = load_json(status_path)
        events = load_tail_jsonl(event_path, args.tail_events)
        if not args.once:
            clear_screen()
        print(render(status, events, status_path))
        if args.once:
            return 0
        time.sleep(max(0.1, args.refresh))


if __name__ == "__main__":
    raise SystemExit(main())
