# sensor-system

This repository contains the full Sensor System project:

- `node/` for the Pico firmware and bootloader artifacts
- `host/` for recorder, configurator, supervisor, dashboards, and deployment files
- `runs/` for local acquisition output that stays untracked

Suggested day-to-day workflow:

1. Make changes locally in this repository.
2. Use `./sgit ...` for repository commands in this environment.
3. Commit before flashing or deploying to another device.
4. Push to your remote.
5. On the Raspberry Pi host, run `git pull`, build from `node/`, and flash the selected artifact.
