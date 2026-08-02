# Shared system profiles

Each tracked file in this directory describes one physical installation and
extends `../common.json`. Keep stable, non-secret facts here: channel names,
sensor settings, board revisions and expected baud rates.

A profile may have a companion Markdown file for installation-specific context
such as required external drivers, converter hardware and maintenance notes.

Do not put device paths, usernames, absolute storage paths, credentials or
temporary enable/disable decisions here. Those belong in the ignored
`host/system_config.json` on each host.

When adding a system, copy `system.example.json`, choose a stable lowercase
name, and commit the new profile so all hosts and Codex sessions share the same
source of truth.
