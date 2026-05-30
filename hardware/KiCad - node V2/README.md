# RP2350 ADXL355 node hardware notes

Working notes for the first custom RP2350A board with ADXL355, USB-C
development power/data, 12 V field input, RS485, and a power architecture
biased toward a clean accelerometer supply.

## Design goals

- Keep firmware changes small by staying close to Raspberry Pi Pico 2/RP2350A.
- Give ADXL355 the quietest practical supply and layout environment.
- Allow development from USB-C without a 12 V supply.
- Allow normal field operation from 12 V.
- Prevent backfeeding between USB VBUS and the 12 V/buck input.
- Keep the schematic easy to manufacture externally, even if components are small.

## Proposed power tree

```text
12V_IN
  -> input fuse/PTC
  -> TVS to GND
  -> reverse polarity protection
  -> TPS62175 buck, 12 V to 5V_BUCK

USB-C VBUS
  -> optional fuse/filter
  -> TPS2121 IN2

5V_BUCK
  -> TPS2121 IN1

TPS2121 OUT = 5V_SYS
  -> TLV75733 -> 3V3_DIG
  -> low-noise LDO -> 3V3_ADXL_PRE
      -> ferrite / 0R option -> 3V3_ADXL
```

Recommended source priority:

- `IN1`: `5V_BUCK` from 12 V, main/field source.
- `IN2`: `USB_5V`, fallback/development source.
- `OUT`: `5V_SYS`.

This means that when both 12 V and USB-C are connected, the board should run
from 12 V while USB is used for data/debug. Exact TPS2121 priority and threshold
resistors must be set from the datasheet during schematic entry.

## Candidate BOM and KiCad mapping

| Block | Candidate part | KiCad symbol | KiCad footprint |
|---|---|---|---|
| MCU | RP2350A | `MCU_RaspberryPi:RP2350A` | `Package_DFN_QFN:QFN-60-1EP_7x7mm_P0.4mm_EP3.4x3.4mm_ThermalVias` |
| QSPI flash | W25Q128JVS, 16 MiB, SOIC-8 | `Memory_Flash:W25Q128JVS` if available, otherwise import/vendor symbol | `Package_SO:SOIC-8_3.9x4.9mm_P1.27mm` |
| Accelerometer | ADXL355BEZ-RL7, LCSC C515892 | local `ADXL355BEZ` | local `E-14-1_ADI` |
| Input PTC | PTS181212V150 | `Device:Fuse` / `Device:Polyfuse` | likely `Fuse:Fuse_1812_4532Metric` or vendor footprint |
| Input TVS | SM6T12A | `Diode:TVS` | usually SMB/SMA package; verify exact package before layout |
| Reverse protection | SS14-E3/61T, LCSC C47460 | `Diode:Schottky` | usually `Diode_SMD:D_SMA` / verify exact package |
| Buck 12 V to 5 V | TPS62175DQCR, LCSC C32097 | `Regulator_Switching:TPS62175DQC` | use TI DQC/VSON-10 footprint; verify against datasheet |
| Power mux | TPS2121RUXR, LCSC C485916 | local `TPS2121RUXR` | local `RUX0012A` |
| 3V3_DIG LDO | TLV75733 | `Regulator_Linear:TLV75733PDBV` or package-specific variant | `Package_TO_SOT_SMD:SOT-23-5` for DBV variant |
| 3V3_DIG alternative | AP2112K-3.3 | `Regulator_Linear:AP2112K-3.3` | `Package_TO_SOT_SMD:SOT-23-5` |
| 3V3_ADXL LDO | TPS7A2033 or ADP150-3.3 | `Regulator_Linear:TPS7A20xxxDBV` or imported ADP150 | `Package_TO_SOT_SMD:SOT-23-5` or exact vendor package |
| ADXL supply filter | MPZ2012S601AT000, 600R @ 100 MHz, 0805 | `Device:Ferrite_Bead` | `Inductor_SMD:L_0805_2012Metric` |
| RS485 TVS | CDSOT23-SM712 | `Diode:SM712_SOT23` | `Package_TO_SOT_SMD:SOT-23` |
| RS485 transceiver | THVD1450DR | `Interface_UART:THVD1450DR` | `Package_SO:SOIC-8_3.9x4.9mm_P1.27mm` |
| USB ESD | USBLC6-2SC6 or TPD2EUSB30 | `Power_Protection:USBLC6-2SC6` / `Power_Protection:TPD2EUSB30` | package-specific SOT-23-6 / X2SON, verify selected part |
| USB-C connector | select exact vertical USB-C part | `Connector:USB_C_Receptacle_USB2.0_16P` | select exact manufacturer footprint |

Local downloaded libraries already found:

```text
ADXL355 symbol:
/Users/anone/Downloads/ul_ADXL355BEZ/KiCADv6/2026-05-26_12-26-55.kicad_sym

ADXL355 footprints:
/Users/anone/Downloads/ul_ADXL355BEZ/KiCADv6/footprints.pretty/E-14-1_ADI.kicad_mod
/Users/anone/Downloads/ul_ADXL355BEZ/KiCADv6/footprints.pretty/E-14-1_ADI-L.kicad_mod
/Users/anone/Downloads/ul_ADXL355BEZ/KiCADv6/footprints.pretty/E-14-1_ADI-M.kicad_mod

TPS2121 symbol:
/Users/anone/Downloads/ul_TPS2121RUXR/KiCADv6/2026-05-26_13-55-57.kicad_sym

TPS2121 footprint:
/Users/anone/Downloads/ul_TPS2121RUXR/KiCADv6/footprints.pretty/RUX0012A.kicad_mod
```

