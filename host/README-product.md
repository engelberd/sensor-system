# Sensor System Host Product Layout

The host stack is now split into product-oriented layers:

- `host_recorder.py`: production recorder entrypoint used for continuous data capture.
- `host_supervisor.py`: multi-channel production supervisor that runs one recorder worker per RS485 channel.
- `host_console.py`: operator-facing terminal console that reads recorder runtime status.
- `host_dashboard.py`: operator-facing web dashboard built on top of supervisor status, config, and event logs.
- `host_operator_panel.py`: operator-facing control panel for supervisor control, runs browsing, preview, and node config.
- `host/live_web.py`: minimal live web UI (time plot + FFT) served by the recorder for test/bring-up.
- `host/recorder/`: recorder package namespace for future modularization.
- `host/console/`: operator console package namespace.
- `host/dashboard/`: dashboard package namespace serving JSON endpoints and the web UI.
- `host/operator_panel/`: operator panel package namespace serving the control UI and APIs.
- `host/common/`: shared protocol and runtime helpers.
- `host/tools/bootloader_cli.py`: maintenance entrypoint for firmware update / bootloader workflows.

## Operational Model

- `host_recorder.py` owns RS485 communication, HDF5 writes, windowed file rotation, and sparse temperature polling for a single channel.
- `host_supervisor.py` owns multi-channel process lifecycle and publishes aggregated runtime status.
- `host_console.py` does not touch the serial bus. It only reads the recorder status JSON and event log.
- `host_dashboard.py` does not touch the serial bus either. It reads supervisor/runtime files and exposes them as a browser UI plus JSON endpoints.
- `host_operator_panel.py` is safe to keep running continuously. It reads status/config/runs by default and only attempts RS485 access for explicit device-config actions; those actions are blocked while the target channel is recording.
- Bootloader/update actions stay in the maintenance lane and should not be mixed into the recorder runtime.

## Runtime Files

Default runtime side-files written by the recorder:

- Status snapshot: `/tmp/sensor-system_recorder_status.json`
- Event log: `/tmp/sensor-system_recorder_events.jsonl`

Default runtime side-files written by the supervisor:

- Status and command files: `/run/sensor-system/`
- Supervisor event log: `logs/sensor-system/supervisor.events.jsonl`
- Per-channel event and process logs: `logs/sensor-system/<channel>.*.log`

Recorder output files are created per channel under `/data/sensor-system/<line>/<YYYY-MM-DD>/`
and use the exact recorder start timestamp in the filename, for example
`line-a_2026-06-15_16-33-48.h5`.

Production logs are bounded: JSONL logs rotate at 5 MiB with three backups, and
recorder process logs rotate at 10 MiB with three backups. Runtime status and
event-log failures are non-fatal to measurement recording. The supervisor also
uses capped exponential restart backoff for repeatedly failing channels and
publishes storage capacity so the dashboard can warn below 15% free space.

The console reads those files by default.
The dashboard reads the supervisor status/event files configured in `host/system_config.json`.

Recorder-side counters such as packet overwrite, RX overflow, and dropped samples
are treated as lifetime counters in the node. The recorder captures a baseline at
session start and the console shows session deltas first, with total values in
parentheses.

## Recommended Usage

Prepare the local host environment first:

```bash
python3 -m venv host/.venv
host/.venv/bin/python -m pip install -r host/requirements-recorder.txt
```

If this host is also your first-time firmware build machine, build the node
artifacts before provisioning a fresh board:

```bash
export PICO_SDK_PATH=$PWD/pico-sdk
cmake -S node -B node/build
cmake --build node/build
```

Convenience launcher from repo root:

```bash
./hostctl ping --port /dev/sensor-system-rs485
./hostctl config --port /dev/sensor-system-rs485 get-config
./hostctl config --port /dev/sensor-system-rs485 commission-scan
./hostctl config --port /dev/sensor-system-rs485 commission-assign --hardware-id 0123456789ABCDEF --node-id 7
./hostctl recorder --port /dev/sensor-system-rs485 --nodes 1,2 --output-dir runs/sensor-system --format hdf5 --window-seconds 600
./hostctl supervisor --config host/system_config.json
./hostctl console
./hostctl dashboard --config host/system_config.json --port 8080
./hostctl operator --config host/system_config.json --port 8090
./hostctl boot --port /dev/sensor-system-rs485 --node 1 --enter app hello
./hostctl update --port /dev/sensor-system-rs485 --node 1
```

Use the actual RS485 adapter path for the installation. USB numbering can change
after reflashing nodes or reconnecting adapters, so production hosts should
prefer stable udev aliases. For multi-channel systems, assign one stable alias
per adapter, for example `/dev/sensor-system-rs485-a` and `/dev/sensor-system-rs485-b`.

Confirmed `./hostctl config` mutation commands such as `set-odr`, `set-range`,
`set-high-pass`, `set-offsets`, `set-watermark`, `set-baudrate`, `load`, and
`reset-defaults` now also update `host/system_config.json` by default. Use
`--no-sync-system-config` to skip that host-side sync for a particular command.

Recorder:

```bash
host/.venv/bin/python host/host_recorder.py \
  --port /dev/sensor-system-rs485 \
  --nodes 1,2 \
  --output-dir runs/sensor-system \
  --format hdf5 \
  --window-seconds 600
```

Multi-channel supervisor:

