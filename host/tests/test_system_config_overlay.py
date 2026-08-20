from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from host.common.system_config import HostSystemConfig, load_config_data

PROJECT_ROOT = Path(__file__).resolve().parents[2]


class SystemConfigOverlayTests(unittest.TestCase):
    def test_capture_v1_window_must_be_bounded_and_divide_utc_day(self) -> None:
        with self.assertRaisesRegex(ValueError, "divide one UTC day"):
            HostSystemConfig.from_dict({
                "storage": {"capture_schema": 1, "window_seconds": 700},
                "channels": [{
                    "name": "line-a",
                    "port": "/dev/ttyUSB0",
                    "nodes": [{"id": 1}],
                }],
            })

    def test_repository_system_profile_and_compatibility_alias_match(self) -> None:
        profile = HostSystemConfig.load(
            PROJECT_ROOT / "host/configs/systems/rpi-sanok.json"
        )
        compatibility = HostSystemConfig.load(
            PROJECT_ROOT / "host/configs/host_system.base.json"
        )

        self.assertEqual(profile, compatibility)
        self.assertEqual(profile.system.name, "rpi-sanok")
        self.assertEqual(profile.system.site, "sanok")
        self.assertEqual(len(profile.channels), 8)
        self.assertTrue(
            all(node.board_revision == 2 for channel in profile.channels for node in channel.nodes)
        )

    def test_repository_macos_overlay_is_valid_when_copied_to_host(self) -> None:
        template_path = (
            PROJECT_ROOT / "host/configs/host_system.macos.example.json"
        )
        data = json.loads(template_path.read_text(encoding="utf-8"))
        data["extends"] = str(PROJECT_ROOT / "host" / data["extends"])

        with tempfile.TemporaryDirectory() as tmp:
            local_path = Path(tmp) / "system_config.json"
            local_path.write_text(json.dumps(data), encoding="utf-8")
            config = HostSystemConfig.load(local_path)

        self.assertEqual(config.system.name, "macos-workstation")
        self.assertEqual(config.supervisor.channel_runtime_dir, "var/run")
        self.assertEqual(len(config.channels), 8)
        self.assertTrue(all(not channel.enabled for channel in config.channels))

    def test_overlay_merges_channels_by_name_and_nodes_by_id(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            base = root / "base.json"
            local = root / "local.json"
            base.write_text(
                json.dumps(
                    {
                        "system": {"name": "base", "timezone": "Europe/Warsaw"},
                        "storage": {"root_dir": "var/recordings", "capture_schema": 1},
                        "channels": [
                            {
                                "name": "line-a",
                                "port": "/dev/base-a",
                                "nodes": [
                                    {"id": 1, "board_revision": 2, "sensor_odr_hz": 250}
                                ],
                            },
                            {
                                "name": "line-b",
                                "port": "/dev/base-b",
                                "nodes": [{"id": 1, "board_revision": 2}],
                            },
                        ],
                    }
                ),
                encoding="utf-8",
            )
            local.write_text(
                json.dumps(
                    {
                        "extends": "base.json",
                        "system": {"name": "host-one"},
                        "channels": [
                            {
                                "name": "line-a",
                                "port": "/dev/local-a",
                                "nodes": [{"id": 1, "sensor_odr_hz": 500}],
                            },
                            {"name": "line-b", "enabled": False},
                        ],
                    }
                ),
                encoding="utf-8",
            )

            resolved = load_config_data(local)
            config = HostSystemConfig.load(local)

            self.assertNotIn("extends", resolved)
            self.assertEqual(config.system.name, "host-one")
            self.assertEqual(config.system.timezone, "Europe/Warsaw")
            self.assertEqual(config.channels[0].port, "/dev/local-a")
            self.assertEqual(config.channels[0].nodes[0].board_revision, 2)
            self.assertEqual(config.channels[0].nodes[0].sensor_odr_hz, 500)
            self.assertFalse(config.channels[1].enabled)

    def test_circular_extends_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "a.json").write_text('{"extends":"b.json"}', encoding="utf-8")
            (root / "b.json").write_text('{"extends":"a.json"}', encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "circular"):
                load_config_data(root / "a.json")


if __name__ == "__main__":
    unittest.main()
