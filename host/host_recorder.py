#!/usr/bin/env python3
from __future__ import annotations

import argparse
from collections import deque
import math
import os
import shutil
import socket
import signal
import struct
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Iterable, Optional
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

import serial

try:
    from host_lab import (
        BURST_HEADER_FORMAT,
        CAPABILITY_TIME_SYNC_V1,
        CMD_GRANT_BURST_READ,
        HOST_NODE_ID,
        STATUS_NO_DATA,
        STATUS_OK,
        ConfigView,
        NodeStats,
        ProtocolClient,
        build_buffer_state_payload,
        build_commit_payload,
        build_get_capabilities_payload,
        build_get_config_payload,
        build_get_temperature_payload,
        build_grant_burst_payload,
        build_stats_payload,
        build_time_sync_payload,
        effective_output_odr_hz,
        parse_buffer_state,
        parse_commit_response,
        parse_capabilities,
        parse_config_view,
        parse_grant_burst_response,
        parse_stats,
        parse_time_sync_response,
        parse_temperature_view,
    )
except ModuleNotFoundError:
    from host.host_lab import (
        BURST_HEADER_FORMAT,
        CAPABILITY_TIME_SYNC_V1,
        CMD_GRANT_BURST_READ,
        HOST_NODE_ID,
        STATUS_NO_DATA,
        STATUS_OK,
        ConfigView,
        NodeStats,
        ProtocolClient,
        build_buffer_state_payload,
        build_commit_payload,
        build_get_capabilities_payload,
        build_get_config_payload,
        build_get_temperature_payload,
        build_grant_burst_payload,
        build_stats_payload,
        build_time_sync_payload,
        effective_output_odr_hz,
        parse_buffer_state,
        parse_commit_response,
        parse_capabilities,
        parse_config_view,
        parse_grant_burst_response,
        parse_stats,
        parse_time_sync_response,
        parse_temperature_view,
    )
from host.common.runtime_status import (
    JsonStatusWriter,
    JsonlEventWriter,
    RuntimeStatusNode,
    RuntimeStatusSnapshot,
)
from host.common.instance_lock import InstanceLock
from host.common.clock_models import (
    ClockState,
    TimeSyncObservation,
)
from host.host_configurator import (
    CMD_GET_DIAGNOSTIC_INFO,
    CMD_CLEAR_DIAGNOSTIC_EVENTS,
    CMD_READ_DIAGNOSTIC_EVENTS,
    READ_DIAGNOSTIC_EVENTS_REQUEST_FORMAT,
    format_diagnostic_detail,
    format_diagnostic_event_code,
    format_diagnostic_severity,
    parse_diagnostic_events,
    parse_diagnostic_info_view,
)
from host.live_web import LiveBuffer, LiveGap, LiveSample, LiveServer
from host.common.version import PROJECT_VERSION
from host.recorder.capture_writers import CAPTURE_SCHEMA_VERSION
from host.recorder.capture_windowing import CaptureV1WindowedWriter
from host.recorder.contracts import (
    EventSeverity,
    QualityEventCode,
    SensorIdentity,
)
from host.recorder.decoder import (
    SAMPLE_PAYLOAD_OFFSET,
    decode_i24_be,
    decode_packet_samples,
    raw_lsb_to_m_s2,
)
from host.recorder.model import (
    QualityEventRecord,
    RecorderNode,
    SampleRecord,
    TemperatureRecord,
)
from host.recorder.ports import BaseWriter, StorageError
from host.recorder.windowing import (
    AcquisitionWindowedWriter,
    WindowedWriter,
    make_single_writer as make_window_file_writer,
)


RECORDER_SCHEMA_VERSION = CAPTURE_SCHEMA_VERSION
RECORDER_VERSION = PROJECT_VERSION
SAMPLE_FLOW_STALL_CONFIRM_S = 2.0
DEFAULT_WINDOW_SECONDS = 600
DEFAULT_MIN_FREE_BYTES = 1024 * 1024 * 1024
GET_VERSION_FORMAT = "<BBBBBB"
CMD_GET_VERSION = 0x04


class StopFlag:
    def __init__(self) -> None:
        self.stop_requested = False
        self.signal_number: int | None = None
        self.signal_name: str | None = None

    def request_stop(self, signum: int, *_args: object) -> None:
        self.stop_requested = True
        self.signal_number = signum
        try:
            self.signal_name = signal.Signals(signum).name
        except ValueError:
            self.signal_name = str(signum)



def parse_node_list(value: str) -> list[int]:
    nodes = [int(part.strip()) for part in value.split(",") if part.strip()]
    if not nodes:
        raise argparse.ArgumentTypeError("node list must not be empty")
    return nodes


def resolve_window_timezone(value: str):
    normalized = value.strip()
    if not normalized:
        raise ValueError("window timezone must not be empty")
    if normalized.lower() == "local":
        return datetime.now().astimezone().tzinfo or timezone.utc
    if normalized.upper() == "UTC":
        return timezone.utc
    try:
        return ZoneInfo(normalized)
    except ZoneInfoNotFoundError as exc:
        raise ValueError(
            f"unknown window timezone '{value}'; use 'local', 'UTC', or an IANA name like Europe/Warsaw"
        ) from exc


def send_and_parse_config(client: ProtocolClient, node_id: int, timeout_s: float) -> ConfigView:
    sequence = client.send_command(node_id, build_get_config_payload())
    response = client.wait_for_response(node_id, sequence, timeout_s)
    if response is None:
        raise RuntimeError(f"node {node_id}: no GetConfig response")
    return parse_config_view(response.payload)


def send_and_parse_version(client: ProtocolClient, node_id: int, timeout_s: float) -> str:
    sequence = client.send_command(node_id, bytes([CMD_GET_VERSION]))
    response = client.wait_for_response(node_id, sequence, timeout_s)
    if response is None:
        raise RuntimeError(f"node {node_id}: no GetVersion response")
    try:
        command, status, fw_major, fw_minor, fw_patch, _protocol_version = struct.unpack(
            GET_VERSION_FORMAT,
            response.payload[: struct.calcsize(GET_VERSION_FORMAT)],
        )
    except struct.error as exc:
        raise RuntimeError(f"node {node_id}: malformed GetVersion response") from exc
    if command != CMD_GET_VERSION:
        raise RuntimeError(f"node {node_id}: unexpected GetVersion command echo={command}")
    if status != STATUS_OK:
        raise RuntimeError(f"node {node_id}: GetVersion failed with status={status}")
    return f"v{fw_major}.{fw_minor}.{fw_patch}"


def send_and_parse_buffer_state(client: ProtocolClient, node_id: int, timeout_s: float):
    sequence = client.send_command(node_id, build_buffer_state_payload())
    response = client.wait_for_response(node_id, sequence, timeout_s)
    if response is None:
        raise RuntimeError(f"node {node_id}: no GetBufferState response")
    return parse_buffer_state(response.payload)


def send_and_parse_temperature(client: ProtocolClient, node_id: int, timeout_s: float):
    sequence = client.send_command(node_id, build_get_temperature_payload())
    response = client.wait_for_response(node_id, sequence, timeout_s)
    if response is None:
        raise RuntimeError(f"node {node_id}: no GetTemperature response")
    return parse_temperature_view(response.payload)


def send_commit(client: ProtocolClient, node_id: int, last_sample_seq: int, timeout_s: float) -> int:
    sequence = client.send_command(node_id, build_commit_payload(last_sample_seq))
    response = client.wait_for_response(node_id, sequence, timeout_s)
    if response is None:
        raise RuntimeError(f"node {node_id}: no CommitReadUpTo response")

    _, status, committed_sample_seq = parse_commit_response(response.payload)
    if status != STATUS_OK:
        raise RuntimeError(f"node {node_id}: CommitReadUpTo failed with status={status}")
    return committed_sample_seq


