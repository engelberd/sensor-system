# Sensor System Node Bootloader And Remote Update

This firmware now builds as a real A/B update system intended for RS485-only maintenance.

## Flash Layout

The Pico 2 flash is split into:

- `0x00000000 .. 0x0001ffff` bootloader (`128 kB`)
- `0x00020000 .. 0x0003ffff` metadata/config (`128 kB`)
- `0x00040000 .. 0x0021ffff` application slot A (`1.875 MB`)
- `0x00220000 .. 0x003fffff` application slot B (`1.875 MB`)

The authoritative constants are in [boot/boot_config.h](/home/anone/pico-projects/node/boot/boot_config.h:1).

## Build Outputs

After:

```bash
cmake -S node -B node/build
cmake --build node/build
```

the important artifacts are:

- `node/build/sensor-system-node-factory.uf2`
  Full factory image for initial provisioning over USB/BOOTSEL.
- `node/build/sensor-system-node.uf2`
  General full-image UF2. For a brand-new node, prefer the factory image above.
- `node/build/sensor-system-node-factory.bin`
  Raw flash image containing bootloader, initial metadata, and slot A app.
- `node/build/sensor-system-node.bin`
  General full-image raw binary variant.
- `node/build/sensor-system-node-bootloader.uf2`
  Bootloader-only image for development.
- `node/build/sensor-system-node-slot-a.bin`
- `node/build/sensor-system-node-slot-b.bin`
- `node/build/sensor-system-node-update-package.json`
  Manifest used by the host update tool. It points at the correct slot-specific image and carries version/CRC metadata.
- `node/build/sensor-system-node-direct.uf2`
  Diagnostic standalone application image without the A/B bootloader. Use it only
  to isolate bootloader issues during bench work.

## Local Build On A New Computer

For a fresh local machine where the node firmware will be built outside the
Raspberry Pi host:

```bash
git clone https://github.com/raspberrypi/pico-sdk.git
export PICO_SDK_PATH=$PWD/pico-sdk
cmake -S node -B node/build
cmake --build node/build
```

On macOS, if CMake does not auto-detect the ARM toolchain correctly, configure
with explicit compiler paths:

```bash
cmake -S node -B node/build \
  -DCMAKE_C_COMPILER=/opt/homebrew/bin/arm-none-eabi-gcc \
  -DCMAKE_CXX_COMPILER=/opt/homebrew/bin/arm-none-eabi-g++ \
  -DCMAKE_ASM_COMPILER=/opt/homebrew/bin/arm-none-eabi-gcc
cmake --build node/build
```

If you see a TinyUSB submodule warning during configuration, it is acceptable
for current RS485-only builds and does not block the firmware artifacts above.

## Recommended Provisioning

For a brand-new node or a board recovered locally:

1. Put the Pico 2 into BOOTSEL mode.
2. Flash `node/build/sensor-system-node-factory.uf2`.
3. Reboot normally.
4. Commission a runtime `node_id` over RS485:

```bash
./hostctl config --port /dev/sensor-system-rs485 commission-scan
./hostctl config --port /dev/sensor-system-rs485 commission-assign --hardware-id 0123456789ABCDEF --node-id 1
./hostctl config --port /dev/sensor-system-rs485 --node 1 get-config
```

Fresh factory images now start as unassigned (`node_id=0`), so they will not
answer normal `ping` or update commands until commissioning assigns a real
runtime address.

Use the actual RS485 adapter path on the host. USB numbering can change after
reflashing or reconnecting adapters, so production hosts should prefer a stable
udev alias such as `/dev/sensor-system-rs485`.

The expected post-provisioning check is:

```bash
./hostctl ping --port /dev/sensor-system-rs485 --node 1
./hostctl boot --port /dev/sensor-system-rs485 --node 1 --enter none hello
```

`ping` should answer. `boot ... --enter none hello` should time out because the
node should be running the application, not sitting in bootloader update mode.

## Remote Update Over RS485

For field updates, use the packaged manifest instead of a raw `.bin`:

```bash
./hostctl update --port /dev/sensor-system-rs485 --node 1
```

The host uses a longer timeout for update operations because the bootloader
erases the inactive slot before acknowledging `Begin`. For slow flash or noisy
links, override it explicitly:

```bash
SENSOR_SYSTEM_UPDATE_TIMEOUT=30 ./hostctl update --port /dev/sensor-system-rs485 --node 1
```

This does:

- asks the running app to reboot into bootloader
- probes bootloader `HELLO`
- learns which inactive slot should receive the update
- selects the matching image from `sensor-system-node-update-package.json`
- uploads the image
- leaves metadata armed for a trial boot into the new slot

You can also use the low-level maintenance tool directly:

```bash
./hostctl boot --port /dev/sensor-system-rs485 --node 1 --enter app hello
./hostctl boot --port /dev/sensor-system-rs485 --node 1 --enter app \
  --image node/build/sensor-system-node-update-package.json upload
```

## Reliability Model

The update flow is A/B with rollback-oriented metadata:

- the currently known-good image remains in `active_slot`
- the new image is written to the other slot
- boot metadata arms a trial boot into the new slot
- if the new image confirms itself, it becomes the new `active_slot`
- if it fails to come up cleanly, bootloader rolls back to the previously active slot

The decision logic lives in:

- [boot/boot_decision.cpp](/home/anone/pico-projects/node/boot/boot_decision.cpp:1)
- [boot/boot_update_engine.cpp](/home/anone/pico-projects/node/boot/boot_update_engine.cpp:1)
- [boot/boot_update_server.cpp](/home/anone/pico-projects/node/boot/boot_update_server.cpp:1)

## Boot Handoff

On RP2350 the slot application is linked at its slot address, for example
`0x10040000` for slot A. The bootloader validates the vector table and then uses
a controlled vector-table chainload:

- deinitializes `uart0`, which bootloader maintenance/update mode used
- stops SysTick
- disables and clears pending NVIC IRQs
- restores a plain privileged MSP context
- sets `VTOR` and `MSP`
- branches to the slot reset handler

This path is deliberately kept in firmware instead of using `rom_chain_image()`,
because the current slot images boot reliably through the vector path and are
managed by our own A/B metadata.

## LED Status

The Pico LED is an operational readiness indicator:

- three short blinks after reset: application startup began
- LED off: initialization is still in progress or failed
- LED on: application, RS485 transport, core1 loop, and ADXL355 driver init all completed

The LED on state is therefore the local "app ready with sensor accepted" signal.

## Hardware Acceptance

Minimum acceptance after changing bootloader or application startup:

1. Flash `sensor-system-node-factory.uf2` for a fresh board, or `sensor-system-node.uf2` for bench reflashing.
2. Confirm LED turns on after startup.
3. Confirm `./hostctl ping --port ...` succeeds.
4. Confirm `./hostctl boot --port ... --node ... --enter none hello` times out.
5. Run `./hostctl update --port ... --node ...`.
6. Confirm the node reboots back into the application and still answers `ping`.
7. Repeat once more to prove A/B alternation.
