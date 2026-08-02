# Sensor System data formats v1

This document is the implementation contract for the first clean Capture and
Archive formats. Dataset names, types, units, sentinels, and invariants are
normative. Examples are descriptive.

## 1. Common rules

### 1.1 Identity

Every file describes exactly one physical sensor.

| Field | Type | Meaning |
|---|---|---|
| `channel_id` | `uint8` | Transport channel `1..8` |
| `sensor_label` | fixed ASCII | Temporary label `A..H` |
| `sensor_id` | fixed ASCII | Future permanent identifier; empty until assigned |
| `node_address` | `uint8` | Address used on the channel |
| `hardware_id` | fixed ASCII | MCU hardware identifier when available |

Initial mapping is `1→A`, `2→B`, ..., `8→H`. Channel, sensor, and node address
are separate concepts. A future sensor move creates a new assignment interval;
it does not rewrite old files.

### 1.2 Numeric conventions

- Integers are explicitly little-endian.
- UTC is signed Unix nanoseconds in `int64`.
- Device time is microseconds in `uint64`.
- `utc_ns = -1` means unavailable. Zero is not used as a missing UTC value.
- `device_time_us = 0` is unavailable only when the accompanying validity flag
  is not set.
- Ranges are half-open: `[start, start + count)`.
- Dataset units are recorded in a `unit` attribute.
- Enumerations are stored as integers and defined in this specification and in
  one shared Python module.

### 1.3 File attributes

Both products contain:

```text
format_name
schema_major
schema_minor
file_id
channel_id
sensor_label
sensor_id
node_address
hardware_id
firmware_version
created_utc_ns
finalized_utc_ns
complete
```

Fixed text widths are `S36` for UUID, `S8` for sensor label, `S64` for permanent
sensor ID, `S32` for hardware ID and firmware version, and `S32` for calibration
revision. `file_id` and session identifiers use canonical lowercase UUID text.
Files are readable only when `complete=true`. Writers publish through
`.partial`, flush all datasets, close the file, atomically rename it, and fsync
the directory.

### 1.4 Source sample representation

The firmware sends the filtered and x2-decimated acceleration as signed 24-bit
integers. HDF5 stores them in an `int32` container without further scaling:

```text
/measurement/raw_xyz  dtype=<i4, shape=(N, 3)
```

Axes are ordered `X,Y,Z`. Attributes include:

```text
encoding = "signed_integer"
valid_bits = 24
unit = "sensor_lsb"
processing_stage = "firmware_filtered_decimated"
```

Readers expose SI values without modifying the source array:

```text
acceleration_m_s2 = raw_xyz * scale_g_per_lsb * 9.80665
```

The selected ADXL355 range, firmware filter profile, decimation factor, offsets,
and calibration revision are recorded in configuration intervals. Host-side
floating-point values are never considered the canonical source.

### 1.5 Calibration model v1

Product calibration is performed by writing `offset_x`, `offset_y`, and
`offset_z` to the ADXL355 offset registers. Archive v1 records those exact
register values and the sample-sequence boundary from which they apply, using
`calibration_method = "sensor_offset_registers"`. No host-side bias vector or
cross-axis matrix is required by v1. A future calibration method may extend the
configuration contract under a new method identifier without changing stored
raw samples.

## 2. Capture HDF5 v1

### 2.1 Purpose and partitioning

Capture is a short-retention, crash-recoverable acquisition journal. Normal
operation uses 10-minute UTC files per sensor. Filename boundaries organize
storage but do not define signal continuity.

Recommended name:

```text
capture/ch-01/sensor-A/2026-08-02/
  sensor-A_20260802T124000Z.capture.h5
```

### 2.2 Measurement blocks

`/capture/blocks` is the commit journal for `/measurement/raw_xyz`. One row
describes a contiguous block derived from one transport packet or packet
fragment.