def send_and_parse_capabilities(
    client: ProtocolClient,
    node_id: int,
    timeout_s: float,
):
    sequence = client.send_command(
        node_id,
        build_get_capabilities_payload(),
    )
    response = client.wait_for_response(node_id, sequence, timeout_s)
    if response is None:
        return None
    try:
        capabilities = parse_capabilities(response.payload)
    except (ValueError, struct.error):
        return None
    if capabilities.status != STATUS_OK:
        return None
    return capabilities


def refresh_time_sync(
    client: ProtocolClient,
    node: RecorderNode,
    timeout_s: float,
) -> TimeSyncObservation:
    sync_id = node.next_sync_id
    node.next_sync_id = (node.next_sync_id + 1) & 0xFFFFFFFF
    if node.next_sync_id == 0:
        node.next_sync_id = 1

    monotonic_before_ns = time.monotonic_ns()
    utc_ns = time.time_ns()
    monotonic_after_ns = time.monotonic_ns()
    node.utc_clock.observe(
        monotonic_before_ns=monotonic_before_ns,
        utc_ns=utc_ns,
        monotonic_after_ns=monotonic_after_ns,
    )

    t1_host_monotonic_ns = time.monotonic_ns()
    sequence = client.send_command(
        node.node_id,
        build_time_sync_payload(sync_id, t1_host_monotonic_ns),
    )
    response_frame = client.wait_for_response(
        node.node_id,
        sequence,
        timeout_s,
    )
    t4_host_monotonic_ns = time.monotonic_ns()
    if response_frame is None:
        raise RuntimeError(f"node {node.node_id}: no TimeSync response")
    response = parse_time_sync_response(response_frame.payload)
    if response.status != STATUS_OK:
        raise RuntimeError(
            f"node {node.node_id}: TimeSync failed with status={response.status}"
        )
    if (
        response.sync_id != sync_id
        or response.echoed_t1_host_monotonic_ns != t1_host_monotonic_ns
    ):
        raise RuntimeError(f"node {node.node_id}: mismatched TimeSync echo")

    observation = TimeSyncObservation(
        boot_epoch=response.boot_epoch,
        sync_id=sync_id,
        t1_host_monotonic_ns=t1_host_monotonic_ns,
        t2_node_rx_us=response.t2_node_rx_us,
        t3_node_tx_us=response.t3_node_tx_us,
        t4_host_monotonic_ns=t4_host_monotonic_ns,
    )
    if not node.node_host_clock.add(observation):
        raise RuntimeError(f"node {node.node_id}: invalid TimeSync observation")
    return observation


def apply_packet_timing(
    node: RecorderNode,
    packet,
    samples: list[SampleRecord],
    *,
    now_monotonic_ns: int,
) -> None:
    if (
        packet.boot_epoch == 0
        or packet.timestamp_quality_flags & 0x04
        or not samples
    ):
        return

    uncertainty_us = max(float(packet.max_fit_residual_us), 1.0)
    node.sample_clock.add_anchor(
        node_id=node.node_id,
        boot_epoch=packet.boot_epoch,
        timing_segment_id=packet.timing_segment_id,
        sample_seq=packet.first_sample_seq,
        device_time_us=packet.first_device_time_us,
        uncertainty_us=uncertainty_us,
    )
    if packet.sample_count > 1:
        node.sample_clock.add_anchor(
            node_id=node.node_id,
            boot_epoch=packet.boot_epoch,
            timing_segment_id=packet.timing_segment_id,
            sample_seq=packet.first_sample_seq + packet.sample_count - 1,
            device_time_us=packet.last_device_time_us,
            uncertainty_us=uncertainty_us,
        )

    if (
        node.node_host_clock.estimate is None
        or node.utc_clock.state != ClockState.LOCKED
        or node.node_host_clock.boot_epoch != packet.boot_epoch
    ):
        return

    for sample in samples:
        if sample.device_time_us is None:
            continue
        host_ns, node_uncertainty_ns, state = (
            node.node_host_clock.predict_host_monotonic_ns(
                sample.device_time_us,
                now_monotonic_ns=now_monotonic_ns,
            )
        )
        if state not in (ClockState.LOCKED, ClockState.HOLDOVER):
            continue
        utc_ns, utc_uncertainty_ns, utc_state = (
            node.utc_clock.to_utc_ns(host_ns)
        )
        if utc_state != ClockState.LOCKED:
            continue
        sample.acquisition_utc_ns = utc_ns
        sample.timing_uncertainty_ns = round(
            node_uncertainty_ns
            + utc_uncertainty_ns
            + packet.max_fit_residual_us * 1000
        )


def initialize_node(
    client: ProtocolClient,
    node_id: int,
    start_from: str,
    timeout_s: float,
) -> RecorderNode:
    config = send_and_parse_config(client, node_id, timeout_s)
    state = send_and_parse_buffer_state(client, node_id, timeout_s)
    firmware_version = send_and_parse_version(client, node_id, timeout_s)
    node = RecorderNode(node_id=node_id, config=config, firmware_version=firmware_version)
    node.capabilities = send_and_parse_capabilities(
        client,
        node_id,
        timeout_s,
    )

    if start_from == "newest":
        newest = state.newest_packet_last_seq or state.newest_seq
        if newest > 0:
            node.committed_sample_seq = send_commit(client, node_id, newest, timeout_s)
        else:
            node.committed_sample_seq = state.committed_sample_seq
        node.expected_sample_seq = node.committed_sample_seq + 1
        return node

    node.committed_sample_seq = state.committed_sample_seq
    if state.oldest_packet_first_seq > 0:
        node.expected_sample_seq = max(state.committed_sample_seq + 1, state.oldest_packet_first_seq)
    else:
        node.expected_sample_seq = state.committed_sample_seq + 1
    return node


def make_single_writer(
    args: argparse.Namespace,
    metadata: dict[str, object],
    output: Path,
    append: bool = False,
) -> BaseWriter:
    return make_window_file_writer(
        args,
        metadata,
        output,
        append=append,
    )


def make_writer(args: argparse.Namespace, metadata: dict[str, object], nodes: list[RecorderNode]) -> BaseWriter:
    if getattr(args, "capture_schema", 5) == 1:
        if not args.output_dir or args.format != "hdf5":
            raise RuntimeError("Capture v1 requires HDF5 with --output-dir")
        if args.timing_mode != "required":
            raise RuntimeError("Capture v1 requires timing mode 'required'")
        identity = SensorIdentity(
            channel_id=args.channel_id,
            sensor_label=args.sensor_label,
            node_address=nodes[0].node_id,
            sensor_id=args.sensor_id or None,
            hardware_id=args.hardware_id or None,
            board_revision=args.board_revision or nodes[0].config.board_revision,
        )
        return CaptureV1WindowedWriter(args, metadata, nodes, identity)
    if args.output_dir:
        if getattr(args, "timing_mode", "legacy") == "required":
            if args.format != "hdf5":
                raise RuntimeError(
                    "timing mode required currently requires HDF5 output"
                )
            return AcquisitionWindowedWriter(args, metadata, nodes)
        return WindowedWriter(args, metadata, nodes)
    if not args.output:
        raise RuntimeError("either --output or --output-dir must be provided")
    return make_single_writer(args, metadata, Path(args.output))


