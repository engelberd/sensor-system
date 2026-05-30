# KiCad Git workflow

## Recommended workflow

1. Pull the latest repository state.
2. Open `hardware/KiCad - node V2/RP2350.kicad_pro`.
3. Make one logical change at a time.
4. Save schematic and PCB.
5. Review `git diff` before committing.
6. Commit KiCad source files together with any footprint/library changes.

## Good commits

Good hardware commits are small enough to review:

- `hardware: add TPS2121 power mux`
- `hardware: route ADXL355 SPI`
- `hardware: update USB-C footprint`
- `hardware: document power input parts`

Avoid one giant commit that changes schematic, board placement, all footprints,
and unrelated notes at once.

## Files to expect in diffs

Schematic changes usually touch:

```text
*.kicad_sch
*.kicad_pro
```

PCB/layout changes usually touch:

```text
*.kicad_pcb
```

Footprint changes usually touch:

```text
*.pretty/*.kicad_mod
```

Local settings and backups are ignored by `.gitignore`.

## Conflict handling

KiCad files are text files, but schematic and PCB conflicts can still be painful.
When a conflict happens:

1. Save a copy of both branches if the change is important.
2. Resolve text conflicts carefully.
3. Open the project in KiCad.
4. Run Electrical Rules Check and Design Rules Check.
5. Save again and commit the resolved project.

## Release folders

When sending a board to manufacture, create a dated release folder only for the
exact exported files that were sent:

```text
hardware/releases/2026-05-30-node-v2/
```

Suggested contents:

- gerbers
- drill files
- BOM export
- position/CPL export
- PDF schematic
- short release note with Git commit hash

Keep ordinary KiCad automatic backups out of Git.
