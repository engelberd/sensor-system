# TODO

Last updated: 2026-07-09

## Priority Now

- Investigate `INT1` behavior on `line-e`.
- Confirm whether `INT1` carries valid FIFO watermark interrupts or false edges.
- Decide whether production mode should use:
  - FIFO watermark IRQ on `INT1`
  - controlled FIFO polling

## Sensor / Firmware

- Run a short dedicated `INT1` debug test without normal FIFO servicing.
- Verify if `INT1` still fires while:
  - `STATUS_FIFO_FULL=0`
  - `FIFO_ENTRIES=0`
- Compare `line-e` against a known-good line on the same firmware version.
- Re-check `INT_MAP`, `FIFO_SAMPLES`, `STATUS`, and `FIFO_ENTRIES` behavior after longer runtime.
- Decide whether `INT1` should remain production-critical or debug-only.
- Review long-term counter design for 24/7 operation:
  - `uptime_ms` wraps after about 49.7 days
  - several diagnostic counters are still `uint32_t`
- Identify which counters should be upgraded to `uint64_t`.
- Consider adding explicit wrap-safe reporting for long-lived systems.

## Hardware / Electrical

- Verify physical mapping again:
  - `INT1 -> GP10`
  - `DRDY -> GP11`
- Check if `INT1` line has ringing, floating behavior, or injected noise.
- Confirm resistor / pull configuration on the PCB and sensor side.
- Compare behavior on alternate lines/boards with the same firmware.
- If possible, inspect `INT1` waveform with external measurement equipment.

## Host / Diagnostics

- Keep `dump-diagnostics` and recorder logs aligned with newest firmware payloads.
- Continue validating persistent diagnostic record parsing after format extensions.
- Decide whether some debug fields should be shown in dashboard / operator panel.
- Add a compact diagnostic summary command for quick field debugging.

## Reliability / Operations

- Define the final 24/7 recovery strategy:
  - when to soft-recover
  - when to restart node only
  - when to restart a whole acquisition pipeline
- Add policy for repeated `NoData` that preserves logs before recovery.
- Review safe update behavior while recorder/supervisor is active.
- Confirm the update guard covers all real production conflict cases.

## Documentation

- Write down the current interrupt model:
  - what uses `INT1`
  - what uses `DRDY`
  - when fallback is allowed
- Document sample sequence behavior and long-term limits.
- Document known limitations for long-running uptime and counters.
- Link detailed line-E investigation notes from project docs.

## Useful References

- Detailed line-E notes:
  - [diagnostics/2026-07-09_line-e_int1_fifo_debug.md](/home/anone/pico-projects/diagnostics/2026-07-09_line-e_int1_fifo_debug.md)
