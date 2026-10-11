# USB observer protocol

The node exposes a read-only, best-effort observer over its Pico USB CDC port.
RS485 remains the production command and recording transport. USB requests do
not grant, commit, pause, configure, reset, or otherwise change node state.

## Framing

USB reuses the version 2 sensor-system frame envelope (`0xAA55`, little
endian, CRC-16/CCITT). A request is a command frame whose payload is exactly
one byte. The endpoint accepts only these observer-local commands:

| Command | Value | Response |
| --- | ---: | --- |
| Get capabilities | `0x01` | Identity and observer limits |
| Read snapshot | `0x02` | Latest coherent sample batch |
| Get stats | `0x03` | Observer publication counters |

Every response starts with command and status bytes. Observer payload format
version is currently `1`. Multi-byte fields are little endian.

## Snapshot response

The 24-byte response header is followed by zero or more 36-byte sample
records. A frame contains at most 27 samples because the common frame payload
limit is 1024 bytes. When the internal 32-sample snapshot is larger, the 27
newest records are returned and flag bit 0 is set.

Snapshot header:

| Field | Size |
| --- | ---: |
| command, status, format version, flags | 4 bytes |
| generation | 4 bytes |
| source sample count | 2 bytes |
| returned sample count | 2 bytes |
| first source index | 2 bytes |
| sample record size | 2 bytes |
| published batch count | 4 bytes |
| producer-truncated batch count | 4 bytes |

Each sample record contains `sample_seq` (u64), XYZ (three i32 values),
`device_time_us` (u64), timing segment (u32), timing-quality flags (u16),
timestamp source (u8), and one reserved byte.

Generation gaps are expected when USB polls more slowly than acquisition. They
mean observer snapshots were superseded; they do not indicate loss in the
RS485 production queue.

## Runtime bounds

- acquisition publishes to a fixed triple buffer and never waits for USB;
- USB processes at most 32 RX bytes and offers at most 64 TX bytes per core-1
  loop iteration;
- disconnect or CDC backpressure discards observer progress only;
- Pico SDK USB background processing and USB-triggered reset/BOOTSEL are
  disabled; TinyUSB is serviced explicitly from core 1.

## Bench use

After flashing the validated board profile, find the Pico CDC port and run:

```sh
host/.venv/bin/python host/host_usb_observer.py \
  --port /dev/cu.usbmodemXXXX capabilities

host/.venv/bin/python host/host_usb_observer.py \
  --port /dev/cu.usbmodemXXXX snapshot

host/.venv/bin/python host/host_usb_observer.py \
  --port /dev/cu.usbmodemXXXX stats
```

For the acceptance test, keep normal RS485 recording active while repeatedly
reading USB snapshots. Compare RS485 sequence continuity and timing diagnostics
before, during, and after USB polling. Use the debug probe only to measure
core-0 service timing or investigate a fault; it is not part of normal USB
operation.