## Input power schematic notes

Initial 12 V input chain:

```text
J_PWR.1 = +12V_IN_RAW
J_PWR.2 = GND

+12V_IN_RAW -> F1 PTC -> +12V_PROT
+12V_PROT -> D_TVS -> GND
+12V_PROT -> D_REV Schottky series -> +12V_SAFE
+12V_SAFE -> TPS62175 VIN
```

Notes:

- `PTS181212V150` is acceptable for a controlled 12 V lab input, but consider a
  higher voltage-rated fuse/PTC for field or industrial-style inputs.
- `SM6T12A` is aggressive for nominal 12 V. It is fine for a well-controlled
  12 V adapter, but for systems that can sit above 12 V, consider a 15 V or
  18 V working-voltage TVS while keeping the TPS62175 28 V input limit safe.
- `SS14` as a series reverse-protection diode is simple and acceptable for V1.
  A P-MOS/ideal-diode style reverse protector would reduce voltage drop and heat.

## Buck notes: TPS62175DQCR

Target output: `5V_BUCK`.

Required schematic items:

- VIN input ceramic capacitor close to VIN/GND.
- Additional optional bulk capacitor footprint near the protected 12 V input.
- Inductor from SW to `5V_BUCK`, selected from TPS62175 datasheet guidance.
- Output capacitor close to inductor/output return.
- Feedback divider for 5 V output.
- EN tied to `+12V_SAFE` or configured with UVLO if desired.
- PG routed to a testpoint or left according to datasheet recommendation.
- Testpoint on `5V_BUCK`.

Layout priority:

- Keep the buck hot loop tiny.
- Keep SW copper small and away from ADXL355.
- Place buck, inductor, and high-current input/output caps away from the
  accelerometer side of the board.

## USB-C and power mux notes

USB-C development connector:

```text
VBUS -> USB_5V
GND  -> GND
CC1  -> 5.1k -> GND
CC2  -> 5.1k -> GND
D+   -> USB ESD -> RP2350 USB_DP
D-   -> USB ESD -> RP2350 USB_DM
```

Power mux:

```text
TPS2121 IN1 <- 5V_BUCK
TPS2121 IN2 <- USB_5V
TPS2121 OUT -> 5V_SYS
TPS2121 GND -> GND
```

Do not leave TPS2121 control pins floating. Configure:

- priority / mode selection,
- input overvoltage thresholds,
- current limit,
- status output if useful.

Exact resistor values should be calculated directly from the TPS2121 datasheet.

## 3.3 V rails

Digital rail:

```text
5V_SYS -> TLV75733 -> 3V3_DIG
```

`3V3_DIG` powers:

- RP2350A I/O and support circuitry,
- W25Q128JVS flash,
- RS485 transceiver logic supply,
- status LED,
- digital pull-ups and test circuitry.

Analog/accelerometer rail:

```text
5V_SYS -> low-noise LDO -> 3V3_ADXL_PRE
3V3_ADXL_PRE -> MPZ2012S601AT000 / 0R option -> 3V3_ADXL
3V3_ADXL -> ADXL355 VDD and VDDIO
```

Recommendation:

- Use TLV75733 for `3V3_DIG`.
- Use a low-noise LDO such as TPS7A2033 or ADP150-3.3 for `3V3_ADXL`.
- Keep the ferrite footprint, but make it replaceable by 0R in case the filter
  resonates with ceramic capacitors or is unnecessary.

## RP2350 and firmware-friendly pin plan

Keep the first custom board close to the current firmware pinout.

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

This minimizes firmware changes during bring-up.

## ADXL355 layout notes

- Place ADXL355 away from the buck inductor, SW node, USB connector, and RS485
  cable connector.
- Do not route fast digital traces under the sensor.
- Keep SPI traces short and calm; avoid unnecessarily high drive strength.
- Put local decoupling very close to ADXL355 supply pins.
- Keep a continuous ground plane under and around the sensor area.
- Consider mechanical placement early: the accelerometer measures the board and
  mounting structure, not just the electrical design.

## RS485 notes

Basic RS485 block:

```text
3V3_DIG -> THVD1450DR VCC
RP2350 UART_TX -> D
RP2350 UART_RX <- R
RP2350 GPIO_DE -> DE and /RE control as desired
A/B -> connector
A/B -> CDSOT23-SM712 -> GND
Optional 120R termination across A/B via jumper or DNP resistor
Optional failsafe bias resistor footprints if needed
```

Use `3V3_DIG`, not `3V3_ADXL`, for RS485.

## Items to verify before PCB release

- RP2350A footprint and exposed pad dimensions against the current Raspberry Pi
  hardware design files.
- ADXL355 land pattern against Analog Devices package drawing.
- Exact USB-C vertical connector part and matching footprint.
- TPS62175 DQC footprint and feedback/inductor/capacitor values.
- TPS2121 resistor configuration for priority, input thresholds, and current limit.
- Flash footprint/package for the selected W25Q128JVS ordering suffix.
- TVS working voltage against real 12 V input tolerance.
- Whether the field input should tolerate 24 V mistakes or only regulated 12 V.