def refresh_stats(client: ProtocolClient, node: RecorderNode, timeout_s: float) -> NodeStats | None:
    previous = node.last_stats
    sequence = client.send_command(node.node_id, build_stats_payload())
    response = client.wait_for_response(node.node_id, sequence, timeout_s)
    if response is not None:
        node.last_stats = parse_stats(response.payload)
    return previous


def _diagnostic_severity_name(value: int) -> str:
    return {0: "debug", 1: "info", 2: "warning", 3: "error", 4: "critical"}.get(value, "warning")


def _send_diagnostic_command(
    client: ProtocolClient,
    node_id: int,
    payload: bytes,
    timeout_s: float,
) -> bytes:
    sequence = client.send_command(node_id, payload)
    response = client.wait_for_response(node_id, sequence, timeout_s)
    if response is None or len(response.payload) < 2:
        raise RuntimeError("diagnostic command returned no valid response")
    if response.payload[1] != STATUS_OK:
        raise RuntimeError(f"diagnostic command failed with status={response.payload[1]}")
    return response.payload


def _diagnostic_message(event_code: int, repeat_count: int) -> str:
    name = format_diagnostic_event_code(event_code)
    messages = {
        32: "Przepełnienie FIFO: próbki mogły zostać utracone.",
        33: "Nie udało się odczytać stanu FIFO.",
        34: "Czujnik wielokrotnie nie zwrócił danych.",
        35: "Błąd odczytu danych z czujnika.",
        36: "Odrzucono nieprawidłową próbkę czujnika.",
        37: "Akwizycja wróciła do prawidłowej pracy.",
        48: "Nie udało się odczytać temperatury czujnika.",
    }
    message = messages.get(event_code, f"Zdarzenie diagnostyczne czujnika: {name}.")
    if repeat_count:
        message += f" Powtórzenia: {repeat_count}."
    return message


