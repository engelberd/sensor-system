# Sensor System repository guidance

## Source of truth

- Keep one product line on `main`; do not create per-host or per-board branches.
- Hardware-dependent pins belong in `node/config/board_profile.h` and are selected at build time.
- `board_revision` is immutable hardware identity: `2` is the installed wiring
  (SCK 14, MOSI 15, DRDY 11, INT1 10, no DE GPIO); `1` uses SCK 10, MOSI 11,
  DRDY 14, INT1 15 and DE on GPIO2.
- Node address, baudrate, ODR, range, filters and offsets are runtime configuration, not board-profile constants.
- Never commit `host/system_config.json`, active systemd units, recordings, runtime files or raw diagnostic dumps.

## Local state

- Use `./hostctl ...` from any directory; it anchors relative paths at the repository root.
- New local installations use `var/recordings`, `var/archive`, `var/log` and `var/tmp`.
- Runtime coordination belongs in `/run/sensor-system`.
- Run `./hostctl paths` to inspect effective paths before recording or deployment.

## Verification

- After host changes run `host/.venv/bin/python -m unittest discover -s host/tests`.
- After firmware logic changes run `bash node/tests/run_host_tests.sh`.
- Use `./sgit` instead of `git` in this Codex workspace.
- A firmware update must not bypass board-revision validation unless the physical board was checked manually.

## Repository hygiene

- Keep conclusions and repeatable diagnostic procedures, not raw process logs.
- Keep KiCad source, project libraries and hardware notes in `hardware/`; publish manufacturing exports as release artifacts.
- Do not rewrite shared Git history without explicit approval and coordination with every deployed system.