| Field | Type | Meaning |
|---|---|---|
| `sample_offset` | `<u8` | First row in `raw_xyz` |
| `sample_count` | `<u4` | Number of rows |
| `boot_epoch` | `<u8` | Firmware boot epoch |
| `first_sample_seq` | `<u8` | Sequence of first row |
| `timing_segment_id` | `<u4` | Firmware timing segment |
| `source_packet_seq` | `<u4` | Transport diagnostic identity |
| `timing_format_version` | `u1` | Firmware timing payload version |
| `timestamp_source` | `u1` | Firmware timestamp source |
| `first_device_time_us` | `<u8` | First sample device time |
| `last_device_time_us` | `<u8` | Last sample device time |
| `sample_period_q16_us` | `<u4` | Firmware period estimate |
| `max_fit_residual_us` | `<u4` | Firmware fit residual bound |
| `first_utc_ns` | `<i8` | Estimated acquisition UTC or `-1` |
| `last_utc_ns` | `<i8` | Estimated acquisition UTC or `-1` |
| `uncertainty_ns` | `<u8` | Maximum half-width for the block |
| `timing_flags` | `<u2` | Firmware timing flags |
| `routing_flags` | `<u2` | Host timing/routing flags |

The write transaction is:

1. append raw XYZ;
2. flush;
3. append the block row;
4. flush.

On recovery, XYZ is truncated to the largest committed
`sample_offset + sample_count`. A block may not overlap another block in the
same file.

### 2.3 Sparse datasets

```text
/environment/temperature
/configuration/intervals
/quality/events
/diagnostics/clock_sync
```

`temperature` fields:

```text
boot_epoch:u8, sample_seq_anchor:u8, raw:u2, celsius:f4,
observed_utc_ns:i8
```

`configuration/intervals` starts a new row whenever measurement-affecting
configuration changes. It includes at minimum:

```text
start_boot_epoch, start_sample_seq, sensor_odr_hz, output_odr_hz,
range_g, high_pass_corner, offset_x, offset_y, offset_z,
decimation_factor, filter_profile, calibration_revision
```

`quality/events` stores measurement-relevant events using:

```text
event_id:u8, boot_epoch:u8, sample_seq_anchor:u8, observed_utc_ns:i8,
event_code:u2, severity:u1, flags:u4, count:u8, value_a:i8, value_b:i8
```

The event-code registry defines the meaning of `count`, `value_a`, and
`value_b`. Initial codes cover physical FIFO loss, recovery, timing
invalidation, configuration and calibration changes, host restart, and known
logical gaps. Operational messages and UI logs do not belong here.

`clock_sync` retains request/response observations and their accepted state for
the 30-day diagnostic period.

### 2.4 Capture routing flags

```text
0x0001 UTC_VALID
0x0002 BOUNDARY_UNCERTAIN
0x0004 INITIALLY_UNSYNCED
0x0008 LATE_ARRIVAL
0x0010 RECOVERED_AFTER_RESTART
```

Samples are stored exactly once. There are no separate `ambiguous`, `late`, or
`unsynced` copies. When the best UTC estimate falls inside a window, that window
owns the sample and the uncertainty is preserved as a flag and numeric bound.

Data without any usable UTC estimate remains in a session-scoped Capture file
until offline reconstruction can assign it. It is never silently discarded.

## 3. Archive HDF5 v1

### 3.1 Purpose and partitioning

Archive is the validated, minimal, long-term source record. The normal partition
is one sensor and one UTC day:

```text
archive/sensor-A/2026/08/
  sensor-A_2026-08-02.archive.h5
```

An exceptional session archive is allowed for data that cannot be assigned to
a UTC day. Such a file is explicitly marked `time_assignment=unresolved`.

### 3.2 Datasets

```text
/measurement/raw_xyz
/stream/segments
/stream/gaps
/timing/control_points
/quality/intervals
/environment/temperature
/configuration/intervals
/provenance/sources
```

`stream/segments` reconstructs sequence identity without repeating it per
sample:

| Field | Type |
|---|---|
| `sample_offset` | `<u8` |
| `sample_count` | `<u8` |
| `boot_epoch` | `<u8` |
| `first_sample_seq` | `<u8` |
| `timing_segment_id` | `<u4` |
| `configuration_index` | `<u4` |

Within a segment, `sample_seq = first_sample_seq + local_index`.

`stream/gaps` represents both sequence-visible and physical sensor loss:

```text
after_sample_offset:u8
boot_epoch:u8
expected_sample_seq:u8
received_sample_seq:u8
missing_sample_count:u8
missing_count_known:u1
gap_kind:u2
estimated_duration_ns:u8
```

A physical FIFO loss is recorded even when firmware output `sample_seq` remains
continuous.