def archive_node_diagnostics(
    client: ProtocolClient,
    node: RecorderNode,
    timeout_s: float,
    event_writer: JsonlEventWriter,
    archive_writer: JsonlEventWriter,
) -> None:
    info_payload = _send_diagnostic_command(
        client, node.node_id, bytes([CMD_GET_DIAGNOSTIC_INFO]), timeout_s
    )
    info = parse_diagnostic_info_view(info_payload)
    host_now_ms = time.time_ns() // 1_000_000
    node.diagnostic_clock_offset_ms = host_now_ms - info.uptime_ms

    if info.dropped_event_count < node.diagnostic_dropped_events:
        node.diagnostic_dropped_events = 0
    if info.dropped_event_count > node.diagnostic_dropped_events:
        delta = info.dropped_event_count - node.diagnostic_dropped_events
        fields = {
            "message": f"Historia czujnika utraciła {delta} zdarzeń przed archiwizacją.",
            "lost_event_count": delta,
            "dropped_event_count": info.dropped_event_count,
            "first_available_event_id": info.first_event_id,
        }
        event_writer.emit("diagnostic_history_gap", severity="critical", node_id=node.node_id, fields=fields)
        archive_writer.emit("diagnostic_history_gap", severity="critical", node_id=node.node_id, fields=fields)
    node.diagnostic_dropped_events = info.dropped_event_count

    if node.next_diagnostic_event_id < info.first_event_id:
        delta = info.first_event_id - node.next_diagnostic_event_id
        # Event ids also cover intentionally filtered debug/info events and
        # survive an acknowledged RAM-buffer clear. Only the node's explicit
        # dropped counter proves that diagnostic history was actually lost.
        if info.dropped_event_count > 0:
            fields = {
                "message": f"Brakuje {delta} starszych zdarzeń w historii czujnika.",
                "lost_event_count": delta,
                "requested_event_id": node.next_diagnostic_event_id,
                "first_available_event_id": info.first_event_id,
            }
            event_writer.emit("diagnostic_history_gap", severity="critical", node_id=node.node_id, fields=fields)
            archive_writer.emit("diagnostic_history_gap", severity="critical", node_id=node.node_id, fields=fields)
        node.next_diagnostic_event_id = info.first_event_id

    while node.next_diagnostic_event_id < info.next_event_id:
        request = struct.pack(
            READ_DIAGNOSTIC_EVENTS_REQUEST_FORMAT,
            CMD_READ_DIAGNOSTIC_EVENTS,
            node.next_diagnostic_event_id,
            16,
        )
        payload = _send_diagnostic_command(client, node.node_id, request, timeout_s)
        first_id, next_id, events = parse_diagnostic_events(payload)
        if not events:
            node.next_diagnostic_event_id = info.next_event_id
            break
        for item in events:
            event_utc = datetime.fromtimestamp(
                (node.diagnostic_clock_offset_ms + item.time_ms) / 1000.0,
                tz=timezone.utc,
            ).isoformat()
            fields: dict[str, object] = {
                "message": _diagnostic_message(item.event_code, item.repeat_count),
                "node_event_utc": event_utc,
                "node_event_id": item.event_id,
                "node_uptime_ms": item.time_ms,
                "node_event_code": item.event_code,
                "node_event_name": format_diagnostic_event_code(item.event_code),
                "node_severity": format_diagnostic_severity(item.severity),
                "repeat_count": item.repeat_count,
                "sample_seq": item.sample_seq,
                "arg0": item.arg0,
                "arg1": item.arg1,
            }
            detail = format_diagnostic_detail(item.event_code, item.arg0, item.arg1)
            if detail is not None:
                fields["technical_detail"] = detail
            severity = _diagnostic_severity_name(item.severity)
            event_writer.emit("node_diagnostic_event", severity=severity, node_id=node.node_id, fields=fields)
            archive_writer.emit("node_diagnostic_event", severity=severity, node_id=node.node_id, fields=fields)
            node.next_diagnostic_event_id = item.event_id + 1
        if node.next_diagnostic_event_id >= next_id or next_id <= first_id:
            break

    if (
        info.stored_event_count >= max(1, (info.event_capacity * 3) // 4)
        and node.next_diagnostic_event_id >= info.next_event_id
    ):
        _send_diagnostic_command(
            client, node.node_id, bytes([CMD_CLEAR_DIAGNOSTIC_EVENTS]), timeout_s
        )
        archive_writer.emit(
            "diagnostic_history_acknowledged",
            node_id=node.node_id,
            fields={
                "message": "Historia czujnika została bezpiecznie zarchiwizowana i zwolniono bufor węzła.",
                "archived_through_event_id": info.next_event_id - 1,
                "stored_event_count": info.stored_event_count,
            },
        )


def emit_stats_changes(
    node: RecorderNode,
    previous: NodeStats | None,
    event_writer: JsonlEventWriter,
    archive_writer: JsonlEventWriter,
    measurement_writer: BaseWriter | None = None,
) -> None:
    current = node.last_stats
    if previous is None or current is None:
        return
    counters = (
        ("dropped_samples", "sample_loss", "critical", "Utracono próbki pomiarowe.", QualityEventCode.SENSOR_FIFO_LOSS),
        ("fifo_overrun_events", "fifo_overrun", "critical", "Wykryto przepełnienie FIFO.", QualityEventCode.SENSOR_FIFO_LOSS),
        ("fifo_discarded_samples", "fifo_samples_discarded", "error", "Odrzucono niepełne próbki FIFO.", QualityEventCode.SENSOR_FIFO_LOSS),
        ("fifo_uncertain_loss_events", "uncertain_sample_loss", "critical", "Wykryto możliwą stratę o nieznanym rozmiarze.", QualityEventCode.SENSOR_FIFO_LOSS),
        ("sensor_errors", "sensor_read_error", "error", "Wystąpił błąd odczytu czujnika.", QualityEventCode.UNKNOWN),
        ("soft_recover_count", "acquisition_recovery", "warning", "Firmware wznowił akwizycję.", QualityEventCode.SENSOR_RECOVERY),
        ("drdy_timestamp_ring_overflow", "drdy_ring_overflow", "critical", "Przepełnił się ring timestampów DRDY.", QualityEventCode.TIMING_INVALIDATION),
        ("timing_binding_mismatch", "timing_binding_mismatch", "critical", "Utracono jednoznaczne powiązanie DRDY z FIFO.", QualityEventCode.TIMING_INVALIDATION),
        ("timing_binding_invalidations", "timing_segment_invalidated", "error", "Segment czasu próbek został unieważniony.", QualityEventCode.TIMING_INVALIDATION),
    )
    for field, event, severity, message, quality_code in counters:
        before = int(getattr(previous, field, 0))
        after = int(getattr(current, field, 0))
        if after <= before:
            continue
        fields = {
            "message": message,
            "counter": field,
            "previous": before,
            "current": after,
            "delta": after - before,
            "sample_seq": current.last_sample_seq,
        }
        event_writer.emit(event, severity=severity, node_id=node.node_id, fields=fields)
        archive_writer.emit(event, severity=severity, node_id=node.node_id, fields=fields)
        if measurement_writer is not None:
            severity_code = {
                "warning": EventSeverity.WARNING,
                "error": EventSeverity.ERROR,
                "critical": EventSeverity.CRITICAL,
            }.get(severity, EventSeverity.INFO)
            measurement_writer.write_quality_event(
                node.node_id,
                QualityEventRecord(
                    boot_epoch=int(node.node_host_clock.boot_epoch or 0),
                    sample_seq_anchor=current.last_sample_seq,
                    observed_utc_ns=time.time_ns(),
                    event_code=int(quality_code),
                    severity=int(severity_code),
                    count=after - before,
                    value_a=before,
                    value_b=after,
                ),
            )


def node_total_sensor_loss(node: RecorderNode) -> int:
    return node.last_stats.dropped_samples if node.last_stats is not None else 0


def node_total_rx_overflow(node: RecorderNode) -> int:
    return node.last_stats.rx_overflow_count if node.last_stats is not None else 0


def node_total_packet_overwrite(node: RecorderNode) -> int:
    return node.last_stats.packet_overwrite_count if node.last_stats is not None else 0


def node_session_sensor_loss(node: RecorderNode) -> int:
    return max(0, node_total_sensor_loss(node) - node.baseline_sensor_loss)


def node_session_rx_overflow(node: RecorderNode) -> int:
    return max(0, node_total_rx_overflow(node) - node.baseline_rx_overflow_count)


def node_session_packet_overwrite(node: RecorderNode) -> int:
    return max(0, node_total_packet_overwrite(node) - node.baseline_packet_overwrite_count)


def capture_stats_baseline(node: RecorderNode) -> None:
    node.baseline_sensor_loss = node_total_sensor_loss(node)
    node.baseline_rx_overflow_count = node_total_rx_overflow(node)
    node.baseline_packet_overwrite_count = node_total_packet_overwrite(node)


def refresh_temperature(
    client: ProtocolClient,
    writer: BaseWriter,
    node: RecorderNode,
    timeout_s: float,
    sample_seq_anchor: Optional[int] = None,
) -> int:
    temperature = send_and_parse_temperature(client, node.node_id, timeout_s)
    rx_unix_ns = time.time_ns()
    resolved_sample_seq_anchor = (
        sample_seq_anchor if sample_seq_anchor is not None else max(node.last_written_seq, node.committed_sample_seq)
    )
    node.last_temperature_c = temperature.celsius
    node.last_temperature_unix_ns = rx_unix_ns
    writer.write_temperature(
        node.node_id,
        [
            TemperatureRecord(
                node_id=node.node_id,
                sample_seq_anchor=resolved_sample_seq_anchor,
                temp_raw=temperature.raw,
                temp_celsius=temperature.celsius,
                boot_epoch=int(node.node_host_clock.boot_epoch or 0),
                observed_utc_ns=rx_unix_ns,
            )
        ],
    )
    return resolved_sample_seq_anchor


def emit_temperature_sample_event(
    event_writer: JsonlEventWriter,
    node: RecorderNode,
    sample_seq_anchor: int,
    *,
    reason: str,
    window_start: Optional[datetime] = None,
) -> None:
    fields: dict[str, object] = {
        "sample_seq_anchor": sample_seq_anchor,
        "temp_celsius": node.last_temperature_c,
        "reason": reason,
    }
    if window_start is not None:
        fields["window_start_utc"] = window_start.isoformat()
    event_writer.emit(
        "temperature_sampled",
        node_id=node.node_id,
        fields=fields,
    )


def maybe_refresh_window_start_temperature(
    client: ProtocolClient,
    writer: BaseWriter,
    node: RecorderNode,
    args: argparse.Namespace,
    event_writer: JsonlEventWriter,
    *,
    now_monotonic: float,
    now_utc: Optional[datetime] = None,
) -> None:
    if not isinstance(writer, (WindowedWriter, CaptureV1WindowedWriter)):
        return

    window_start = writer.current_window(now_utc)
    if node.last_temperature_window_start == window_start:
        return
    if now_monotonic < node.next_window_temperature_retry_at:
        return

    try:
        sample_seq_anchor = refresh_temperature(
            client,
            writer,
            node,
            args.timeout,
            sample_seq_anchor=node.expected_sample_seq,
        )
        node.last_temperature_window_start = window_start
        node.next_window_temperature_retry_at = 0.0
        if args.temperature_interval > 0:
            node.next_temperature_at = now_monotonic + args.temperature_interval
        emit_temperature_sample_event(
            event_writer,
            node,
            sample_seq_anchor,
            reason="window_start",
            window_start=window_start,
        )
    except RuntimeError as exc:
        print(f"[WARN] {exc}", file=sys.stderr)
        event_writer.emit(
            "temperature_read_failed",
            severity="warning",
            node_id=node.node_id,
            fields={
                "error": str(exc),
                "reason": "window_start",
                "window_start_utc": window_start.isoformat(),
            },
        )
        node.next_window_temperature_retry_at = now_monotonic + max(args.error_sleep, 1.0)


def active_output_path(writer: BaseWriter, args: argparse.Namespace) -> Optional[str]:
    if isinstance(writer, (WindowedWriter, CaptureV1WindowedWriter)):
        return str(writer.current_path) if writer.current_path is not None else None
    if args.output:
        return str(Path(args.output))
    return None


def write_runtime_status(
    writer: BaseWriter,
    args: argparse.Namespace,
    nodes: list[RecorderNode],
    started_utc: str,
    status_writer: JsonStatusWriter,
) -> None:
    snapshot = RuntimeStatusSnapshot(
        schema_version=1,
        updated_utc=datetime.now(timezone.utc).isoformat(),
        started_utc=started_utc,
        recorder_version=RECORDER_VERSION,
        destination=args.output_dir or args.output,
        active_file=active_output_path(writer, args),
        port=args.port,
        baud=args.baud,
        channel_name=args.channel_name,
        nodes=[
            RuntimeStatusNode(
                node_id=node.node_id,
                name=None,
                firmware_version=node.firmware_version,
                board_revision=node.config.board_revision,
                online=node.online,
                sensor_odr_hz=node.config.odr_hz,
                output_odr_hz=effective_output_odr_hz(node.config.odr_hz),
                samples_written=node.samples_written,
                instant_samples_per_second_5s=node.instant_samples_per_second_5s,
                rate_stability_percent_5s=node.rate_stability_percent_5s,
                sample_flow_state=node.sample_flow_state,
                expected_sample_seq=node.expected_sample_seq,
                last_written_seq=node.last_written_seq,
                bursts_ok=node.bursts_ok,
                bursts_no_data=node.bursts_no_data,
                bursts_failed=node.bursts_failed,
                gaps_detected=node.gaps_detected,
                empty_polls=node.empty_polls,
                sensor_loss_total=node_total_sensor_loss(node),
                sensor_loss_session=node_session_sensor_loss(node),
                rx_overflow_total=node_total_rx_overflow(node),
                rx_overflow_session=node_session_rx_overflow(node),
                packet_overwrite_total=node_total_packet_overwrite(node),
                packet_overwrite_session=node_session_packet_overwrite(node),
                baseline_sensor_loss=node.baseline_sensor_loss,
                baseline_rx_overflow_count=node.baseline_rx_overflow_count,
                baseline_packet_overwrite_count=node.baseline_packet_overwrite_count,
                last_temperature_c=node.last_temperature_c,
                last_temperature_unix_ns=node.last_temperature_unix_ns,
            )
            for node in nodes
        ],
        timing_mode=args.timing_mode,
    )
    status_writer.write(snapshot)


def update_rate_metrics(nodes: Iterable[RecorderNode], now_monotonic: float) -> None:
    for node in nodes:
        if node.rate_history is None:
            node.rate_history = deque()
        node.rate_history.append((now_monotonic, node.samples_written))
        cutoff = now_monotonic - 5.5
        while len(node.rate_history) > 1 and node.rate_history[0][0] < cutoff:
            node.rate_history.popleft()

        history = list(node.rate_history)
        if len(history) < 2:
            node.instant_samples_per_second_5s = None
            node.rate_stability_percent_5s = None
            continue

        first_time, first_samples = history[0]
        last_time, last_samples = history[-1]
        elapsed = last_time - first_time
        if elapsed <= 0:
            node.instant_samples_per_second_5s = None
            node.rate_stability_percent_5s = None
            continue

        instant_rate = max(0.0, (last_samples - first_samples) / elapsed)
        node.instant_samples_per_second_5s = instant_rate

        interval_rates: list[float] = []
        for (prev_time, prev_samples), (curr_time, curr_samples) in zip(history, history[1:]):
            interval_elapsed = curr_time - prev_time
            if interval_elapsed <= 0:
                continue
            interval_rates.append(max(0.0, (curr_samples - prev_samples) / interval_elapsed))

        if not interval_rates:
            node.rate_stability_percent_5s = None
            continue
        if len(interval_rates) == 1:
            node.rate_stability_percent_5s = 100.0
            continue

        mean_rate = sum(interval_rates) / len(interval_rates)
        variance = sum((rate - mean_rate) ** 2 for rate in interval_rates) / len(interval_rates)
        stddev = math.sqrt(variance)
        node.rate_stability_percent_5s = max(
            0.0,
            min(100.0, 100.0 * (1.0 - (stddev / max(mean_rate, 1e-9)))))


def emit_sample_flow_changes(
    nodes: Iterable[RecorderNode],
    now_monotonic: float,
    event_writer: JsonlEventWriter,
) -> None:
    for node in nodes:
        rate = node.instant_samples_per_second_5s
        if not node.online:
            node.sample_flow_state = "offline"
            node.sample_flow_zero_since = None
            continue
        if rate is None:
            node.sample_flow_state = "starting"
            continue
        if rate > 0.0:
            was_stalled = node.sample_flow_state == "stalled"
            stalled_for_s = (
                max(0.0, now_monotonic - node.sample_flow_zero_since)
                if node.sample_flow_zero_since is not None
                else None
            )
            node.sample_flow_state = "flowing"
            node.sample_flow_zero_since = None
            if was_stalled:
                event_writer.emit(
                    "sample_flow_recovered",
                    node_id=node.node_id,
                    fields={
                        "message": "Recorder ponownie otrzymuje próbki z czujnika.",
                        "samples_per_second_5s": rate,
                        "stalled_for_s": stalled_for_s,
                    },
                )
            continue

        if node.sample_flow_zero_since is None:
            node.sample_flow_zero_since = now_monotonic
        if (now_monotonic - node.sample_flow_zero_since) < SAMPLE_FLOW_STALL_CONFIRM_S:
            node.sample_flow_state = "stopping"
            continue
        if node.sample_flow_state != "stalled":
            event_writer.emit(
                "sample_flow_stalled",
                severity="error",
                node_id=node.node_id,
                fields={
                    "message": "Czujnik jest online, ale recorder nie otrzymuje żadnych próbek.",
                    "samples_per_second_5s": rate,
                    "samples_written": node.samples_written,
                    "expected_sample_seq": node.expected_sample_seq,
                },
            )
        node.sample_flow_state = "stalled"


def record_one_burst(
    client: ProtocolClient,
    writer: BaseWriter,
    node: RecorderNode,
    args: argparse.Namespace,
    event_writer: JsonlEventWriter,
    live: LiveBuffer | None,
) -> None:
    sequence = client.send_command(
        node.node_id,
        build_grant_burst_payload(node.expected_sample_seq, args.grant_packets),
    )
    response = client.wait_for_response(node.node_id, sequence, args.timeout)
    if response is None:
        node.online = False
        node.bursts_failed += 1
        time.sleep(args.idle_sleep)
        return

    _, status, _granted_start_seq, granted_max_frames = parse_grant_burst_response(response.payload)
    if status == STATUS_NO_DATA:
        node.online = True
        node.bursts_no_data += 1
        node.empty_polls += 1
        time.sleep(args.idle_sleep)
        return

    if status != STATUS_OK:
        node.online = False
        node.bursts_failed += 1
        time.sleep(args.error_sleep)
        return

    packets = client.collect_burst_packets(
        node.node_id,
        max_packets=granted_max_frames,
        idle_timeout_s=args.burst_idle_timeout,
        session_timeout_s=args.burst_session_timeout,
    )
    if not packets:
        node.online = False
        node.bursts_failed += 1
        time.sleep(args.error_sleep)
        return

    batch: list[SampleRecord] = []
    expected_seq = node.expected_sample_seq
    last_contiguous_seq = node.committed_sample_seq

    for packet in packets:
        if packet.first_sample_seq != expected_seq:
            node.gaps_detected += 1
            writer.write_gap(
                node.node_id,
                expected_sample_seq=expected_seq,
                received_sample_seq=packet.first_sample_seq,
                packet_seq=packet.packet_seq,
                boot_epoch=packet.boot_epoch,
            )
            if live is not None:
                live.publish_gap(
                    LiveGap(
                        node_id=node.node_id,
                        expected_sample_seq=expected_seq,
                        received_sample_seq=packet.first_sample_seq,
                        packet_seq=packet.packet_seq,
                    )
                )
            event_writer.emit(
                "gap_detected",
                severity="warning",
                node_id=node.node_id,
                fields={
                    "expected_sample_seq": expected_seq,
                    "received_sample_seq": packet.first_sample_seq,
                    "packet_seq": packet.packet_seq,
                },
            )
            expected_seq = packet.first_sample_seq

        samples = decode_packet_samples(
            node.node_id,
            packet.payload,
            packet.first_sample_seq,
            packet.sample_count,
            packet.packet_seq,
            node.config.range_g,
            sample_payload_offset=packet.sample_payload_offset,
            boot_epoch=packet.boot_epoch,
            timing_segment_id=packet.timing_segment_id,
            timing_quality_flags=packet.timestamp_quality_flags,
            first_device_time_us=packet.first_device_time_us,
            last_device_time_us=packet.last_device_time_us,
            timing_format_version=packet.timing_format_version,
            timestamp_source=packet.timestamp_source,
            sample_period_q16_us=packet.sample_period_q16_us,
            max_fit_residual_us=packet.max_fit_residual_us,
        )
        apply_packet_timing(
            node,
            packet,
            samples,
            now_monotonic_ns=time.monotonic_ns(),
        )
        if live is not None and samples:
            live.publish_samples(
                node.node_id,
                [
                    LiveSample(
                        node_id=s.node_id,
                        sample_seq=s.sample_seq,
                        x=s.x,
                        y=s.y,
                        z=s.z,
                        packet_seq=s.packet_seq,
                    )
                    for s in samples
                ],
            )
        batch.extend(samples)
        last_contiguous_seq = packet.first_sample_seq + packet.sample_count - 1
        expected_seq = last_contiguous_seq + 1

    writer.write_samples(node.node_id, batch)
    writer.flush()
    node.committed_sample_seq = send_commit(
        client,
        node.node_id,
        last_contiguous_seq,
        args.timeout,
    )
    node.expected_sample_seq = node.committed_sample_seq + 1
    node.last_written_seq = last_contiguous_seq
    node.samples_written += len(batch)
    node.bursts_ok += 1
    node.online = True


def handle_node_runtime_error(
    node: RecorderNode,
    args: argparse.Namespace,
    event_writer: JsonlEventWriter,
    exc: RuntimeError,
) -> None:
    node.online = False
    node.bursts_failed += 1
    print(f"[WARN] {exc}", file=sys.stderr)
    event_writer.emit(
        "runtime_warning",
        severity="warning",
        node_id=node.node_id,
        fields={"channel_name": args.channel_name, "error": str(exc)},
    )
    time.sleep(args.error_sleep)


def print_status(nodes: Iterable[RecorderNode], started_at: float) -> None:
    elapsed_s = max(0.001, time.monotonic() - started_at)
    total_samples = sum(node.samples_written for node in nodes)
    print(
        f"[REC] t={elapsed_s:8.1f}s samples={total_samples} "
        f"rate={total_samples / elapsed_s:7.1f} samples/s"
    )
    for node in nodes:
        stats_suffix = ""
        if node.last_stats is not None:
            stats_suffix = (
                f" sensor_loss={node_session_sensor_loss(node)}"
                f" rx_ovf={node_session_rx_overflow(node)}"
                f" pkt_ovf={node_session_packet_overwrite(node)}"
                f" poll={node.last_stats.fifo_poll_fallback_reads}"
                f" recov={node.last_stats.soft_recover_count}"
                f" nd_irq={node.last_stats.no_data_with_irq}"
                f" nd_poll={node.last_stats.no_data_without_irq}"
                f" int1={node.last_stats.fifo_int1_events}"
                f" drdy={node.last_stats.fifo_drdy_events}"
                f" gpio_i1={node.last_stats.gpio_int1_edges}"
                f" gpio_d={node.last_stats.gpio_drdy_edges}"
                f" dbg=0x{node.last_stats.debug_config_snapshot:08X}"
                f" irq_nf={node.last_stats.irq_status_not_full}"
                f" irq_lt3={node.last_stats.irq_fifo_entries_lt_3}"
                f" irq_ltwm={node.last_stats.irq_fifo_entries_lt_watermark}"
                f" irqdbg=0x{node.last_stats.debug_irq_snapshot:08X}"
                f" spurious_i1={node.last_stats.spurious_int1_events}"
                f" fifo_ovr={node.last_stats.fifo_overrun_events}"
                f" fifo_discard={node.last_stats.fifo_discarded_samples}"
                f" fifo_loss_unknown={node.last_stats.fifo_uncertain_loss_events}"
                f" totals=({node_total_sensor_loss(node)}/{node_total_rx_overflow(node)}/{node_total_packet_overwrite(node)})"
            )
        print(
            f"  node={node.node_id} written={node.samples_written}"
            f" next={node.expected_sample_seq}"
            f" bursts_ok={node.bursts_ok}"
            f" no_data={node.bursts_no_data}"
            f" failed={node.bursts_failed}"
            f" gaps={node.gaps_detected}"
            f"{stats_suffix}"
        )
    sys.stdout.flush()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Sensor System RS485 recorder")
    parser.add_argument("--port", default="/dev/sensor-system-rs485")
    parser.add_argument("--baud", type=int, default=115200)
    parser.add_argument("--channel-name", default="default")
    parser.add_argument("--channel-id", type=int, default=1)
    parser.add_argument("--sensor-label", default="A")
    parser.add_argument("--sensor-id", default="")
    parser.add_argument("--hardware-id", default="")
    parser.add_argument("--board-revision", type=int, choices=[1, 2])
    parser.add_argument("--capture-schema", type=int, choices=[1, 5], default=5)
    parser.add_argument("--nodes", type=parse_node_list, default=[1], help="Comma separated node ids, e.g. 1,2")
    parser.add_argument("--output", help="Single output file path, e.g. run.h5 or run.csv")
    parser.add_argument("--output-dir", help="Root directory for rotated output files")
    parser.add_argument("--overwrite", action="store_true", help="Overwrite output file if it already exists")
    parser.add_argument("--format", choices=["hdf5", "csv"], default="hdf5")
    parser.add_argument("--compression", choices=["gzip", "lzf", "none"], default="gzip")
    parser.add_argument(
        "--window-seconds",
        type=int,
        default=DEFAULT_WINDOW_SECONDS,
        help="Rotation window for --output-dir; default creates 10-minute files",
    )
    parser.add_argument(
        "--window-timezone",
        default="local",
        help="Timezone used to align --output-dir windows; use 'local', 'UTC', or an IANA name like Europe/Warsaw",
    )
    parser.add_argument("--duration", type=float, default=0.0, help="Seconds to record; 0 means until Ctrl+C")
    parser.add_argument(
        "--min-free-bytes",
        type=int,
        default=DEFAULT_MIN_FREE_BYTES,
        help="Stop safely before storage free space falls below this reserve",
    )
    parser.add_argument("--start-from", choices=["newest", "oldest"], default="newest")
    parser.add_argument("--grant-packets", type=int, default=4)
    parser.add_argument("--timeout", type=float, default=0.5)
    parser.add_argument("--burst-idle-timeout", type=float, default=0.15)
    parser.add_argument("--burst-session-timeout", type=float, default=0.75)
    parser.add_argument("--status-interval", type=float, default=1.0)
    parser.add_argument(
        "--console-status-interval",
        type=float,
        default=30.0,
        help="Seconds between human-readable progress lines; runtime JSON is updated independently",
    )
    parser.add_argument("--flush-interval", type=float, default=2.0)
    parser.add_argument("--stats-interval", type=float, default=30.0)
    parser.add_argument(
        "--timing-mode",
        choices=["legacy", "observe", "required"],
        default="legacy",
        help=(
            "legacy keeps receive-time rotation; observe computes acquisition "
            "time without changing routing; required enforces acquisition-time routing"
        ),
    )
    parser.add_argument(
        "--temperature-interval",
        type=float,
        default=3600.0,
        help="Seconds between periodic temperature reads; 0 disables periodic reads but rotated files still get a window-start sample",
    )
    parser.add_argument("--status-file", default="/tmp/sensor-system_recorder_status.json", help="JSON runtime status for the operator console")
    parser.add_argument("--event-log", default="/tmp/sensor-system_recorder_events.jsonl", help="JSONL event log for recorder lifecycle and warnings")
    parser.add_argument("--idle-sleep", type=float, default=0.01)
    parser.add_argument("--error-sleep", type=float, default=0.10)
    parser.add_argument("--live", action="store_true", help="Serve a minimal live web UI (time plot + FFT) over HTTP/SSE")
    parser.add_argument("--live-host", default="0.0.0.0")
    parser.add_argument("--live-port", type=int, default=8000)
    args = parser.parse_args()
    if not args.output and not args.output_dir:
        parser.error("one of --output or --output-dir is required")
    if args.output and args.output_dir:
        parser.error("use either --output or --output-dir, not both")
    if args.window_seconds <= 0:
        parser.error("--window-seconds must be greater than zero")
    if args.min_free_bytes < 0:
        parser.error("--min-free-bytes must be non-negative")
    if args.capture_schema == 1 and args.output_dir and (
        not 60 <= args.window_seconds <= 3600
        or 86400 % args.window_seconds != 0
    ):
        parser.error(
            "Capture v1 --window-seconds must divide one UTC day and be "
            "in range 60..3600"
        )
    args.window_timezone_name = args.window_timezone
    try:
        args.window_timezone = resolve_window_timezone(args.window_timezone_name)
    except ValueError as exc:
        parser.error(str(exc))
    return args


def storage_usage_path(destination: str | Path) -> Path:
    candidate = Path(destination).resolve()
    while not candidate.exists() and candidate != candidate.parent:
        candidate = candidate.parent
    return candidate


def ensure_storage_reserve(destination: str | Path, min_free_bytes: int) -> int:
    try:
        free = shutil.disk_usage(storage_usage_path(destination)).free
    except OSError as exc:
        raise StorageError(
            f"cannot inspect storage capacity for '{destination}': {exc}"
        ) from exc
    if free < min_free_bytes:
        raise StorageError(
            f"storage reserve breached for '{destination}': "
            f"free={free} bytes, required={min_free_bytes} bytes"
        )
    return free


def run_recorder(args: argparse.Namespace) -> int:
    started_at = time.monotonic()
    created_utc = datetime.now(timezone.utc).isoformat()
    stop_reason = "running"
    stop_flag = StopFlag()
    status_writer = JsonStatusWriter(args.status_file)
    event_writer = JsonlEventWriter(args.event_log)
    diagnostic_root = Path(args.output_dir) if args.output_dir else Path(args.output).parent
    diagnostic_archive = JsonlEventWriter(
        diagnostic_root / "diagnostics" / f"{args.channel_name}.events.jsonl"
    )
    destination = args.output_dir or args.output
    signal.signal(signal.SIGINT, stop_flag.request_stop)
    signal.signal(signal.SIGTERM, stop_flag.request_stop)

    metadata: dict[str, object] = {
        "created_utc": created_utc,
        "host_node_id": HOST_NODE_ID,
        "channel_name": args.channel_name,
        "port": args.port,
        "baud": args.baud,
        "start_from": args.start_from,
        "grant_packets": args.grant_packets,
        "window_seconds": args.window_seconds,
        "window_timezone": args.window_timezone_name,
        "recorder_version": RECORDER_VERSION,
        "timing_mode": args.timing_mode,
        "host_name": socket.gethostname(),
    }

    try:
        ensure_storage_reserve(destination, args.min_free_bytes)
        live_buffer: LiveBuffer | None = None
        live_server: LiveServer | None = None
        with serial.Serial(
            port=args.port,
            baudrate=args.baud,
            timeout=0.03,
            write_timeout=0.5,
        ) as ser:
            client = ProtocolClient(ser)
            nodes = [
                initialize_node(client, node_id, args.start_from, args.timeout)
                for node_id in args.nodes
            ]
            for node in nodes:
                if (
                    args.board_revision is not None
                    and node.config.board_revision is not None
                    and args.board_revision != node.config.board_revision
                ):
                    raise RuntimeError(
                        f"node {node.node_id}: configured board_revision="
                        f"{args.board_revision}, reported={node.config.board_revision}"
                    )
                refresh_stats(client, node, args.timeout)
                capture_stats_baseline(node)
                node.next_time_sync_at = started_at
                if args.timing_mode == "required":
                    flags = (
                        node.capabilities.feature_flags
                        if node.capabilities is not None
                        else 0
                    )
                    if not (flags & CAPABILITY_TIME_SYNC_V1):
                        raise RuntimeError(
                            f"node {node.node_id}: timing mode required but "
                            "TimeSync v1 is unavailable"
                        )

            if args.live:
                live_buffer = LiveBuffer()
                live_buffer.set_meta(
                    {
                        "created_utc": created_utc,
                        "port": args.port,
                        "baud": args.baud,
                        "output_odr_hz": effective_output_odr_hz(nodes[0].config.odr_hz) if nodes else 0.0,
                        "nodes": [
                            {
                                "node_id": n.node_id,
                                "sensor_odr_hz": n.config.odr_hz,
                                "output_odr_hz": effective_output_odr_hz(n.config.odr_hz),
                                "range_g": n.config.range_g,
                                "high_pass_corner": n.config.high_pass_corner,
                                "fifo_watermark": n.config.fifo_watermark,
                            }
                            for n in nodes
                        ],
                    }
                )
                live_server = LiveServer(args.live_host, args.live_port, live_buffer)
                live_server.start()
                print(f"[LIVE] http://{args.live_host}:{args.live_port}/")

            writer = make_writer(args, metadata, nodes)
            try:
                for recovery in getattr(writer, "recovery_events", ()):
                    recovery_event = str(
                        recovery.get("event", "partial_window_recovery")
                    )
                    severity = (
                        "error"
                        if recovery_event == "partial_window_unrecoverable"
                        else "warning"
                    )
                    fields = dict(recovery)
                    fields.pop("event", None)
                    event_writer.emit(
                        recovery_event,
                        severity=severity,
                        fields=fields,
                    )
                    diagnostic_archive.emit(
                        recovery_event,
                        severity=severity,
                        fields=fields,
                    )
                event_writer.emit(
                    "recorder_started",
                    fields={
                        "channel_name": args.channel_name,
                        "port": args.port,
                        "baud": args.baud,
                        "destination": args.output_dir or args.output,
                        "node_ids": list(args.nodes),
                    },
                )
                for node in nodes:
                    writer.add_node(node)
                    node.next_temperature_at = started_at
                    node.online = True
                    event_writer.emit(
                        "node_initialized",
                        node_id=node.node_id,
                        fields={
                            "sensor_odr_hz": node.config.odr_hz,
                            "output_odr_hz": effective_output_odr_hz(node.config.odr_hz),
                            "range_g": node.config.range_g,
                            "high_pass_corner": node.config.high_pass_corner,
                            "baseline_sensor_loss": node.baseline_sensor_loss,
                            "baseline_rx_overflow_count": node.baseline_rx_overflow_count,
                            "baseline_packet_overwrite_count": node.baseline_packet_overwrite_count,
                        },
                    )
                    print(
                        f"[INIT] channel={args.channel_name} node={node.node_id} "
                        f"sensor_odr={node.config.odr_hz}Hz "
                        f"output_odr={effective_output_odr_hz(node.config.odr_hz):g}Hz "
                        f"range={node.config.range_g}g "
                        f"high_pass={node.config.high_pass_corner} "
                        f"fifo_watermark={node.config.fifo_watermark} "
                        f"start_seq={node.expected_sample_seq}"
                    )
                    try:
                        archive_node_diagnostics(
                            client,
                            node,
                            args.timeout,
                            event_writer,
                            diagnostic_archive,
                        )
                    except RuntimeError as exc:
                        event_writer.emit(
                            "diagnostic_poll_failed",
                            severity="warning",
                            node_id=node.node_id,
                            fields={
                                "message": "Nie udało się pobrać historii diagnostycznej czujnika.",
                                "error": str(exc),
                            },
                        )

                next_status_at = started_at
                next_console_status_at = started_at
                next_flush_at = started_at + args.flush_interval
                next_stats_at = started_at + args.stats_interval
                next_storage_check_at = started_at + 30.0

                while not stop_flag.stop_requested:
                    now = time.monotonic()
                    if now >= next_storage_check_at:
                        ensure_storage_reserve(destination, args.min_free_bytes)
                        next_storage_check_at = now + 30.0
                    if args.duration > 0 and (now - started_at) >= args.duration:
                        stop_reason = "duration_elapsed"
                        break

                    loop_now_utc = datetime.now(timezone.utc)
                    for node in nodes:
                        try:
                            if (
                                args.timing_mode != "legacy"
                                and now >= node.next_time_sync_at
                            ):
                                flags = (
                                    node.capabilities.feature_flags
                                    if node.capabilities is not None
                                    else 0
                                )
                                if flags & CAPABILITY_TIME_SYNC_V1:
                                    node.next_time_sync_at = now + 5.0
                                    observation = refresh_time_sync(
                                        client,
                                        node,
                                        args.timeout,
                                    )
                                    writer.write_clock_sync(
                                        node.node_id,
                                        observation,
                                        True,
                                    )
                                    sync_interval = (
                                        30.0
                                        if node.node_host_clock.state
                                        == ClockState.LOCKED
                                        else 5.0
                                    )
                                    node.next_time_sync_at = (
                                        time.monotonic() + sync_interval
                                    )
                            maybe_refresh_window_start_temperature(
                                client,
                                writer,
                                node,
                                args,
                                event_writer,
                                now_monotonic=now,
                                now_utc=loop_now_utc,
                            )
                            record_one_burst(client, writer, node, args, event_writer, live_buffer)
                        except StorageError:
                            raise
                        except RuntimeError as exc:
                            handle_node_runtime_error(node, args, event_writer, exc)

                    now = time.monotonic()
                    if args.temperature_interval > 0:
                        for node in nodes:
                            if now >= node.next_temperature_at:
                                try:
                                    sample_seq_anchor = refresh_temperature(client, writer, node, args.timeout)
                                    if isinstance(writer, (WindowedWriter, CaptureV1WindowedWriter)):
                                        node.last_temperature_window_start = writer.current_window()
                                        node.next_window_temperature_retry_at = 0.0
                                    emit_temperature_sample_event(
                                        event_writer,
                                        node,
                                        sample_seq_anchor,
                                        reason="periodic",
                                    )
                                except StorageError:
                                    raise
                                except RuntimeError as exc:
                                    print(f"[WARN] {exc}", file=sys.stderr)
                                    event_writer.emit(
                                        "temperature_read_failed",
                                        severity="warning",
                                        node_id=node.node_id,
                                        fields={"error": str(exc), "reason": "periodic"},
                                    )
                                node.next_temperature_at = now + args.temperature_interval
                    if now >= next_stats_at:
                        for node in nodes:
                            previous_stats = refresh_stats(client, node, args.timeout)
                            emit_stats_changes(
                                node,
                                previous_stats,
                                event_writer,
                                diagnostic_archive,
                                writer,
                            )
                            try:
                                archive_node_diagnostics(
                                    client,
                                    node,
                                    args.timeout,
                                    event_writer,
                                    diagnostic_archive,
                                )
                            except RuntimeError as exc:
                                event_writer.emit(
                                    "diagnostic_poll_failed",
                                    severity="warning",
                                    node_id=node.node_id,
                                    fields={
                                        "message": "Nie udało się pobrać nowych zdarzeń diagnostycznych.",
                                        "error": str(exc),
                                    },
                                )
                        next_stats_at = now + args.stats_interval

                    if now >= next_flush_at:
                        writer.flush()
                        next_flush_at = now + args.flush_interval

                    if now >= next_status_at:
                        update_rate_metrics(nodes, now)
                        emit_sample_flow_changes(nodes, now, event_writer)
                        write_runtime_status(writer, args, nodes, created_utc, status_writer)
                        next_status_at = now + args.status_interval
                    if now >= next_console_status_at:
                        print_status(nodes, started_at)
                        next_console_status_at = now + max(1.0, args.console_status_interval)
            finally:
                if stop_flag.stop_requested:
                    stop_reason = f"signal:{stop_flag.signal_name or stop_flag.signal_number}"
                write_runtime_status(writer, args, nodes, created_utc, status_writer)
                close_error: Exception | None = None
                try:
                    writer.close()
                except Exception as exc:
                    close_error = exc
                if live_server is not None:
                    live_server.stop()
                event_writer.emit(
                    "recorder_stopped",
                    fields={
                        "channel_name": args.channel_name,
                        "destination": args.output_dir or args.output,
                        "samples_written": sum(node.samples_written for node in nodes),
                        "stop_reason": stop_reason,
                        "signal_number": stop_flag.signal_number,
                        "signal_name": stop_flag.signal_name,
                        "close_error": str(close_error) if close_error else None,
                    },
                )
                if close_error is not None:
                    raise StorageError(
                        f"failed to close recorder storage safely: {close_error}"
                    ) from close_error

        print_status(nodes, started_at)
        destination = args.output_dir or args.output
        print(f"[DONE] wrote {destination}")
        return 0 if all(node.gaps_detected == 0 for node in nodes) else 2
    except serial.SerialException as exc:
        event_writer.emit(
            "serial_error",
            severity="error",
            fields={"channel_name": args.channel_name, "port": args.port, "error": str(exc)},
        )
        print(f"[ERROR] serial error on {args.port}: {exc}", file=sys.stderr)
        return 1
    except StorageError as exc:
        event_writer.emit(
            "storage_error",
            severity="critical",
            fields={
                "channel_name": args.channel_name,
                "destination": args.output_dir or args.output,
                "error": str(exc),
            },
        )
        print(f"[ERROR] storage failure: {exc}", file=sys.stderr)
        return 3
    except OSError as exc:
        event_writer.emit(
            "storage_io_error",
            severity="critical",
            fields={
                "channel_name": args.channel_name,
                "destination": args.output_dir or args.output,
                "error": str(exc),
            },
        )
        print(f"[ERROR] storage I/O failure: {exc}", file=sys.stderr)
        return 3
    except RuntimeError as exc:
        event_writer.emit(
            "runtime_error",
            severity="error",
            fields={"channel_name": args.channel_name, "error": str(exc)},
        )
        print(f"[ERROR] {exc}", file=sys.stderr)
        return 1


def recorder_lock_path(args: argparse.Namespace) -> Path:
    """Use the runtime-status namespace so supervised and manual runs conflict."""
    safe_channel = "".join(
        character if character.isalnum() or character in "-_" else "_"
        for character in args.channel_name
    ) or "default"
    return Path(args.status_file).parent / f"{safe_channel}.recorder.lock"


def main() -> int:
    args = parse_args()
    lock = InstanceLock(
        recorder_lock_path(args),
        {
            "kind": "sensor-system-recorder",
            "channel_name": args.channel_name,
            "port": args.port,
            "destination": args.output_dir or args.output,
        },
    )
    try:
        lock.acquire()
        return run_recorder(args)
    except OSError as exc:
        print(f"[ERROR] recorder runtime storage failure: {exc}", file=sys.stderr)
        return 3
    except RuntimeError as exc:
        print(f"[ERROR] {exc}", file=sys.stderr)
        return 1
    finally:
        lock.release()


if __name__ == "__main__":
    raise SystemExit(main())
