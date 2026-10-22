from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from host.dashboard.v2_commissioning import (
    CommissioningProfileError,
    CommissioningProfileStore,
)


class CommissioningProfileStoreTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        systems = root / "configs" / "systems"
        systems.mkdir(parents=True)
        self.profile = systems / "lab.json"
        self.profile.write_text(
            json.dumps(
                {
                    "system": {"name": "lab"},
                    "channels": [
                        {
                            "name": "line-b",
                            "channel_id": 2,
                            "enabled": True,
                            "nodes": [
                                {
                                    "id": 1,
                                    "hardware_id": "0102030405060708",
                                    "board_revision": 2,
                                    "enabled": True,
                                    "output_rate_hz": 125,
                                }
                            ],
                        }
                    ],
                }
            ),
            encoding="utf-8",
        )
        self.active = root / "system.json"
        self.active.write_text(
            json.dumps({"extends": "configs/systems/lab.json"}), encoding="utf-8"
        )
        self.store = CommissioningProfileStore(self.active)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_adds_named_node_to_stable_system_profile(self) -> None:
        result = self.store.add_node(
            channel_id=2,
            node_name="Łożysko północ",
            node_address=2,
            hardware_id="AABBCCDDEEFF0011",
            board_revision=2,
        )

        self.assertEqual(Path(result["profile_path"]), self.profile.resolve())
        saved = json.loads(self.profile.read_text(encoding="utf-8"))
        self.assertEqual(
            saved["channels"][0]["nodes"][1],
            {
                "id": 2,
                "name": "Łożysko północ",
                "hardware_id": "aabbccddeeff0011",
                "board_revision": 2,
                "enabled": True,
                "output_rate_hz": 125,
            },
        )
        self.assertEqual(json.loads(self.active.read_text(encoding="utf-8")), {"extends": "configs/systems/lab.json"})

    def test_rejects_address_and_hardware_identity_conflicts_before_write(self) -> None:
        original = self.profile.read_bytes()
        with self.assertRaisesRegex(CommissioningProfileError, "address is already used"):
            self.store.add_node(
                channel_id=2,
                node_name="duplicate address",
                node_address=1,
                hardware_id="1111111111111111",
                board_revision=2,
            )
        with self.assertRaisesRegex(CommissioningProfileError, "hardware_id is already present"):
            self.store.add_node(
                channel_id=2,
                node_name="duplicate hardware",
                node_address=2,
                hardware_id="0102030405060708",
                board_revision=2,
            )
        self.assertEqual(self.profile.read_bytes(), original)

    def test_updates_scheduler_output_rate_in_stable_system_profile(self) -> None:
        result = self.store.update_node_output_rate(
            channel_id=2,
            node_address=1,
            output_rate_hz=500,
        )

        saved = json.loads(self.profile.read_text(encoding="utf-8"))
        self.assertEqual(saved["channels"][0]["nodes"][0]["output_rate_hz"], 500)
        self.assertEqual(result["output_rate_hz"], 500)

    def test_rejects_output_rate_update_for_unknown_node(self) -> None:
        original = self.profile.read_bytes()
        with self.assertRaisesRegex(CommissioningProfileError, "node is not defined"):
            self.store.update_node_output_rate(
                channel_id=2,
                node_address=9,
                output_rate_hz=500,
            )
        self.assertEqual(self.profile.read_bytes(), original)

    def test_same_output_rate_does_not_rewrite_profile(self) -> None:
        original = self.profile.read_bytes()

        result = self.store.update_node_output_rate(
            channel_id=2,
            node_address=1,
            output_rate_hz=125.0,
        )

        self.assertEqual(result["output_rate_hz"], 125)
        self.assertEqual(self.profile.read_bytes(), original)

    def test_updates_telemetry_interval_with_output_rate_atomically(self) -> None:
        result = self.store.update_node_output_rate(
            channel_id=2,
            node_address=1,
            output_rate_hz=125,
            telemetry_interval_seconds=300,
        )

        saved = json.loads(self.profile.read_text(encoding="utf-8"))
        self.assertEqual(
            saved["channels"][0]["nodes"][0]["telemetry_interval_seconds"], 300
        )
        self.assertEqual(result["telemetry_interval_seconds"], 300)
        self.assertTrue(result["telemetry_interval_changed"])

    def test_rejects_invalid_telemetry_interval_before_write(self) -> None:
        original = self.profile.read_bytes()
        with self.assertRaisesRegex(CommissioningProfileError, "60..86400"):
            self.store.update_node_output_rate(
                channel_id=2,
                node_address=1,
                output_rate_hz=125,
                telemetry_interval_seconds=30,
            )
        self.assertEqual(self.profile.read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
