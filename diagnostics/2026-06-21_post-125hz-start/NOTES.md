# Post-Change Start Snapshot 2026-06-21

Start snapshot time: `2026-06-21T15:05:08+02:00`

What was done:

- preserved pre-change runtime state in `diagnostics/2026-06-21_pre-125hz-runtime/`
- updated `host/system_config.json` so every channel targets `sensor_odr_hz=250` and `expected_odr_hz=125.0`
- stopped the active `adxl355-supervisor.service` to release RS485 ports
- sent `set-odr 250` successfully to:
  - `line-a` `/dev/ttyCH9344USB0`
  - `line-b` `/dev/ttyCH9344USB1`
  - `line-c` `/dev/ttyCH9344USB2`
  - `line-d` `/dev/ttyCH9344USB3`
  - `line-e` `/dev/ttyCH9344USB4`
  - `line-f` `/dev/ttyCH9344USB5`
  - `line-h` `/dev/ttyCH9344USB7`
- `line-g` `/dev/ttyCH9344USB6` did not answer `get-status` or `set-odr`
- restarted the runtime with `host/tools/soak_control.sh start`, which cleared runtime logs before start

## Immediate Verification

- `line-a`: `online=True`, `sensor_odr=250`, `output_odr=125.0`, `gaps=0`, `packet_overwrite_session=0`
- `line-b`: `online=True`, `sensor_odr=250`, `output_odr=125.0`, `gaps=0`, `packet_overwrite_session=0`
- `line-c`: `online=True`, `sensor_odr=250`, `output_odr=125.0`, `gaps=0`, `packet_overwrite_session=0`
- `line-d`: `online=True`, `sensor_odr=250`, `output_odr=125.0`, `gaps=0`, `packet_overwrite_session=0`
- `line-e`: `online=True`, `sensor_odr=250`, `output_odr=125.0`, `gaps=0`, `packet_overwrite_session=0`
- `line-f`: `online=True`, `sensor_odr=250`, `output_odr=125.0`, `gaps=0`, `packet_overwrite_session=0`
- `line-h`: `online=True`, `sensor_odr=250`, `output_odr=125.0`, `gaps=0`, `packet_overwrite_session=0`
- `line-g`: `online=False`, stale runtime still reports `sensor_odr=500`, `output_odr=250.0`

## Line G Exception

Current `line-g` process log:

```text
[ERROR] node 1: no GetConfig response
[ERROR] node 1: no GetConfig response
[ERROR] node 1: no GetConfig response
[ERROR] node 1: no GetConfig response
```

Supervisor event after restart:

- `line-g` started and then exited with `exit_code=1` at `2026-06-21T13:05:10.601259+00:00`

## Snapshot Files

- `diagnostics/2026-06-21_post-125hz-start/sensor-system_channels/`
- `diagnostics/2026-06-21_post-125hz-start/sensor-system_supervisor_status.json`
- `diagnostics/2026-06-21_post-125hz-start/sensor-system_supervisor_events.jsonl`

## Comparison Plan

After a few days, compare against this start snapshot using:

- `gaps_detected`
- `packet_overwrite_session`
- `bursts_failed`
- `instant_samples_per_second_5s`
- any repeated `line-g` failure or recovery