```bash
host/.venv/bin/python host/host_supervisor.py --config host/system_config.json
```

Soak-test launcher with cleanup and `tmux` sessions:

```bash
host/tools/soak_control.sh start --purge-data
host/tools/soak_control.sh status
host/tools/soak_control.sh alerts
host/tools/soak_control.sh stop
```

The launcher enforces a single supervisor/watch pair, kills stale
`host_supervisor.py` and `host_recorder.py` processes before each start, clears
runtime logs by default, and optionally wipes `/data/sensor-system`.

Console:

```bash
host/.venv/bin/python host/host_console.py
```

Dashboard:

```bash
host/.venv/bin/python host/host_dashboard.py --config host/system_config.json --port 8080
```

Then open `http://<host-ip>:8080/` on the local network. The dashboard shows the
operator overview, channels, nodes, runtime files, recent events, data preview,
and guarded control actions. It exposes JSON endpoints under `/api/dashboard`,
`/api/channels`, `/api/events`, `/api/config`, and `/api/health`.

The dashboard and operator panel do not currently provide user authentication.
Bind them to `127.0.0.1` unless remote access is required, and expose them only
on a trusted, firewalled production network or through a protected VPN/reverse
proxy. Do not publish their ports directly to the Internet.

Operator panel:

```bash
host/.venv/bin/python host/host_operator_panel.py --config host/system_config.json --port 8090
```

Then open `http://<host-ip>:8090/`. The operator panel can stay online continuously
next to the recorder/supervisor; direct node config actions are rejected while a
channel is actively recording so the panel does not steal the serial port mid-run.

Bootloader maintenance:

```bash
host/.venv/bin/python host/tools/bootloader_cli.py --help
```

## Commissioning New Nodes

Fresh factory-programmed nodes now start as unassigned (`node_id=0`) and stay
out of normal runtime polling until they are commissioned.

Typical workflow:

```bash
./hostctl config --port /dev/sensor-system-rs485 commission-scan
./hostctl config --port /dev/sensor-system-rs485 commission-assign --hardware-id 0123456789ABCDEF --node-id 7
./hostctl config --port /dev/sensor-system-rs485 --node 7 get-config
```

`commission-assign` persists the new `node_id` immediately, so a separate
`save` step is not required for this path.

For a fresh board on a local development machine, the practical sequence is:

1. Flash `node/build/sensor-system-node-factory.uf2` over USB/BOOTSEL.
2. Connect the board to the RS485 bus.
3. Run `commission-scan`.
4. Assign the runtime `node_id`.
5. Verify with `get-config` and then `ping`.

## Bootloader Workflow

The recommended split is:

- initial board provisioning: flash `node/build/sensor-system-node-factory.uf2`
- bench reflashing of an already commissioned board over USB: `node/build/sensor-system-node.uf2`
- remote maintenance over RS485: use `./hostctl update`

The update command uploads `node/build/sensor-system-node-update-package.json`, which
contains both slot images and lets the host select the currently inactive slot.

After provisioning, the application should answer `ping`, while a bootloader
`hello` without entering bootloader mode should time out:

```bash
./hostctl ping --port /dev/sensor-system-rs485
./hostctl boot --port /dev/sensor-system-rs485 --node 1 --enter none hello
```

Useful commands:

```bash
./hostctl boot --port /dev/sensor-system-rs485 --node 1 --enter app hello
./hostctl update --port /dev/sensor-system-rs485 --node 1
./hostctl boot --port /dev/sensor-system-rs485 --node 1 abort
```

`hostctl update` uses auto-enter mode. It first checks whether the node is
already in bootloader recovery mode, which is expected after power loss during
an update; otherwise it asks the running application to reboot into the
bootloader. It also uses a longer per-request timeout by default because the
bootloader erases the inactive application slot before acknowledging `Begin`.
After `End` is accepted, the host waits for the node to reboot into the updated
application and verifies it with an application `Ping`. The update should be
treated as successful only after this post-update verification completes.

If power is lost before `End`, the partly written target slot is not treated as
valid. On the next boot, the bootloader opens a bounded recovery window so the
host can rerun `./hostctl update` and complete the upload. If no host resumes
the update, the bootloader clears the update request and boots the last
confirmed application slot.

Override timing when needed with:

```bash
SENSOR_SYSTEM_UPDATE_TIMEOUT=30 ./hostctl update --port /dev/sensor-system-rs485 --node 1
```

Additional update timing knobs:

```bash
SENSOR_SYSTEM_UPDATE_VERIFY_WAIT=15 \
SENSOR_SYSTEM_UPDATE_VERIFY_RETRIES=20 \
./hostctl update --port /dev/sensor-system-rs485 --node 1
```

For production hosts, create a stable udev alias for the RS485 adapter, for
example `/dev/sensor-system-rs485`, and use that path in recorder/update commands.
An example rule is provided in
`host/udev/99-sensor-system-rs485.rules.example`.

## Live Web UI (tests)

For quick live viewing on a local network, run the recorder with `--live`:

```bash
./hostctl recorder --port /dev/sensor-system-rs485 --nodes 1 --output-dir runs/sensor-system --format hdf5 --window-seconds 600 --live --live-port 8000
```

Then open `http://<host-ip>:8000/` from another machine on the same network.
The page shows a time-domain plot and a live PSD (FFT) view.

There is a dedicated firmware-side note in
[README-bootloader.md](../node/README-bootloader.md).
