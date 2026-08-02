# Host configuration layers

| Path | Tracked | Purpose |
| --- | --- | --- |
| `common.json` | yes | Product-wide storage and supervisor defaults |
| `systems/<name>.json` | yes | Stable hardware and sensor inventory for one installation |
| `host_system.local.example.json` | yes | Example local port overlay copied during setup |
| `../system_config.json` | no | Active host-specific paths and temporary overrides |

The active file should extend exactly one profile from `systems/`; that profile
extends `common.json`. The `host_system.base.json` file is a compatibility alias
for existing Sanok installations and should not be used by new systems.

Validate the result with `./hostctl paths --init` and `./hostctl doctor`.
