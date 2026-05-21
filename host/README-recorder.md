# Sensor System RS485 Recorder

`host_recorder.py` is the single-channel production-oriented host receiver. It
talks to nodes over one RS485 channel, drains burst packets, commits received
sample ranges, and writes decoded XYZ samples to disk.

For multi-channel deployments use `host_supervisor.py`, which runs one recorder
worker per RS485 channel from `host/system_config.json`.

## Install HDF5 Dependencies

```bash
python3 -m venv host/.venv
host/.venv/bin/python -m pip install -r host/requirements-recorder.txt
```

If you are using a fresh local machine, create the virtual environment first and
then use the `./hostctl` launcher from the repository root for day-to-day work.

## Record To HDF5

```bash
host/.venv/bin/python host/host_recorder.py \
  --port /dev/sensor-system-rs485 \
  --nodes 1 \
  --output runs/run_001.h5 \
  --format hdf5
```

Use `--duration 60` for a fixed-length one-minute capture. Without `--duration`,
recording runs until `Ctrl+C`.

## Record To Rotating HDF5 Files

```bash
host/.venv/bin/python host/host_recorder.py \
  --port /dev/sensor-system-rs485 \
  --nodes 1,2 \
  --output-dir runs/sensor-system \
  --format hdf5 \
  --window-seconds 600 \
  --window-timezone Europe/Warsaw
```

This creates one file per 10-minute local-time window. For example:

- `runs/sensor-system/2026-04-24/2026-04-24_14-20.h5`
- `runs/sensor-system/2026-04-24/2026-04-24_14-30.h5`

If the recorder is restarted inside the same window, it appends to the existing
file for that window.

Each new rotated file starts with one temperature sample per node. That sample
is anchored to the first expected `sample_seq` in the new window, which makes
later joins and summaries simpler.

`--temperature-interval 0` now disables only the extra periodic reads. The
window-start temperature sample is still kept for each rotated file.

`--output-dir` now defaults to `--window-seconds 600` even without an explicit
flag. Pass `--window-timezone` to align file boundaries to a named local zone.

## Record To Daily HDF5 Files

```bash
host/.venv/bin/python host/host_recorder.py \
  --port /dev/sensor-system-rs485 \
  --nodes 1,2 \
  --output-dir runs/archive \
  --format hdf5 \
  --window-seconds 86400 \
  --window-timezone Europe/Warsaw
```

This creates one file per local day, for example `2026-04-24.h5`.

## Operator Runtime Files

The recorder also writes:

- a JSON runtime status snapshot, default `/tmp/sensor-system_recorder_status.json`
- a JSONL event log, default `/tmp/sensor-system_recorder_events.jsonl`

`recorder_stopped` now includes `stop_reason` and signal fields when shutdown was
triggered by `SIGINT` or `SIGTERM`.

Those files are intended for `host_console.py` and other local supervisory tools.

## Live Web UI

For test/bring-up you can enable a minimal live web UI (time plot + PSD/FFT) that
streams the same sample fields as HDF5 over HTTP/SSE:

```bash
./hostctl recorder --port /dev/sensor-system-rs485 --nodes 1 --output-dir runs/sensor-system --format hdf5 --window-seconds 600 --live --live-port 8000
```

Open `http://<host-ip>:8000/`.

## Gaps And Resets

`sample_seq` is a firmware-side sequence. It can contain gaps after events such
as node resets, host restarts, power loss, or when starting from `--start-from newest`.

For HDF5 output the recorder writes gap markers to `/nodes/<node_id>/gaps`.
For CSV output it writes `<output>.gaps.csv`.

## HDF5 Layout

- `/nodes/<node_id>/samples`: append-only compound dataset.
- `/nodes/<node_id>/temperature`: sparse host-side temperature reads.
- `/nodes/<node_id>/gaps`: records detected discontinuities in `sample_seq`.
- `sample_seq`: output sample sequence after firmware-side filtering and x2 decimation.
- `x`, `y`, `z`: acceleration samples stored in `m/s^2`.
- `packet_seq`: firmware packet sequence.
- `sample_seq_anchor` in `temperature`: host-side anchor for the reading.
  Periodic reads use the newest committed/written sample sequence, while the
  first temperature sample in a rotated file uses the first expected sequence
  for that file window.

Node metadata such as `sensor_odr_hz`, `output_odr_hz`, `range_g`, and
`fifo_watermark` is stored as attributes on `/nodes/<node_id>`.

## CSV Fallback

```bash
host/.venv/bin/python host/host_recorder.py \
  --port /dev/sensor-system-rs485 \
  --nodes 1 \
  --output runs/run_001.csv \
  --format csv
```

CSV metadata is written to a sidecar file named `run_001.csv.meta.json`.

For day-to-day use from the repository root, the shorter launcher form is:

```bash
./hostctl recorder --port /dev/sensor-system-rs485 --nodes 1,2 --output-dir runs/sensor-system --format hdf5 --window-seconds 600
./hostctl console
```

Use the RS485 adapter path that exists on the host. In production prefer a
stable udev alias such as `/dev/sensor-system-rs485`.

For first-time node provisioning and later firmware updates, see:

- [node/README-bootloader.md](/home/anone/pico-projects/node/README-bootloader.md:1)
- [host/README-product.md](/home/anone/pico-projects/host/README-product.md:1)
