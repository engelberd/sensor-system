# Hardware progress

This is the shared status page for the KiCad hardware work. Keep it short and
update it in the same commit as the schematic/layout change.

## Current goal

Bring up the first RP2350A + ADXL355 node PCB with USB-C development access,
12 V field power, RS485, and clean accelerometer supply.

## Board status

Status: `schematic/layout in progress`

| Area | Status | Notes |
|---|---:|---|
| Architecture | in progress | Power tree, sensor, RS485, USB-C direction documented. |
| Part selection | in progress | Main candidates listed in `parts.md`; several footprints still need verification. |
| Schematic | in progress | KiCad project exists in `KiCad - node V2/`. |
| PCB layout | in progress | Board file exists; release checks still open. |
| ERC/DRC | todo | Run before every PCB release candidate. |
| Manufacturing export | todo | Create only after ERC/DRC and footprint checks pass. |
| Bring-up plan | todo | Add test points, power-up order, and first-flash procedure before ordering. |

## Assumptions

- Repository stays as one monorepo with `hardware/`, `node/`, and `host/`.
- Project-local footprints in `my_footprints.pretty/` are the shared source for
  non-standard footprints.
- Raw vendor downloads in `footprints.extra/` are local scratch material and are
  ignored by Git.
- First board revision prioritizes bring-up and debug access over minimum size.
- Firmware pinout should stay close to the existing node firmware where possible.

## Decisions

| Date | Decision | Reason |
|---|---|---|
| 2026-05-30 | Use monorepo layout instead of nested hardware Git repo. | Easier combined hardware/software review and fewer submodule problems. |
| 2026-05-30 | Track KiCad source files and cleaned project-local footprints. | Everyone opens the same project with the same footprint library. |
| 2026-05-30 | Ignore KiCad backups, local project state, and raw vendor imports. | Keeps commits reviewable and avoids machine-local noise. |
| 2026-05-30 | Track concrete JLCPCB/LCSC sourcing in `jlcpcb-parts.md`. | Keeps stock-sensitive decisions separate from general design notes. |

## To do

| Priority | Task | Owner | Notes |
|---:|---|---|---|
| P0 | Verify RP2350A QFN footprint against Raspberry Pi hardware files. | TBD | Required before PCB release. |
| P0 | Verify ADXL355 footprint/package choice. | TBD | Pick `E-14-1_ADI`, `E-14-1_ADI-M`, or another verified footprint. |
| P0 | Calculate TPS62175 feedback, inductor, and capacitor values. | TBD | Use datasheet values; document result in `parts.md` or schematic notes. |
| P0 | Calculate TPS2121 priority, threshold, and current-limit resistors. | TBD | Do not leave control pins floating. |
| P0 | Verify USB-C connector footprint and exact MPN. | TBD | Mechanical fit and stock matter. |
| P0 | Finalize JLCPCB/LCSC part numbers for every assembled BOM row. | TBD | Start from `jlcpcb-parts.md`; stock must be re-checked before order. |
| P1 | Add/verify test points for `12V_IN`, `5V_BUCK`, `5V_SYS`, `3V3_DIG`, `3V3_ADXL`, SWD, UART/RS485. | TBD | Helps bring-up a lot. |
| P1 | Run KiCad ERC and fix release-blocking errors. | TBD | Record result below. |
| P1 | Run KiCad DRC and fix release-blocking errors. | TBD | Record result below. |
| P1 | Add bring-up checklist. | TBD | Include first power-up current limit and rail checks. |
| P2 | Prepare release export folder naming convention. | TBD | See `kicad-git-workflow.md`. |

## Done

| Date | Item |
|---|---|
| 2026-05-30 | Added hardware documentation structure. |
| 2026-05-30 | Added KiCad Git ignore rules. |
| 2026-05-30 | Added shared parts/footprint table. |
| 2026-05-30 | Added KiCad collaboration workflow. |
| 2026-05-30 | Added first JLCPCB critical-parts shortlist. |

## Open risks

| Risk | Impact | Mitigation |
|---|---|---|
| Wrong footprint for RP2350A, ADXL355, TPS2121, USB-C, or buck regulator. | Board may be unbuildable. | Verify against datasheets before release. |
| 12 V input protection too aggressive or too weak. | Nuisance clamping or damaged input stage. | Decide real input tolerance and choose TVS accordingly. |
| Buck noise coupling into ADXL355. | Poor accelerometer measurements. | Keep buck away from sensor, route carefully, preserve analog rail filtering option. |
| KiCad parallel edits conflict. | Lost time resolving schematic/PCB changes. | Pull before editing and keep commits small. |

## Verification log

| Date | Check | Result | Notes |
|---|---|---|---|
| TBD | ERC | todo | |
| TBD | DRC | todo | |
| TBD | Footprint review | todo | |
| TBD | Manufacturing export review | todo | |

## Next step

Verify critical footprints and power component resistor/value calculations before
spending more time on final PCB routing.
