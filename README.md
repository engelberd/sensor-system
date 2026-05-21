# sensor-system

This repository contains the full Sensor System project:

- `node/` for the Pico firmware and bootloader artifacts
- `host/` for recorder, configurator, supervisor, dashboards, and deployment files
- `runs/` for local acquisition output that stays untracked

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
cmake -S node -B node/build \
  -DCMAKE_C_COMPILER=/opt/homebrew/bin/arm-none-eabi-gcc \
  -DCMAKE_CXX_COMPILER=/opt/homebrew/bin/arm-none-eabi-g++ \
  -DCMAKE_ASM_COMPILER=/opt/homebrew/bin/arm-none-eabi-gcc
cmake --build node/build
```

The most important output files are:

- `node/build/sensor-system-node-factory.uf2` for first-time USB/BOOTSEL flashing
- `node/build/sensor-system-node.uf2` as the general full-image UF2
- `node/build/sensor-system-node-update-package.json` for later RS485 updates

Further details live in:

- [node/README-bootloader.md](/home/anone/pico-projects/node/README-bootloader.md:1)
- [host/README-product.md](/home/anone/pico-projects/host/README-product.md:1)
- [host/README-deploy.md](/home/anone/pico-projects/host/README-deploy.md:1)
