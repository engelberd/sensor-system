from __future__ import annotations

import unittest

from host.recorder.contracts import (
    ARCHIVE_SCHEMA_MAJOR,
    CAPTURE_SCHEMA_MAJOR,
    ArchiveQualityFlag,
    CaptureRoutingFlag,
    DecimationFilterProfile,
    SensorIdentity,
)


class RecorderContractTests(unittest.TestCase):
    def test_initial_channel_sensor_mapping(self) -> None:
        identities = [
            SensorIdentity.temporary(index, node_address=1)
            for index in range(1, 9)
        ]
        self.assertEqual(identities[0].sensor_label, "A")
        self.assertEqual(identities[-1].sensor_label, "H")

    def test_sensor_assignment_can_move_between_channels(self) -> None:
        identity = SensorIdentity(
            channel_id=1,
            sensor_label="B",
            node_address=1,
        )
        self.assertEqual(identity.sensor_label, "B")

    def test_board_revision_is_validated(self) -> None:
        identity = SensorIdentity(
            channel_id=1, sensor_label="A", node_address=1, board_revision=2
        )
        self.assertEqual(identity.board_revision, 2)
        with self.assertRaisesRegex(ValueError, "board_revision"):
            SensorIdentity(
                channel_id=1, sensor_label="A", node_address=1, board_revision=3
            )

    def test_sensor_label_must_be_known(self) -> None:
        with self.assertRaisesRegex(ValueError, "range A..H"):
            SensorIdentity(channel_id=1, sensor_label="X", node_address=1)

    def test_quality_and_routing_flags_are_independent(self) -> None:
        routing = (
            CaptureRoutingFlag.UTC_VALID
            | CaptureRoutingFlag.BOUNDARY_UNCERTAIN
        )
        quality = ArchiveQualityFlag.BOUNDARY_UNCERTAIN
        self.assertTrue(routing & CaptureRoutingFlag.UTC_VALID)
        self.assertTrue(quality & ArchiveQualityFlag.BOUNDARY_UNCERTAIN)
        self.assertFalse(quality & ArchiveQualityFlag.TIME_UNSYNCED)

    def test_first_clean_schemas_use_major_version_one(self) -> None:
        self.assertEqual(CAPTURE_SCHEMA_MAJOR, 1)
        self.assertEqual(ARCHIVE_SCHEMA_MAJOR, 1)

    def test_filter_profile_values_match_firmware_contract(self) -> None:
        self.assertEqual(int(DecimationFilterProfile.LIGHT), 0)
        self.assertEqual(int(DecimationFilterProfile.BALANCED), 1)
        self.assertEqual(int(DecimationFilterProfile.AGGRESSIVE), 2)


if __name__ == "__main__":
    unittest.main()
