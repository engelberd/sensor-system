# JLCPCB parts shortlist

Checked on 2026-05-30. JLCPCB/LCSC stock changes often, so re-check every row
right before ordering or uploading the BOM/CPL.

Goal: every assembled part should have a concrete JLCPCB/LCSC part number and a
matching KiCad footprint before PCB release.

## Critical ICs and special parts

| Block | Candidate part | JLCPCB/LCSC code | JLCPCB status seen | KiCad footprint | Decision |
|---|---|---:|---|---|---|
| MCU | Raspberry Pi `RP2350A` | `C42411118` | In stock | `Package_DFN_QFN:QFN-60-1EP_7x7mm_P0.4mm_EP3.4x3.4mm` | Use this JLCPCB/LCSC part, not the unavailable `C9900206085` assembly placeholder; verify footprint against Raspberry Pi package drawing. |
| Accelerometer | Analog Devices `ADXL355BEZ-RL7` | `C515892` | In stock | `my_footprints:E-14-1_ADI-M` currently placed | Use only after package/land-pattern review; high-cost part. |
| QSPI flash | Winbond `W25Q128JVSIM` | `C97521` | In stock | `Package_SO:SOIC-8_5.3x5.3mm_P1.27mm` | Good match for current schematic value `W25Q128JVS`. |
| Power mux | TI `TPS2121RUXR` | `C485916` | In stock | `my_footprints:RUX0012A` | Good candidate; calculate resistor configuration before release. |
| Buck regulator | TI `TPS62175DQCR` | `C32097` | In stock | `Package_SON:WSON-10-1EP_2x3mm_P0.5mm_EP0.84x2.4mm_ThermalVias` | Good candidate; verify DQC/WSON footprint and datasheet values. |
| Digital LDO | TI `TLV75733PDBVR` | `C485517` | In stock | `Package_TO_SOT_SMD:SOT-23-5` | Good 3V3 digital rail candidate. |
| Analog LDO | TI `TPS7A2033PDBVR` | `C2862740` | In stock | `Package_TO_SOT_SMD:SOT-23-5` | Good low-noise ADXL rail candidate; verify pinout with symbol. |
| RS485 transceiver | TI `THVD1450DR` | `C2671361` | In stock | `Package_SO:SOIC-8_3.9x4.9mm_P1.27mm` | Good match for current footprint. |
| USB ESD | ST `USBLC6-2SC6` | `C7519` | In stock | `Package_TO_SOT_SMD:SOT-23-6` | Good match for current schematic. |
| RS485 TVS | Bourns `CDSOT23-SM712` | `C404012` | In stock | `Package_TO_SOT_SMD:SOT-23` | Preferred known-brand part; cheaper GOODWORK `C21713983` also exists in stock. |
| 12 V TVS | `SMBJ14A` | `C353392` | In stock | `Diode_SMD:D_SMB` | Good candidate if 14 V working standoff fits real input tolerance. |
| Reverse input diode | MDD `SS14` | `C2480` | In stock, Basic | Current schematic uses `Diode_SMD:D_SMA` | Package is SMA/DO-214AC, matching the current footprint family. |
| Input polyfuse | Eaton `PTS181212V150` | `C3760264` | In stock, low stock seen | `Fuse:Fuse_1812_4532Metric` | Current candidate matches the 1812 footprint; verify 12 V/1.5 A is enough. |
| USB-C connector | G-Switch `GT-USB-7051A` | `C2843970` | In stock | `my_footprints:USB_C_Receptacle_G-Switch_GT-USB-7051x` | KiCad library footprint explicitly references this LCSC/JLC part; assembly difficulty is high. |
| RP2350 core inductor | Abracon `AOTA-B201610S3R3-101-T` or equivalent 3.3 uH | TBD | TBD | Current schematic `L2` has no footprint | Needed for RP2350 1V1 supply; choose part from Raspberry Pi guidance or JLC equivalent. |
| Buck inductor | 10 uH shielded power inductor | TBD | TBD | Current schematic `L1` has no footprint | Pick from TPS62175 datasheet/current/ripple requirements. |
| ADXL ferrite | 600R @ 100 MHz 0805 bead | TBD | TBD | `Inductor_SMD:L_0805_2012Metric` | Pick exact bead or use 0R option after review. |

## Passives

Passives should be chosen after the core topology is fixed. Prefer JLCPCB basic
parts where electrical requirements allow it.

| Group | Values in schematic | Footprints | Status |
|---|---|---|---|
| Decoupling caps | `100nF`, `1uF`, `2.2uF`, `4.7uF`, `10uF`, `22uF`, `10nF`, `1nF/2kV` | mostly `C_0603`, `C_0805`, one `C_1206` | Need exact voltage ratings and JLC codes. |
| Resistors | `0`, `27ohm`, `33ohm`, `120ohm`, `1k`, `2.2k`, `5.1k`, `10k`, `47k`, `80k`, `100k`, `523k`, `1M` | `R_0201`, `R_0402`, `R_0603`, `R_0805` | Prefer 0603 unless layout/USB demands smaller; assign JLC codes in BOM pass. |
| LED | status LED | `LED_0603_1608Metric` | Pick color/current and matching resistor. |
| Test points | multiple rails | mixed/no footprint | Need assembly/no-assembly decision; usually not JLC-assembled. |

## Footprint work to do in KiCad

- Assign footprints for schematic parts still missing footprints:
  `J3`, `R2`, `R3`, `R13`, `R14`, `R18`, `R19`, `L1`, `L2`, `JP1`, test points.
- Decide whether to keep all small passives as placed or standardize most
  resistors/caps to 0603 for easier assembly and rework.
- Verify `ADXL355BEZ-RL7` footprint choice: current board places
  `my_footprints:E-14-1_ADI-M`.
- Verify `RP2350A`, `TPS2121RUXR`, `TPS62175DQCR`, USB-C, and connector
  footprints from manufacturer drawings before release.

## Sources to re-check

- JLCPCB parts search: `https://jlcpcb.com/parts`
- LCSC product pages for the `C` codes above
- Raspberry Pi RP2350/RP2350A hardware design files and datasheet
- Analog Devices ADXL355 package drawing
- TI TPS62175, TPS2121, TLV75733, TPS7A20 datasheets
