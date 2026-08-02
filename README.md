# sensor-system

This repository contains the full Sensor System project:

- `hardware/` for KiCad PCB projects, parts, footprints, and hardware notes
- `node/` for the Pico firmware and bootloader artifacts
- `host/` for recorder, configurator, supervisor, dashboards, and deployment files

Current product version: **0.4.1**.

For day-to-day use, see the Polish
[operator guide](docs/INSTRUKCJA-OPERATORA.md). For a new host computer, run:

```bash
./host/tools/setup_host.sh
```

The generated, ignored `host/system_config.json` extends the tracked
`host/configs/host_system.base.json`. Keep shared channel and sensor defaults in
the base file; keep the host name, device ports, storage overrides and disabled
channels in the local overlay. Validate an installation with:

```bash
./hostctl doctor --config host/system_config.json
```

Release validation and publication are described in
[RELEASING.md](RELEASING.md), and notable changes are listed in
[CHANGELOG.md](CHANGELOG.md).

Local runtime output has one predictable layout:

- `var/recordings/` for new measurements
- `var/archive/` for compacted archives
- `var/log/` for host logs and events
- `var/tmp/` for temporary artifacts
- `/run/sensor-system/` for locks, commands and live status

These directories are not versioned. Use `./hostctl paths` to display the
effective paths for the active host configuration.

Every installed node has an immutable hardware revision. Revision `2` is the
currently installed wiring (SCK 14, MOSI 15, DRDY 11, INT1 10, no DE GPIO),
while revision `1` uses SCK 10, MOSI 11, DRDY 14, INT1 15 and DE on GPIO2.
Node IDs and sensor settings remain runtime configuration independent of that
revision.

## Repository Notes

In this Codex workspace the top-level `.git/` path is reserved by the
environment, so repository metadata lives in `.git-main/` and the local wrapper
`./sgit` should be used instead of plain `git`.

Suggested day-to-day workflow in this workspace:

1. Make changes locally in this repository.
2. Use `./sgit ...` for repository commands in this environment.
3. Commit before flashing or deploying to another device.
4. Push to your remote.
5. On the Raspberry Pi host, run `git pull`, build from `node/`, and flash the selected artifact.

## Local Bring-Up On A New Computer

If you are building and flashing from a local Linux or macOS machine instead of
the Raspberry Pi host:

1. Clone the repository and enter it.
2. Install the Pico SDK and toolchain.
3. Build `node/`.
4. Flash `node/build/sensor-system-node-factory.uf2` onto a brand-new Pico 2.
5. Create `host/.venv` and install host requirements.
6. Use `./hostctl config` to commission the node over RS485.

Example setup for a clean local checkout:

```bash
git clone git@github.com:engelberd/sensor-system.git
cd sensor-system
git clone https://github.com/raspberrypi/pico-sdk.git
export PICO_SDK_PATH=$PWD/pico-sdk
python3 -m venv host/.venv
host/.venv/bin/python -m pip install -r host/requirements-recorder.txt
```

On macOS with Homebrew, the typical firmware toolchain is:

```bash
brew install cmake picotool arm-none-eabi-gcc
```

Build the node firmware with explicit compiler paths when needed:

```bash
cmake -S node --preset board-v2 \
  -DCMAKE_C_COMPILER=/opt/homebrew/bin/arm-none-eabi-gcc \
  -DCMAKE_CXX_COMPILER=/opt/homebrew/bin/arm-none-eabi-g++ \
  -DCMAKE_ASM_COMPILER=/opt/homebrew/bin/arm-none-eabi-gcc
cmake --build node/build
```

For a V1 board use `cmake -S node --preset board-v1` followed by
`cmake --build node/build.v1`. Never reuse one build directory for both board
revisions.

The most important output files are:

- `node/build/sensor-system-node-factory.uf2` for first-time USB/BOOTSEL flashing
- `node/build/sensor-system-node.uf2` as the general full-image UF2
- `node/build/sensor-system-node-update-package.json` for later RS485 updates

Further details live in:

- [hardware/README.md](hardware/README.md)
- [node/README-bootloader.md](node/README-bootloader.md)
- [host/README-product.md](host/README-product.md)
- [host/README-deploy.md](host/README-deploy.md)
