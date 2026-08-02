# Sensor System Diagnostics

Raw diagnostic captures are operational data and are intentionally ignored by
Git. Store a temporary investigation under `var/diagnostics/<date>-<incident>/`
or another external data volume. When an incident is closed, keep only the
reusable conclusion or procedure in documentation and remove the raw logs.

This project uses a layered diagnostics model for Pico nodes on RS485:

- cheap counters for steady-state health
- a small on-device black-box event buffer
- a sticky incident snapshot for the first meaningful fault after boot or clear
- one persistent incident record in flash for post-restart access
- host-side JSONL/runtime status files for long retention and operator visibility

## Goals

- distinguish symptom from cause, especially for `NoData`
- preserve the first useful signal before later noise overwrites it
- keep firmware diagnostics cheap enough for real-time acquisition
- make host-side collection structured and easy to correlate

## What To Check And When

### Every acquisition loop

These checks must stay cheap and deterministic:

- interrupt seen for FIFO watermark path
- ADXL355 `STATUS` read result
- FIFO entry count
- FIFO read result
- sample decode validity
- sample sequence progress

What to record:

- counters always
- event only on first occurrence, severity change, recovery, or power-of-two streak

### At boot and init

Check:

- reset cause
- sensor init
- device identity (`DEVID_AD`, `DEVID_MST`, `PARTID`)
- applied ODR/range/high-pass/watermark
- controller init
- RS485 init
- transport init

What to record:

- `Boot`
- init failures as `Error`/`Critical`
- config/application milestones as `Info`

### On config change

Check immediately after apply:

- device accepted new config
- acquisition recovered
- expected output ODR matches host assumptions

What to record:

- `RuntimeConfigApplied`
- `RuntimeConfigApplyFailed`

### On first anomaly

The first anomaly is where root cause usually appears. Capture:

- sticky fault snapshot
- current counters
- last `STATUS` register
- last FIFO entry count
- last FIFO read result
- FIFO decode flags
- whether the loop was triggered by IRQ

### On repeated anomaly

Do not log every repeat. Use counters plus power-of-two escalation:

- 1, 2, 4, 8, 16, 32...

This keeps signal while preventing the ring buffer from being flooded.

### On recovery

Always record recovery after a streak of `NoData` or sensor errors:

- how long the problem lasted
- what streak counts were cleared

Recovery events are often as useful as failure events because they show the boundary of the bad window.

## What To Store On Pico

Do not store full text logs or every successful operation.

Store locally:

- counters
- recent warning/error events
- selected `Info` milestones: boot, config applied, recovery, diagnostics cleared
- one sticky incident snapshot
- one persistent flash record saved on the first significant incident per boot

Do not store locally:

- every normal sample
- every repeated identical warning
- large text messages

The persistent flash record is intended for the case where RS485 is lost and the node later reboots.
It should contain the last important pre-restart context, not a full event history.

## `NoData` Decision Tree

`NoData` is only a symptom. To debug it, check these fields together:

- IRQ seen?
- `STATUS` register value
- FIFO entries before read
- FIFO read result (`Ok`, `NoData`, `CommError`, `InvalidSample`, ...)
- FIFO decode flags:
  - empty entry seen
  - axis mismatch seen
- time since last good sample
- sensor error streak

Interpretation:

- IRQ missing: likely interrupt/wiring/config path
- IRQ seen + FIFO entries low: sensor not producing data or watermark not reached
- IRQ seen + FIFO entries present + `NoData`: FIFO payload/driver interpretation problem
- `CommError`: SPI path
- `InvalidSample`: framing/decode/order issue

## Event Argument Conventions

Diagnostic `arg0`/`arg1` are event-specific and should be decoded, not treated as generic numbers.

Current firmware packs driver context for acquisition faults as:

- `arg0` byte 0: last ADXL355 `STATUS`
- `arg0` byte 1: last FIFO entry count
- `arg0` byte 2: last FIFO read status (`SensorStatus`)
- `arg0` byte 3:
  - bit 7: IRQ seen
  - bit 0: empty FIFO entry observed
  - bit 1: axis mismatch observed

Examples:

- `FifoRepeatedNoData`: `arg1` = current no-data streak
- `FifoOverrun`: `arg1` = dropped-sample counter at the time of the event
- `FifoStatusReadError`, `SensorReadError`, `InvalidSample`:
  - `arg1` byte 0 = `SensorStatus`
  - `arg1` bytes 1..3 = error streak

## Host Responsibilities

The host should keep doing the heavy lifting:

- persist runtime status snapshots
- persist JSONL event streams
- fetch node diagnostics over RS485 when an anomaly appears
- correlate node diagnostics with recorder/supervisor warnings

Recommended polling cadence:

- status/runtime metrics: every 1s
- extended stats: every 5s
- diagnostic dump: on node init, on warning/error transition, on exit/restart, and after manual repro

## Exceptions And `catch`

For firmware on Pico:

- do not rely on broad `try/catch`
- use explicit return-code checks around SPI, sensor config, RS485, transport, and storage boundaries
- convert failures immediately into structured diagnostic events

For host Python:

- narrow `except` blocks are appropriate at I/O boundaries
- convert exceptions into structured JSONL events
- keep one broad top-level `except` only for process-level reporting and exit code control

## Practical Workflow

For a suspect node:

1. Reproduce the issue while the recorder or supervisor is running.
2. Read node status and diagnostic info.
3. Read the sticky fault snapshot.
4. Drain diagnostic events.
5. Correlate with host `events.jsonl`, `status.json`, and `process.log`.

Useful commands:

```bash
host/.venv/bin/python host/host_configurator.py --port /dev/ttyCH9344USB2 --baud 115200 --node 1 get-status
host/.venv/bin/python host/host_configurator.py --port /dev/ttyCH9344USB2 --baud 115200 --node 1 get-diagnostic-info
host/.venv/bin/python host/host_configurator.py --port /dev/ttyCH9344USB2 --baud 115200 --node 1 get-fault-snapshot
host/.venv/bin/python host/host_configurator.py --port /dev/ttyCH9344USB2 --baud 115200 --node 1 get-persistent-diagnostic-record
host/.venv/bin/python host/host_configurator.py --port /dev/ttyCH9344USB2 --baud 115200 --node 1 dump-diagnostics
host/.venv/bin/python host/host_configurator.py --port /dev/ttyCH9344USB2 --baud 115200 --node 1 clear-persistent-diagnostic-record
```

If the first question is "SPI, memory, or something else?", the best first discriminator is:

- `FifoStatusReadError` or `SensorReadError` => likely SPI/driver path
- `FifoRepeatedNoData` with IRQ seen and low FIFO entries => likely sensor/config/timing path
- `InvalidSample` => likely FIFO decode/order/pathology in sample handling
- host-only failures with healthy node diagnostics => transport or host path
