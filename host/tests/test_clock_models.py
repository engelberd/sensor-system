from __future__ import annotations

import unittest

from host.common.clock_models import (
    ClockState,
    NodeHostClockModel,
    SampleClockModel,
    TimeSyncObservation,
    UtcCorrelationModel,
)


class SampleClockModelTests(unittest.TestCase):
    def test_estimates_real_period_and_restarts_on_new_epoch(self) -> None:
        model = SampleClockModel(min_lock_points=4)
        for index in range(4):
            self.assertTrue(model.add_anchor(
                node_id=1,
                boot_epoch=10,
                timing_segment_id=3,
                sample_seq=index * 32,
                device_time_us=1_000_000 + index * 32 * 8_001,
                uncertainty_us=2,
            ))
        self.assertEqual(model.state, ClockState.LOCKED)
        assert model.estimate is not None
        self.assertAlmostEqual(model.estimate.slope, 8_001.0, places=6)

        self.assertTrue(model.add_anchor(
            node_id=1,
            boot_epoch=11,
            timing_segment_id=1,
            sample_seq=0,
            device_time_us=100,
            uncertainty_us=2,
        ))
        self.assertEqual(model.state, ClockState.UNSYNCED)
        self.assertEqual(len(model.points), 1)

    def test_rejects_non_monotonic_sequence(self) -> None:
        model = SampleClockModel()
        self.assertTrue(model.add_anchor(
            node_id=1,
            boot_epoch=1,
            timing_segment_id=1,
            sample_seq=10,
            device_time_us=1000,
            uncertainty_us=1,
        ))
        self.assertFalse(model.add_anchor(
            node_id=1,
            boot_epoch=1,
            timing_segment_id=1,
            sample_seq=10,
            device_time_us=2000,
            uncertainty_us=1,
        ))
        self.assertEqual(model.state, ClockState.INVALID)

    def test_exact_historical_anchor_is_idempotent(self) -> None:
        model = SampleClockModel()
        for sample_seq, device_time_us in ((10, 1000), (20, 2000)):
            self.assertTrue(model.add_anchor(
                node_id=1,
                boot_epoch=1,
                timing_segment_id=1,
                sample_seq=sample_seq,
                device_time_us=device_time_us,
                uncertainty_us=1,
            ))

        self.assertTrue(model.add_anchor(
            node_id=1,
            boot_epoch=1,
            timing_segment_id=1,
            sample_seq=10,
            device_time_us=1000,
            uncertainty_us=1,
        ))
        self.assertNotEqual(model.state, ClockState.INVALID)


class NodeHostClockModelTests(unittest.TestCase):
    def observation(
        self,
        index: int,
        *,
        boot_epoch: int = 7,
        extra_rtt_ns: int = 0,
    ) -> TimeSyncObservation:
        node_us = 1_000_000 + index * 1_000_000
        host_ns = 5_000_000_000 + node_us * 1000
        return TimeSyncObservation(
            boot_epoch=boot_epoch,
            sync_id=index,
            t1_host_monotonic_ns=host_ns - 100_000 - extra_rtt_ns,
            t2_node_rx_us=node_us - 10,
            t3_node_tx_us=node_us + 10,
            t4_host_monotonic_ns=host_ns + 100_000 + extra_rtt_ns,
        )

    def test_low_rtt_regression_locks_and_maps_device_clock(self) -> None:
        model = NodeHostClockModel(min_lock_observations=5)
        for index in range(6):
            self.assertTrue(model.add(self.observation(index)))
        self.assertEqual(model.state, ClockState.LOCKED)
        predicted, uncertainty, state = model.predict_host_monotonic_ns(
            8_000_000,
            now_monotonic_ns=model.last_host_monotonic_ns or 0,
        )
        self.assertAlmostEqual(predicted, 13_000_000_000, delta=10)
        self.assertGreaterEqual(uncertainty, 90_000)
        self.assertEqual(state, ClockState.LOCKED)

    def test_new_epoch_discards_old_fit(self) -> None:
        model = NodeHostClockModel(min_lock_observations=3)
        for index in range(3):
            self.assertTrue(model.add(self.observation(index)))
        self.assertEqual(model.state, ClockState.LOCKED)
        self.assertTrue(model.add(self.observation(4, boot_epoch=8)))
        self.assertEqual(model.state, ClockState.UNSYNCED)
        self.assertEqual(len(model.observations), 1)


class UtcCorrelationTests(unittest.TestCase):
    def test_wall_clock_step_starts_new_segment(self) -> None:
        model = UtcCorrelationModel(step_threshold_ns=10_000)
        model.observe(
            monotonic_before_ns=1_000,
            utc_ns=1_001_000,
            monotonic_after_ns=1_002,
        )
        first_segment = model.segment_id
        model.observe(
            monotonic_before_ns=2_000,
            utc_ns=1_102_000,
            monotonic_after_ns=2_002,
        )
        self.assertEqual(model.segment_id, first_segment + 1)


if __name__ == "__main__":
    unittest.main()
