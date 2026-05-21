# Sensor System Deployment Checklist (Host + Node)

This is a practical pre-flight list for deploying a node where you will **not**
have physical USB access after installation (e.g. the node is mounted high up).

It assumes initial provisioning was already done locally with
`node/build/sensor-system-node-factory.uf2`, and that the deployed host machine
will own RS485 communication plus future remote updates.

## 1) Node (firmware) must-haves

- **Remote update over RS485 works end-to-end**:
  - Run `./hostctl update --port ... --node <id>` twice to prove A/B alternation.
  - Simulate an interrupted update (power loss mid-upload), then confirm the
    bootloader recovery window allows completing the update and otherwise rolls
    back to the last confirmed slot.
- **Watchdog enabled**: the application enables a 3 s watchdog and feeds it in
  the core0 acquisition loop (`node/main.cpp`).
- **Boot success confirmation**: the app calls `boot::app_confirm_boot_success()`
  after startup so the bootloader can mark the slot as confirmed (`node/main.cpp`).
- **Persistent config is correct**: verify `node_id`, ODR, range, watermark, and
  offsets via `./hostctl config ... get-config` and save what you need before
  installation.
- **Fresh nodes are commissioned before install**: factory images now start as
  unassigned (`node_id=0`). Use `./hostctl config --port ... commission-scan`
  and `commission-assign` to give each new node a unique runtime address.

## 2) Host (computer) must-haves

- **Stable serial device path**: install a udev rule so the RS485 adapter appears
  as a stable symlink (example in `host/udev/99-sensor-system-rs485.rules.example`).
- **Auto-start on boot**: run either the single-channel recorder or the
  multi-channel supervisor under `systemd` with restart-on-failure. The repo
  includes example units for both:
  `host/systemd/sensor-system-recorder.service.example` and
  `host/systemd/sensor-system-supervisor.service.example`.
- **Operator page always available**: for supervisor-based deployments, also run
  `host/systemd/sensor-system-operator-panel.service.example` under `systemd`. The panel
  is safe to keep online continuously because it only reads runtime files until an
  operator explicitly opens device config, and those config actions are blocked
  while a channel is recording.
- **Disk/mount ready**:
  - Ensure the target directory exists and is writable (e.g. `/data/sensor-system`).
  - Ensure there is enough free space for continuous capture.
- **Time sync**: enable NTP/chrony/systemd-timesyncd; host timestamps are written
  into the output files.

For a development laptop or bench machine that is temporarily acting as the
host, the minimum local setup is:

- `python3 -m venv host/.venv`
- `host/.venv/bin/python -m pip install -r host/requirements-recorder.txt`
- use `./hostctl ping`, `./hostctl config`, and `./hostctl update` from the repo root

## 3) One-time setup steps (typical Linux host)

1. Create Python venv and install dependencies:
   - `python3 -m venv host/.venv`
   - `host/.venv/bin/python -m pip install -r host/requirements-recorder.txt`
2. Install udev rule (edit VID/PID/serial first on the target host):
   - copy `host/udev/99-sensor-system-rs485.rules.example` to `/etc/udev/rules.d/99-sensor-system-rs485.rules`
   - reload rules and replug the adapter (or `udevadm trigger`)
3. Create the output directory (example):
   - `sudo mkdir -p /data/sensor-system && sudo chown -R $USER:$USER /data/sensor-system`
4. Install the recording service:
   - copy `host/systemd/sensor-system-recorder.service.example` to `/etc/systemd/system/sensor-system-recorder.service`
   - edit paths/args
   - `sudo systemctl daemon-reload`
   - `sudo systemctl enable --now sensor-system-recorder.service`
   - for multi-channel deployments, prefer `host/systemd/sensor-system-supervisor.service.example`
5. For multi-channel deployments, install the always-on operator panel too:
   - copy `host/systemd/sensor-system-operator-panel.service.example` to `/etc/systemd/system/sensor-system-operator-panel.service`
   - edit paths/args if needed
   - `sudo systemctl daemon-reload`
   - `sudo systemctl enable --now sensor-system-supervisor.service sensor-system-operator-panel.service`

## 4) Suggested pre-flight tests before final install

- 24–72 hour soak run on the same host hardware:
  - verify window rotation (`600` seconds by default, `86400` for daily if configured)
  - verify file boundaries match the configured local timezone if you pass `--window-timezone`
  - verify `host_console.py` stays responsive (status + events update)
- Reboot host during recording and confirm it restarts and continues with
  `--start-from newest`.
- Power-cycle the node and confirm it comes back and answers `./hostctl ping`.
- Confirm `./hostctl update --port ... --node ...` still works from the final host.
