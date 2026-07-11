# TODO

Last updated: 2026-07-11

## Priority Now

- Investigate the electrical/SPI cause of the 21 confirmed lost samples on `line-g`.
- Add a multi-record flash diagnostic journal that survives power loss, has wear limits,
  and can be acknowledged record-by-record only after durable host archival.
- Surface durable diagnostic events and plain-language loss alarms in the dashboard.
- Investigate `INT1` behavior on `line-e`.
- Confirm whether `INT1` carries valid FIFO watermark interrupts or false edges.
- Validate firmware `0.3.8` rejection of false `INT1` edges on line E.
- Confirm that fallback counters increase without FIFO overrun when INT1 is disconnected.

## Sensor / Firmware

- Run a short dedicated `INT1` debug test without normal FIFO servicing.
- Verify if `INT1` still fires while:
  - `STATUS_FIFO_FULL=0`
  - `FIFO_ENTRIES=0`
- Compare `line-e` against a known-good line on the same firmware version.
- Re-check `INT_MAP`, `FIFO_SAMPLES`, `STATUS`, and `FIFO_ENTRIES` behavior after longer runtime.
- Decide whether `INT1` should remain production-critical after the protected fallback test.
- Review long-term counter design for 24/7 operation:
  - `uptime_ms` wraps after about 49.7 days
  - several diagnostic counters are still `uint32_t`
- Identify which counters should be upgraded to `uint64_t`.
- Consider adding explicit wrap-safe reporting for long-lived systems.
- Add a hardware-in-the-loop test for partial SPI transfer and FIFO axis mismatch.

## Hardware / Electrical

- Verify physical mapping again:
  - `INT1 -> GP10`
  - `DRDY -> GP11`
- Check if `INT1` line has ringing, floating behavior, or injected noise.
- Check whether the extra edge appears immediately after FIFO service and INT1 deassertion.
- Confirm resistor / pull configuration on the PCB and sensor side.
- Compare behavior on alternate lines/boards with the same firmware.
- If possible, inspect `INT1` waveform with external measurement equipment.

## Host / Diagnostics

- Keep `dump-diagnostics` and recorder logs aligned with newest firmware payloads.
- Preserve the recorder's durable `diagnostics/<channel>.events.jsonl` files during data retention and export.
- Add `boot_id` to diagnostic event identity so host restarts can deduplicate archived node events safely.
- Surface `spurious_int1`, `fifo_overruns`, `fifo_discarded`, and `fifo_loss_unknown` in the dashboard.
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
