from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from host.host_configurator import build_parser, ConfigView, sync_system_config_from_device_config


class HostConfiguratorSyncTests(unittest.TestCase):
    def test_sync_system_config_updates_confirmed_device_settings(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            config_path = Path(tmp) / "system_config.json"
            config_path.write_text(
                json.dumps(
                    {
                        "system": {"name": "sensor-system-production"},
                        "storage": {"root_dir": "runs"},
                        "supervisor": {"status_file": "/tmp/status.json"},
                        "channels": [
                            {
                                "name": "line-a",
                                "port": "/dev/ttyACM0",
                                "baud": 115200,
                                "nodes": [
                                    {
                                        "id": 1,
                                        "name": "Czujnik 1",
                                        "enabled": True,
                                        "expected_odr_hz": 500,
                                    }
                                ],
                            }
                        ],
                    }
                )
                + "\n",
                encoding="utf-8",
            )

            result = sync_system_config_from_device_config(
                config_path,
                port="/dev/ttyACM0",
                previous_node_id=1,
                updated=ConfigView(
                    node_id=1,
                    baudrate=57600,
                    odr_hz=125,
                    range_g=4,
                    offset_x=11,
                    offset_y=22,
                    offset_z=33,
                    fifo_watermark=30,
                    act_threshold=0,
                    act_count=0,
                    high_pass_corner=3,
                ),
            )

            self.assertEqual(result.channel_name, "line-a")
            saved = json.loads(config_path.read_text(encoding="utf-8"))
            channel = saved["channels"][0]
            node = channel["nodes"][0]
            self.assertEqual(channel["baud"], 57600)
            self.assertEqual(node["id"], 1)
            self.assertEqual(node["expected_odr_hz"], 62.5)
            self.assertEqual(node["sensor_odr_hz"], 125)
            self.assertEqual(node["range_g"], 4)
            self.assertEqual(node["high_pass_corner"], 3)
            self.assertEqual(node["fifo_watermark"], 30)
            self.assertEqual(node["offset_x"], 11)
            self.assertEqual(node["offset_y"], 22)
            self.assertEqual(node["offset_z"], 33)

    def test_set_odr_parser_rejects_disabled_lower_odr_value(self) -> None:
        with self.assertRaises(SystemExit):
            build_parser().parse_args(["set-odr", "62.5"])

    def test_set_baudrate_parser_rejects_disabled_higher_baudrate(self) -> None:
        with self.assertRaises(SystemExit):
            build_parser().parse_args(["set-baudrate", "230400"])


if __name__ == "__main__":
    unittest.main()
