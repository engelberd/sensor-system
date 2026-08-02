"""Decode wire-format acceleration samples into capture-domain records."""

from __future__ import annotations

import struct
from typing import Optional

from host.host_lab import BURST_HEADER_FORMAT
from host.recorder.model import SampleRecord, scale_m_s2_per_lsb


SAMPLE_PAYLOAD_OFFSET = struct.calcsize(BURST_HEADER_FORMAT)


def decode_i24_be(data: bytes) -> int:
    value = (data[0] << 16) | (data[1] << 8) | data[2]
    if value & 0x800000:
        value -= 0x1000000
    return value


def raw_lsb_to_m_s2(raw_value: int, range_g: int) -> float:
    return raw_value * scale_m_s2_per_lsb(range_g)


def decode_packet_samples(
    node_id: int,
    packet_payload: bytes,
    first_sample_seq: int,
    sample_count: int,
    packet_seq: int,
    range_g: int,
    sample_payload_offset: int = SAMPLE_PAYLOAD_OFFSET,
    boot_epoch: int = 0,
    timing_segment_id: int = 0,
    timing_quality_flags: int = 0,
    first_device_time_us: int = 0,
    last_device_time_us: int = 0,
    timing_format_version: int = 0,
    timestamp_source: int = 0,
    sample_period_q16_us: int = 0,
    max_fit_residual_us: int = 0,
) -> list[SampleRecord]:
    expected_size = sample_payload_offset + sample_count * 9
    if len(packet_payload) < expected_size:
        raise ValueError(
            "burst packet payload is shorter than declared sample_count"
        )

    records: list[SampleRecord] = []
    base = sample_payload_offset
    for index in range(sample_count):
        offset = base + index * 9
        device_time_us: Optional[int] = None
        if boot_epoch != 0:
            if sample_count == 1:
                device_time_us = first_device_time_us
            else:
                device_time_us = (
                    first_device_time_us
                    + (
                        (last_device_time_us - first_device_time_us) * index
                    )
                    // (sample_count - 1)
                )
        records.append(
            SampleRecord(
                node_id=node_id,
                sample_seq=first_sample_seq + index,
                raw_x=decode_i24_be(packet_payload[offset:offset + 3]),
                raw_y=decode_i24_be(packet_payload[offset + 3:offset + 6]),
                raw_z=decode_i24_be(packet_payload[offset + 6:offset + 9]),
                packet_seq=packet_seq,
                range_g=range_g,
                device_time_us=device_time_us,
                boot_epoch=boot_epoch,
                timing_segment_id=timing_segment_id,
                timing_quality_flags=timing_quality_flags,
                timing_format_version=timing_format_version,
                timestamp_source=timestamp_source,
                sample_period_q16_us=sample_period_q16_us,
                max_fit_residual_us=max_fit_residual_us,
            )
        )
    return records
