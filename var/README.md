# Local runtime data

This directory has the same layout on every checkout, but its contents are
local and ignored by Git:

- `recordings/` — active Capture files
- `archive/` — compacted Archive files
- `diagnostics/` — temporary diagnostic exports
- `log/` — supervisor and worker logs
- `tmp/` — disposable working files

Only this README and the empty directory markers are versioned. Never commit
measurements, logs, locks, status files or temporary artifacts.
