# Hardware design notes

## Current direction

Custom RP2350A node with:

- ADXL355 accelerometer over SPI
- RS485 field bus
- USB-C development power/data
- 12 V field input
- 12 V to 5 V buck, followed by 5 V source muxing and 3.3 V rails

## Power tree

```text
12V_IN
  -> fuse/PTC
  -> TVS
  -> reverse protection
  -> TPS62175 buck -> 5V_BUCK

USB_C_VBUS -> USB_5V

5V_BUCK -> TPS2121 IN1
USB_5V  -> TPS2121 IN2
TPS2121 OUT -> 5V_SYS

5V_SYS -> TLV75733 -> 3V3_DIG
5V_SYS -> low-noise LDO -> 3V3_ADXL_PRE -> ferrite/0R -> 3V3_ADXL
```

Preferred TPS2121 behavior: 12 V/buck source has priority when both field power
and USB are connected. USB remains useful for data/debug.

## Firmware-friendly pin plan

ADXL355 SPI:

```text
SCK  -> GPIO10
MOSI -> GPIO11
MISO -> GPIO12
CS   -> GPIO13
DRDY -> GPIO14
INT1 -> GPIO15
```

RS485:

```text
UART TX -> GPIO0
UART RX -> GPIO1
DE      -> GPIO2
```

## Layout priorities

- Put ADXL355 away from the buck inductor, SW node, USB connector, and RS485
  connector.
- Do not route fast digital traces under the accelerometer.
- Keep a continuous ground plane around the sensor area.
- Keep buck hot loops very small.
- Keep SW copper small and away from the analog/sensor side.
- Put local decoupling close to every power pin, especially ADXL355 and RP2350.
- Put RS485 TVS near the cable connector.

## Release checks

- RP2350A footprint verified against current Raspberry Pi hardware files.
- ADXL355 footprint verified against Analog Devices package drawing.
- USB-C connector footprint verified against manufacturer drawing.
- TPS62175 feedback, inductor, input/output caps calculated from datasheet.
- TPS2121 priority, threshold, and current-limit resistors calculated.
- Flash ordering suffix, symbol, and footprint match.
- 12 V TVS working voltage matches real supply tolerance.
- PCB manufacturer stackup and minimum rules entered into KiCad.
