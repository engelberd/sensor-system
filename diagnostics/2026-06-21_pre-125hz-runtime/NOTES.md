# Diagnostics Snapshot 2026-06-21

Snapshot time: `2026-06-21T13:16:55+02:00`

Source snapshot copies:

- `diagnostics/2026-06-21_pre-125hz-runtime/sensor-system_channels/`
- `diagnostics/2026-06-21_pre-125hz-runtime/sensor-system_supervisor_status.json`
- `diagnostics/2026-06-21_pre-125hz-runtime/sensor-system_supervisor_events.jsonl`

Goal of this snapshot:

- preserve the pre-change runtime state before switching every channel to `output_odr=125 Hz`
- preserve raw per-channel status and event logs before log rotation / cleanup

## Current Summary

- All channels have been running for about `2d 13h 9m`.
- `line-a` is the main outlier: very high `gaps_detected` and very high session `packet_overwrite`.
- `line-g` is currently offline and appears stale; its active file is still on `2026-06-19`.
- `line-f` reports `last_temperature_c=233.29`, which looks implausible and should be treated as suspect telemetry.
- Runtime/config mismatch was present before the change: `line-e` and `line-h` were configured in `host/system_config.json` for `125 Hz` output but were still reporting `250 Hz` output at runtime.

## Per-Channel State

### `line-a`

- Sensor: `Node 1`
- Port: `/dev/ttyCH9344USB0`
- Measurement window: `2026-06-18T22:07:09.912777+00:00` -> `2026-06-21T11:16:55.247267+00:00`
- Runtime: `2d 13h 9m 45s`
- State: `online`
- ODR: `sensor=500 Hz`, `output=250 Hz`
- Samples written: `53,782,816`
- Gaps detected: `13,811`
- Packet overwrite: `session=59,895`, `total=59,978`
- RX overflow: `session=0`, `total=0`
- Sensor loss: `session=0`, `total=0`
- Bursts: `ok=745,399`, `failed=19,253`
- Instant rate 5s: `250.76 sps`
- Rate stability 5s: `99.01%`
- Last temperature: `29.31 C`
- Active file: `/home/anone/pico-projects/data/line-a/2026-06-21/line-a_2026-06-21_13-00-00.h5`
- Assessment: severe host-side backlog / overwrite behavior; highest-priority comparison line after ODR change.

### `line-b`

- Sensor: `Node 1`
- Port: `/dev/ttyCH9344USB1`
- Measurement window: `2026-06-18T22:07:09.943445+00:00` -> `2026-06-21T11:16:55.134890+00:00`
- Runtime: `2d 13h 9m 45s`
- State: `online`
- ODR: `sensor=500 Hz`, `output=250 Hz`
- Samples written: `55,680,448`
- Gaps detected: `8`
- Packet overwrite: `session=0`, `total=84`
- RX overflow: `session=0`, `total=0`
- Sensor loss: `session=0`, `total=0`
- Bursts: `ok=778,818`, `failed=8`
- Instant rate 5s: `253.21 sps`
- Rate stability 5s: `99.03%`
- Last temperature: `27.32 C`
- Active file: `/home/anone/pico-projects/data/line-b/2026-06-21/line-b_2026-06-21_13-00-00.h5`
- Assessment: healthy baseline.

### `line-c`

- Sensor: `Node 1`
- Port: `/dev/ttyCH9344USB2`
- Measurement window: `2026-06-18T22:07:10.012147+00:00` -> `2026-06-21T11:16:54.694772+00:00`
- Runtime: `2d 13h 9m 44s`
- State: `online`
- ODR: `sensor=500 Hz`, `output=250 Hz`
- Samples written: `55,092,800`
- Gaps detected: `562`
- Packet overwrite: `session=0`, `total=10,532`
- RX overflow: `session=0`, `total=0`
- Sensor loss: `session=0`, `total=0`
- Bursts: `ok=780,881`, `failed=181`
- Instant rate 5s: `247.29 sps`
- Rate stability 5s: `96.33%`
- Last temperature: `28.98 C`
- Active file: `/home/anone/pico-projects/data/line-c/2026-06-21/line-c_2026-06-21_13-00-00.h5`
- Assessment: moderate gap behavior with historical overwrite total, but no current session overwrite.

### `line-d`

