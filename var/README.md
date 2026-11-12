# Local runtime data

This directory has the same layout on every checkout, but its contents are
local and ignored by Git:

- `recordings/` — active Capture, Time and Telemetry WAL plus configuration history
- `analysis/` — flat directory of self-contained HDF5 windows, e.g. `line-b_2026-11-11_16-38.h5`
- `archive/` — compacted Archive files
- `diagnostics/` — temporary diagnostic exports
- `log/` — supervisor and worker logs
- `tmp/` — disposable working files
Runtime status, sockets and locks belong in `/run/sensor-system`, outside this directory.

Only this README and the empty directory markers are versioned. Never commit
measurements, logs, locks, status files or temporary artifacts.
