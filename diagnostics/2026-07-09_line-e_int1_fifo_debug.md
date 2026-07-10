# Line E INT1/FIFO Debug Notes

Date: 2026-07-09
Channel: `line-e`
Port during tests: `/dev/ttyCH9344USB4`

## Current Findings

- Recorder path works correctly at the data level.
- Recent runs show about `125 samples/s`, `gaps=0`, `sensor_loss=0`.
- `sample_seq` handling is healthy and not the current issue.
- `DRDY` was disabled as an active acquisition source.
- Firmware `0.3.8` validates INT1 against `STATUS` and `FIFO_ENTRIES` before touching FIFO.
- Firmware `0.3.8` rejects false INT1 edges without delaying fallback.
- Partial FIFO reads preserve valid prefix samples and explicitly count discarded data.
- `INT1` is active and generates many edges/events.
- A large fraction of `INT1`-triggered service attempts still return `NoData`.

## Strongest Evidence So Far

- New debug counters show:
  - `poll=0`
  - `drdy=0`
  - `int1` continues to rise
  - `nd_irq` rises roughly in proportion to half of `int1`
- New IRQ debug snapshot observed in recorder log:
  - `irqdbg=0x0C1E0000`
- Current interpretation of `irqdbg=0x0C1E0000`:
  - `STATUS = 0x00`
  - `FIFO_ENTRIES = 0`
  - `watermark = 30`
  - flags = `entries_lt3`, `entries_lt_watermark`

## What This Likely Means

- The node is not failing to measure.
- The FIFO data path can work correctly enough to sustain normal recording.
- The remaining problem appears to be on the `INT1` side:
  - false edges
  - wrong physical signal observed on the MCU pin
  - electrical ringing/noise
  - unexpected board-level routing/buffering
  - less likely: wrong interpretation of ADXL355 interrupt behavior

## What The ADXL355 Docs Suggest

- With `INT_MAP = 0x06`, `INT1` should represent only:
  - `FIFO_FULL`
  - `FIFO_OVR`
- It should not represent:
  - `DATA_RDY`
  - `ACTIVITY`
- Therefore repeated `INT1` events with `STATUS=0x00` and `FIFO_ENTRIES=0` do not match expected FIFO watermark behavior.
- `FIFO_SAMPLES=30` means 30 axis entries, equivalent to 10 complete XYZ samples.
- At 250 Hz the expected watermark time is 40 ms and the full 96-entry FIFO time is 128 ms.

## Changes Already Made

- `0.3.6`
  - moved away from servicing FIFO on `DRDY`
  - added richer host/node diagnostics
- `0.3.7`
  - disabled `DRDY` as active path
  - made `consume_data_ready_event_sources()` atomic
  - allowed partial FIFO read success
  - changed fallback timing to depend on watermark/ODR
  - added IRQ-specific debug counters and snapshots
- `0.3.8`
  - corrected fallback timing to use three FIFO entries per XYZ sample
  - bounded fallback before the FIFO capacity deadline at every supported ODR
  - rejected INT1 when status and entry count do not confirm FIFO data
  - delayed IRQ arming until sensor configuration is complete
  - preserved valid prefix samples while reporting discarded or uncertain data loss
  - added saturating diagnostic counters and explicit loss telemetry

## Things To Check Next

- Run a dedicated `INT1` debug mode:
  - count `INT1`
  - read `STATUS` and `FIFO_ENTRIES`
  - do not service FIFO normally for a short controlled test
- Verify whether `INT1` still toggles while:
  - `STATUS_FIFO_FULL=0`
  - `FIFO_ENTRIES=0`
- Confirm physical wiring again:
  - `INT1 -> GP10`
  - `DRDY -> GP11`
- Check if there is any board-level source of pulses on the `INT1` trace:
  - pull resistor choice
  - trace length/noise
  - adapter/buffer behavior
  - shared line or wrong header pin
- Compare line E behavior against another known-good line on the same firmware.
- If possible, inspect the actual `INT1` waveform on hardware.

## Long-Term Reliability Notes

- `sample_seq` is `uint64_t` and is safe for many years of 24/7 operation.
- RS485 payloads carry sequence values as fixed-size binary `uint64_t`.
- Separate concern for 24/7:
  - some status/diagnostic counters are still `uint32_t`
  - `uptime_ms` will wrap after about 49.7 days

## Practical Decision Paths

### Path A: Keep investigating `INT1`

- Best if we want FIFO watermark IRQ as the final production mechanism.
- Requires proving whether `INT1` is electrically trustworthy.

### Path B: Treat `INT1` as unreliable on this line

- Use controlled FIFO polling as primary acquisition trigger.
- Keep `INT1` only as debug/telemetry.
- This may be the most robust operational path if hardware is the culprit.

## Useful Commands

```bash
./hostctl config --port /dev/ttyCH9344USB4 --baud 115200 --node 1 get-status
./hostctl config --port /dev/ttyCH9344USB4 --baud 115200 --node 1 dump-diagnostics
```