`timing/control_points` contains only the points required to reconstruct time
within a declared error bound:

```text
sample_offset:u8
device_time_us:u8
utc_ns:i8
uncertainty_ns:u8
method:u2
flags:u2
```

Control points are selected adaptively. A fixed decimation such as “one point
per minute” is forbidden unless validation proves the configured maximum
reconstruction error for every interval.

`quality/intervals` is run-length encoded:

```text
sample_offset:u8, sample_count:u8, quality_flags:u4
```

Required Archive quality flags:

```text
0x00000001 TIME_UNCERTAIN
0x00000002 TIME_UNSYNCED
0x00000004 BOUNDARY_UNCERTAIN
0x00000008 SENSOR_LOSS_ADJACENT
0x00000010 SENSOR_RECOVERY
0x00000020 CONFIG_CHANGED
0x00000040 CALIBRATION_UNKNOWN
0x00000080 MEASUREMENT_INVALID
```

### 3.3 Provenance

`/provenance/sources` records every sealed Capture input using:

```text
capture_file_id
relative_name
sha256
sample_count_read
```

The Archive file also records compactor version, validation profile, conversion
time, and an Archive content summary. Packet sequence and raw clock exchanges
are not retained after successful compaction.

## 4. Validation contract

Archive publication requires all of the following:

1. Every committed Capture sample is classified exactly once.
2. Archived `raw_xyz` values are bit-identical and in the same stream order.
3. Segment ranges are ordered, non-overlapping, and cover every archived row.
4. Every discontinuity is either a new segment or an explicit gap.
5. Physical loss events are retained even without a sequence gap.
6. Every sample has a quality classification, including an explicit good
   interval where applicable.
7. Reconstructed control-point time stays within the declared error bound of
   retained Capture timing evidence.
8. Configuration and calibration are defined for every measurement row.
9. The completed file reopens and all HDF5 dataset checks pass.
10. A SHA-256 manifest is written after final publication.

Failure produces a rejected catalog record and preserves Capture inputs. It
never produces a nominally valid partial Archive.

## 5. Compression and checksums

Portable defaults are:

```text
compression = gzip
compression_level = 4
shuffle = true
fletcher32 = true
```

Initial chunk sizes are implementation defaults, not format requirements. They
must be selected by a benchmark covering 125 Hz and 250 Hz sensors, ten-minute
preview reads, full-day sequential reads, conversion time, and final size.

The external manifest uses SHA-256. HDF5 checksums detect chunk corruption;
SHA-256 identifies and verifies the published file as a whole.

### 5.1 Initial layout benchmark

A prototype using recent completed Capture files produced these results before
adding sparse blocks and quality metadata:

| Sensor | Samples | Existing v5 | Raw `int32` XYZ | `float32 m/s²` XYZ | Raw/float |
|---|---:|---:|---:|---:|---:|
| C | 150,031 | 1,623,138 B | 531,329 B | 1,119,250 B | 47.5% |
| D | 75,003 | 846,033 B | 264,060 B | 536,673 B | 49.2% |

Both datasets used identical gzip level 4, shuffle, Fletcher32, and chunking.
All tested legacy `float32` XYZ values round-tripped to integer sensor LSB and
back bit-identically. This is evidence for migration testing, not permission to
use floating point as the new source representation. The complete Archive size
must be benchmarked again after segments, timing, quality, and provenance are
implemented; the table must not be treated as a final storage promise.

## 6. Lifecycle and retention

The catalog state machine is:

```text
DISCOVERED → SEALED → CONVERTING → VALIDATED → REPLICATED
                                           ↘ REJECTED
REPLICATED → RETENTION_ELIGIBLE → CAPTURE_DELETED
```

Conversion is idempotent. Capture retention is 30 days, but deletion remains
manual until a storage backend and verified replication policy are deployed.
The converter never deletes inputs. A separate retention command may delete
only catalog entries explicitly marked `RETENTION_ELIGIBLE`.

## 7. Current timing accuracy

Timing uncertainty is stored per block/control point and is never embedded as a
schema constant. The approximately one-day test observed single-sensor UTC
uncertainty medians around `32.4–32.7 ms`, p95 around `114–117 ms`, and maxima
around `345–414 ms`. Hardware improvements reduce recorded values without a
format change.