- Sensor: `Node 1`
- Port: `/dev/ttyCH9344USB3`
- Measurement window: `2026-06-18T22:07:09.974128+00:00` -> `2026-06-21T11:16:55.200758+00:00`
- Runtime: `2d 13h 9m 45s`
- State: `online`
- ODR: `sensor=500 Hz`, `output=250 Hz`
- Samples written: `55,432,608`
- Gaps detected: `31`
- Packet overwrite: `session=0`, `total=83`
- RX overflow: `session=0`, `total=0`
- Sensor loss: `session=0`, `total=0`
- Bursts: `ok=779,793`, `failed=11`
- Instant rate 5s: `247.99 sps`
- Rate stability 5s: `96.27%`
- Last temperature: `29.42 C`
- Active file: `/home/anone/pico-projects/data/line-d/2026-06-21/line-d_2026-06-21_13-00-00.h5`
- Assessment: mostly healthy; low gap count.

### `line-e`

- Sensor: `Node 1`
- Port: `/dev/ttyCH9344USB4`
- Measurement window: `2026-06-18T22:07:09.992429+00:00` -> `2026-06-21T11:16:55.589008+00:00`
- Runtime: `2d 13h 9m 45s`
- State: `online`
- ODR: `sensor=500 Hz`, `output=250 Hz`
- Samples written: `55,217,568`
- Gaps detected: `5`
- Packet overwrite: `session=0`, `total=81`
- RX overflow: `session=0`, `total=0`
- Sensor loss: `session=0`, `total=0`
- Bursts: `ok=780,668`, `failed=5`
- Instant rate 5s: `253.23 sps`
- Rate stability 5s: `94.78%`
- Last temperature: `29.97 C`
- Active file: `/home/anone/pico-projects/data/line-e/2026-06-21/line-e_2026-06-21_13-00-00.h5`
- Assessment: healthy runtime, but runtime ODR did not match config before restart.

### `line-f`

- Sensor: `Node 1`
- Port: `/dev/ttyCH9344USB5`
- Measurement window: `2026-06-18T22:07:09.739345+00:00` -> `2026-06-21T11:16:54.853041+00:00`
- Runtime: `2d 13h 9m 45s`
- State: `online`
- ODR: `sensor=500 Hz`, `output=250 Hz`
- Samples written: `2,391,616`
- Gaps detected: `0`
- Packet overwrite: `session=0`, `total=68`
- RX overflow: `session=0`, `total=0`
- Sensor loss: `session=0`, `total=0`
- Bursts: `ok=33,617`, `failed=140`
- Instant rate 5s: `0.00 sps`
- Rate stability 5s: `100.00%`
- Last temperature: `233.29 C`
- Active file: `/home/anone/pico-projects/data/line-f/2026-06-21/line-f_2026-06-21_13-00-00.h5`
- Assessment: channel appears abnormal or mostly idle; temperature reading is not believable and needs follow-up.

### `line-g`

- Sensor: `Node 1`
- Port: `/dev/ttyCH9344USB6`
- Measurement window: `2026-06-18T22:07:10.010229+00:00` -> `2026-06-21T11:16:55.477854+00:00`
- Runtime: `2d 13h 9m 45s`
- State: `offline`
- ODR: `sensor=500 Hz`, `output=250 Hz`
- Samples written: `312,480`
- Gaps detected: `0`
- Packet overwrite: `session=0`, `total=0`
- RX overflow: `session=0`, `total=0`
- Sensor loss: `session=0`, `total=0`
- Bursts: `ok=4,392`, `failed=196,635`
- Instant rate 5s: `0.00 sps`
- Rate stability 5s: `100.00%`
- Last temperature: `26.22 C`
- Active file: `/home/anone/pico-projects/data/line-g/2026-06-19/line-g_2026-06-19_03-11-00.h5`
- Assessment: channel offline / stale before change; should be watched closely after restart.

### `line-h`

- Sensor: `Node 1`
- Port: `/dev/ttyCH9344USB7`
- Measurement window: `2026-06-18T22:07:10.037478+00:00` -> `2026-06-21T11:16:54.762997+00:00`
- Runtime: `2d 13h 9m 44s`
- State: `online`
- ODR: `sensor=500 Hz`, `output=250 Hz`
- Samples written: `55,059,872`
- Gaps detected: `60`
- Packet overwrite: `session=0`, `total=2,359`
- RX overflow: `session=0`, `total=0`
- Sensor loss: `session=0`, `total=0`
- Bursts: `ok=781,149`, `failed=16`
- Instant rate 5s: `249.60 sps`
- Rate stability 5s: `96.29%`
- Last temperature: `26.99 C`
- Active file: `/home/anone/pico-projects/data/line-h/2026-06-21/line-h_2026-06-21_13-00-00.h5`
- Assessment: low gap count, but runtime ODR did not match config before restart.

## Planned Change After This Snapshot

- set every line to `sensor_odr_hz=250`
- expect runtime `output_odr_hz=125.0`
- restart all lines together
- clear runtime logs for a clean post-change comparison window
