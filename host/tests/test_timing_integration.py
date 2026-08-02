from __future__ import annotations

from types import SimpleNamespace
import unittest

from host.common.clock_models import ClockState, TimeSyncObservation
from host.host_recorder import apply_packet_timing
from host.recorder.model import RecorderNode, SampleRecord


class TimingPipelineIntegrationTests(unittest.TestCase):
    BOOT_EPOCH = 0x1020304050607080
    HOST_OFFSET_NS = 8_000_000_000
    UTC_OFFSET_NS = 1_800_000_000_000_000_000
    NODE_TO_HOST_NS_PER_US = 1000.0 / 1.00005

    def make_node(self) -> RecorderNode:
        return RecorderNode(
            node_id=1,
            config=SimpleNamespace(odr_hz=250, range_g=2),
        )

    def host_time_for_node_us(self, node_time_us: int) -> int:
        return round(
            self.HOST_OFFSET_NS
            + node_time_us * self.NODE_TO_HOST_NS_PER_US
        )

    def lock_node_host_clock(
        self,
        node: RecorderNode,
        *,
        boot_epoch: int = BOOT_EPOCH,
        start_sync_id: int = 1,
    ) -> None:
        for index, one_way_ns in enumerate(
            (80_000, 130_000, 70_000, 210_000, 90_000, 75_000)
        ):
            node_mid_us = 2_000_000 + index * 1_000_000
            host_mid_ns = self.host_time_for_node_us(node_mid_us)
            observation = TimeSyncObservation(
                boot_epoch=boot_epoch,
                sync_id=start_sync_id + index,
                t1_host_monotonic_ns=host_mid_ns - one_way_ns - 10_000,
                t2_node_rx_us=node_mid_us - 10,
                t3_node_tx_us=node_mid_us + 10,
                t4_host_monotonic_ns=host_mid_ns + one_way_ns + 10_000,
            )
            self.assertTrue(node.node_host_clock.add(observation))
        self.assertEqual(node.node_host_clock.state, ClockState.LOCKED)

    def test_drift_jitter_retransmission_and_reboot_are_fail_closed(
        self,
    ) -> None:
        node = self.make_node()
        self.lock_node_host_clock(node)

        utc_observation_host_ns = self.host_time_for_node_us(7_500_000)
        node.utc_clock.observe(
            monotonic_before_ns=utc_observation_host_ns - 1_000,
            utc_ns=utc_observation_host_ns + self.UTC_OFFSET_NS,
            monotonic_after_ns=utc_observation_host_ns + 1_000,
        )

        first_device_us = 8_000_000
        period_us = 8_000
        packet = SimpleNamespace(
            boot_epoch=self.BOOT_EPOCH,
            timestamp_quality_flags=1,
            timing_segment_id=3,
            first_sample_seq=10_000,
            sample_count=32,
            first_device_time_us=first_device_us,
            last_device_time_us=first_device_us + 31 * period_us,
            max_fit_residual_us=2,
        )
        samples = [
            SampleRecord(
                node_id=1,
                sample_seq=packet.first_sample_seq + index,
                raw_x=0,
                raw_y=0,
                raw_z=0,
                packet_seq=77,
                device_time_us=first_device_us + index * period_us,
                boot_epoch=self.BOOT_EPOCH,
                timing_segment_id=3,
                timing_quality_flags=1,
            )
            for index in range(packet.sample_count)
        ]
        now_ns = node.node_host_clock.last_host_monotonic_ns or 0
        apply_packet_timing(
            node,
            packet,
            samples,
            now_monotonic_ns=now_ns,
        )

        for sample in samples:
            assert sample.device_time_us is not None
            expected_utc_ns = (
                self.host_time_for_node_us(sample.device_time_us)
                + self.UTC_OFFSET_NS
            )
            self.assertIsNotNone(sample.acquisition_utc_ns)
            self.assertAlmostEqual(
                sample.acquisition_utc_ns or 0,
                expected_utc_ns,
                delta=250_000,
            )
            self.assertIsNotNone(sample.timing_uncertainty_ns)

        anchor_count = len(node.sample_clock.points)
        apply_packet_timing(
            node,
            packet,
            samples,
            now_monotonic_ns=now_ns,
        )
        self.assertEqual(len(node.sample_clock.points), anchor_count)
        self.assertNotEqual(node.sample_clock.state, ClockState.INVALID)

        reboot_epoch = self.BOOT_EPOCH + 1
        self.lock_node_host_clock(
            node,
            boot_epoch=reboot_epoch,
            start_sync_id=100,
        )
        self.assertEqual(node.node_host_clock.boot_epoch, reboot_epoch)
        stale_samples = [
            SampleRecord(
                node_id=1,
                sample_seq=sample.sample_seq,
                raw_x=sample.raw_x,
                raw_y=sample.raw_y,
                raw_z=sample.raw_z,
                packet_seq=sample.packet_seq,
                range_g=sample.range_g,
                device_time_us=sample.device_time_us,
                boot_epoch=sample.boot_epoch,
                timing_segment_id=sample.timing_segment_id,
                timing_quality_flags=sample.timing_quality_flags,
            )
            for sample in samples
        ]
        apply_packet_timing(
            node,
            packet,
            stale_samples,
            now_monotonic_ns=node.node_host_clock.last_host_monotonic_ns or 0,
        )
        self.assertTrue(
            all(sample.acquisition_utc_ns is None for sample in stale_samples)
        )


if __name__ == "__main__":
    unittest.main()
