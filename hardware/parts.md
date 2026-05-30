# Parts and footprint decisions

Working list for the RP2350 + ADXL355 node. Use this file as the shared source
of truth before ordering parts or releasing gerbers.

Status values:

- `proposed` - likely choice, still needs verification
- `selected` - approved for schematic/layout
- `verify` - do not release PCB until this row is checked
- `dnp` - footprint kept but normally not populated

| Block | Part | Status | KiCad symbol | Footprint | Notes / links |
|---|---|---:|---|---|---|
| MCU | RP2350A | verify | `MCU_RaspberryPi:RP2350A` | `QFN-60-1EP_7x7mm_P0.4mm_EP3.4x3.4mm` | Verify exposed pad and land pattern against Raspberry Pi hardware files. |
| Accelerometer | ADXL355BEZ-RL7 | selected | local/imported `ADXL355BEZ` | `E-14-1_ADI` | Main vibration sensor. Local footprints also include `E-14-1_ADI-L` and `E-14-1_ADI-M`; release choice must match ADI package drawing. |
| QSPI flash | W25Q128JVS or equivalent 16 MiB flash | proposed | `Memory_Flash:W25Q128JVS` or imported symbol | `SOIC-8_3.9x4.9mm_P1.27mm` | Confirm exact ordering suffix and voltage range. |
| 12 V input connector | 2-pin terminal / connector TBD | proposed | `Connector_Generic:Conn_01x02` | `TerminalBlock_RND_205-00278_1x04_P5.00mm_Vertical` or TBD | Footprint currently looks 4-pin; verify mechanical choice. |
| Input PTC | PTS181212V150 | proposed | `Device:Polyfuse` | `Fuse_1812_4532Metric` | Verify voltage/current rating for field use. |
| Input TVS | SM6T12A or higher working-voltage TVS | verify | `Diode:TVS` | `D_SMB` / `D_SMA` | Check real 12 V tolerance; consider 15 V or 18 V working voltage. |
| Reverse protection | SS14-E3/61T or equivalent | proposed | `Diode:Schottky` | `D_SMA` | Simple V1 approach; MOS/ideal diode can be considered later. |
| Buck regulator | TPS62175DQCR | proposed | `Regulator_Switching:TPS62175DQC` or imported | vendor DQC/VSON-10 footprint | Calculate feedback, inductor, caps from datasheet before layout release. |
| Power mux | TPS2121RUXR | selected | local/imported `TPS2121RUXR` | `RUX0012A` | Configure priority, thresholds, current limit; do not leave control pins floating. |
| Digital 3.3 V LDO | TLV75733 | proposed | `Regulator_Linear:TLV75733PDBV` | `SOT-23-5` | Powers MCU I/O, flash, RS485, LEDs. |
| Analog 3.3 V LDO | TPS7A2033 or ADP150-3.3 | proposed | package-specific symbol | `SOT-23-5` or exact vendor footprint | Low-noise rail for ADXL355. |
| ADXL supply filter | MPZ2012S601AT000 or 0R option | proposed | `Device:Ferrite_Bead` | `L_0805_2012Metric` | Keep replacement option in case ferrite/caps resonate. |
| RS485 transceiver | THVD1450DR | proposed | `Interface_UART:THVD1450DR` | `SOIC-8_3.9x4.9mm_P1.27mm` | 3.3 V logic rail. |
| RS485 TVS | CDSOT23-SM712 | proposed | `Diode:SM712_SOT23` | `SOT-23` | Protect A/B lines near connector. |
| USB ESD | USBLC6-2SC6 or TPD2EUSB30 | proposed | package-specific symbol | `SOT-23-6` / exact package | Match selected part and connector routing. |
| USB-C connector | G-Switch GT-USB-7051x or TBD | verify | `Connector:USB_C_Receptacle_USB2.0_16P` | `USB_C_Receptacle_G-Switch_GT-USB-7051x` | Verify mechanical fit and availability. |
| Programming/debug header | SWD header TBD | proposed | `Connector_Generic` | `PinHeader_1x02_P2.54mm_Vertical` or TBD | Pick final pin count and orientation. |

## Links to fill in

Add order links here once the exact MPN is chosen:

| Part | Supplier | Link | Notes |
|---|---|---|---|
| RP2350A | TBD | TBD | |
| ADXL355BEZ-RL7 | LCSC / distributor | TBD | LCSC reference noted earlier: C515892. |
| TPS2121RUXR | LCSC / distributor | TBD | LCSC reference noted earlier: C485916. |
| TPS62175DQCR | LCSC / distributor | TBD | LCSC reference noted earlier: C32097. |
| SS14-E3/61T | LCSC / distributor | TBD | LCSC reference noted earlier: C47460. |
