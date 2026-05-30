# Hardware

Hardware design files for the Sensor System project.

Current KiCad project:

- `KiCad - node V2/RP2350.kicad_pro`

## Repository layout

This project should stay as one GitHub repository for now:

- `hardware/` - KiCad projects, PCB notes, parts, footprints, manufacturing files
- `node/` - RP2350/Pico firmware
- `host/` - host tools, recorder, dashboards, deployment files

This is a monorepo layout. It is simpler than a nested Git repository and makes
hardware/software changes easy to review together. A separate hardware repository
or Git submodule only starts to pay off if the PCB must be shared with a different
team, licensed differently, or released on a different schedule.

## What is tracked

Track these files:

- `*.kicad_pro`
- `*.kicad_sch`
- `*.kicad_pcb`
- project-local `*.kicad_sym` files
- project-local `*.pretty/*.kicad_mod` footprint libraries
- selected 3D models used by footprints
- documentation in this directory
- released fabrication outputs in a named release folder, if needed

Do not track these files:

- `*.kicad_prl`
- `*-backups/`
- `fp-info-cache`
- local lock/temp files
- `.DS_Store`

## Collaboration rules

- Commit before making large schematic rewires or PCB placement changes.
- Pull before opening KiCad if someone else may have pushed hardware changes.
- Avoid editing the same schematic sheet or PCB placement in parallel.
- For footprint changes, edit project-local libraries first, then update the
  board from the library and commit both the library change and board change.
- Put part decisions in `parts.md` before layout, especially when package,
  stock, or footprint choice matters.
- Put layout/manufacturing decisions in `design-notes.md`.
- Track current work, assumptions, tasks, and completed items in `progress.md`.

## Opening the project

Open this file in KiCad:

```text
hardware/KiCad - node V2/RP2350.kicad_pro
```

If a footprint is missing, first check the project-local library:

```text
hardware/KiCad - node V2/my_footprints.pretty
```

Vendor downloads and raw imported libraries can stay locally in
`footprints.extra/`, which is ignored by Git. After checking a vendor footprint,
copy the cleaned version into `my_footprints.pretty` so everyone gets the same
reviewed footprint.
