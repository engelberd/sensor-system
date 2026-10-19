use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use sensor_core::DeliveryCursors;

use crate::measurement_transfer::PersistenceKind;
use crate::node_session::NodeSession;
use crate::transaction::MonotonicMicros;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeMetricsSnapshot {
    pub channel_id: u8,
    pub node_address: u8,
    pub received_cursor: u64,
    pub durable_cursor: u64,
    pub released_cursor: u64,
    pub produced_cursor: Option<u64>,
    pub last_measurement_monotonic_us: Option<u64>,
    pub last_time_sync_monotonic_us: Option<u64>,
    pub configuration_revision: Option<u32>,
    pub output_rate_code: Option<u8>,
    pub persisted_sample_count: u64,
    pub gap_count: u64,
    pub missing_sample_count: u64,
    pub continuity_break_count: u64,
}

#[derive(Default)]
struct NodeMetricsAtomic {
    received_cursor: AtomicU64,
    durable_cursor: AtomicU64,
    released_cursor: AtomicU64,
    produced_cursor: AtomicU64,
    has_produced_cursor: AtomicBool,
    last_measurement_monotonic_us: AtomicU64,
    has_measurement: AtomicBool,
    last_time_sync_monotonic_us: AtomicU64,
    has_time_sync: AtomicBool,
    configuration_revision: AtomicU64,
    output_rate_code: AtomicU64,
    has_configuration: AtomicBool,
    persisted_sample_count: AtomicU64,
    gap_count: AtomicU64,
    missing_sample_count: AtomicU64,
    continuity_break_count: AtomicU64,
}

#[derive(Clone)]
pub struct NodeMetricsReader {
    channel_id: u8,
    node_address: u8,
    metrics: Arc<NodeMetricsAtomic>,
}

impl NodeMetricsReader {
    pub fn snapshot(&self) -> NodeMetricsSnapshot {
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        NodeMetricsSnapshot {
            channel_id: self.channel_id,
            node_address: self.node_address,
            received_cursor: load(&self.metrics.received_cursor),
            durable_cursor: load(&self.metrics.durable_cursor),
            released_cursor: load(&self.metrics.released_cursor),
            produced_cursor: optional_counter(
                &self.metrics.has_produced_cursor,
                &self.metrics.produced_cursor,
            ),
            last_measurement_monotonic_us: optional_counter(
                &self.metrics.has_measurement,
                &self.metrics.last_measurement_monotonic_us,
            ),
            last_time_sync_monotonic_us: optional_counter(
                &self.metrics.has_time_sync,
                &self.metrics.last_time_sync_monotonic_us,
            ),
            configuration_revision: optional_counter(
                &self.metrics.has_configuration,
                &self.metrics.configuration_revision,
            )
            .and_then(|value| u32::try_from(value).ok()),
            output_rate_code: optional_counter(
                &self.metrics.has_configuration,
                &self.metrics.output_rate_code,
            )
            .and_then(|value| u8::try_from(value).ok()),
            persisted_sample_count: load(&self.metrics.persisted_sample_count),
            gap_count: load(&self.metrics.gap_count),
            missing_sample_count: load(&self.metrics.missing_sample_count),
            continuity_break_count: load(&self.metrics.continuity_break_count),
        }
    }
}

pub(crate) struct NodeMetricsPublisher {
    metrics: Arc<NodeMetricsAtomic>,
}

impl NodeMetricsPublisher {
    pub(crate) fn new(
        channel_id: u8,
        node_address: u8,
        initial: DeliveryCursors,
    ) -> (Self, NodeMetricsReader) {
        let metrics = Arc::new(NodeMetricsAtomic::default());
        let publisher = Self {
            metrics: Arc::clone(&metrics),
        };
        publisher.observe_cursors(initial);
        (
            publisher,
            NodeMetricsReader {
                channel_id,
                node_address,
                metrics,
            },
        )
    }

    pub(crate) fn observe_session(
        &self,
        session: &NodeSession,
        produced_cursor: Option<u64>,
        now: MonotonicMicros,
    ) {
        self.observe_cursors(session.cursors());
        if let Some(produced) = produced_cursor {
            self.metrics
                .produced_cursor
                .store(produced, Ordering::Relaxed);
            self.metrics
                .has_produced_cursor
                .store(true, Ordering::Release);
        }
        self.metrics
            .last_measurement_monotonic_us
            .store(now.get(), Ordering::Relaxed);
        self.metrics.has_measurement.store(true, Ordering::Release);
    }

    pub(crate) fn observe_time_sync(&self, now: MonotonicMicros) {
        self.metrics
            .last_time_sync_monotonic_us
            .store(now.get(), Ordering::Relaxed);
        self.metrics.has_time_sync.store(true, Ordering::Release);
    }

    pub(crate) fn observe_persistence(&self, kind: PersistenceKind) {
        match kind {
            PersistenceKind::SampleChunk { sample_count } => {
                saturating_add(
                    &self.metrics.persisted_sample_count,
                    u64::from(sample_count),
                );
            }
            PersistenceKind::Gap(gap) => {
                saturating_add(&self.metrics.gap_count, 1);
                saturating_add(&self.metrics.missing_sample_count, gap.missing_count);
            }
            PersistenceKind::ContinuityBreak(_) => {
                saturating_add(&self.metrics.continuity_break_count, 1);
            }
        }
    }

    pub(crate) fn observe_configuration(
        &self,
        configuration: sensor_protocol::ObservedConfiguration,
    ) {
        self.metrics
            .configuration_revision
            .store(u64::from(configuration.revision), Ordering::Relaxed);
        self.metrics.output_rate_code.store(
            u64::from(configuration.desired.output_rate as u8),
            Ordering::Relaxed,
        );
        self.metrics
            .has_configuration
            .store(true, Ordering::Release);
    }

    fn observe_cursors(&self, cursors: DeliveryCursors) {
        self.metrics
            .received_cursor
            .store(cursors.received().get(), Ordering::Relaxed);
        self.metrics
            .durable_cursor
            .store(cursors.durable().get(), Ordering::Relaxed);
        self.metrics
            .released_cursor
            .store(cursors.released().get(), Ordering::Relaxed);
    }
}

fn optional_counter(present: &AtomicBool, value: &AtomicU64) -> Option<u64> {
    present
        .load(Ordering::Acquire)
        .then(|| value.load(Ordering::Relaxed))
}

fn saturating_add(counter: &AtomicU64, amount: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(amount))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_core::{
        BootEpoch, ContinuityBreak, ContinuityBreakReason, Gap, GapReason, HardwareId,
        SampleSequence, SequenceCursor,
    };

    #[test]
    fn publishes_cursors_and_freshness_without_locks() {
        let session = NodeSession::recovered(
            HardwareId::from_bytes([1; 8]),
            BootEpoch::new(9),
            SequenceCursor::new(12),
            SequenceCursor::new(10),
        )
        .unwrap();
        let (publisher, reader) = NodeMetricsPublisher::new(2, 7, session.cursors());
        publisher.observe_session(&session, Some(15), MonotonicMicros::new(123));
        publisher.observe_time_sync(MonotonicMicros::new(456));
        publisher.observe_configuration(sensor_protocol::ObservedConfiguration {
            revision: 3,
            desired: sensor_protocol::MeasurementConfiguration::product_default(),
            sensor_odr: sensor_protocol::Adxl355Odr::Hz125,
            decimation_factor: 1,
        });

        assert_eq!(
            reader.snapshot(),
            NodeMetricsSnapshot {
                channel_id: 2,
                node_address: 7,
                received_cursor: 12,
                durable_cursor: 12,
                released_cursor: 10,
                produced_cursor: Some(15),
                last_measurement_monotonic_us: Some(123),
                last_time_sync_monotonic_us: Some(456),
                configuration_revision: Some(3),
                output_rate_code: Some(sensor_protocol::OutputRateCode::Hz125 as u8),
                persisted_sample_count: 0,
                gap_count: 0,
                missing_sample_count: 0,
                continuity_break_count: 0,
            }
        );
    }

    #[test]
    fn counts_written_samples_and_explicit_loss_separately() {
        let (publisher, reader) = NodeMetricsPublisher::new(2, 7, DeliveryCursors::empty());
        publisher.observe_persistence(PersistenceKind::SampleChunk { sample_count: 48 });
        publisher.observe_persistence(PersistenceKind::SampleChunk { sample_count: 17 });
        publisher.observe_persistence(PersistenceKind::Gap(
            Gap::new(
                HardwareId::from_bytes([1; 8]),
                BootEpoch::new(9),
                SampleSequence::new(65),
                11,
                GapReason::JournalOverflow,
            )
            .unwrap(),
        ));
        publisher.observe_persistence(PersistenceKind::ContinuityBreak(
            ContinuityBreak::new(
                HardwareId::from_bytes([1; 8]),
                BootEpoch::new(9),
                SequenceCursor::new(76),
                ContinuityBreakReason::ProcessingReset,
            )
            .unwrap(),
        ));

        let snapshot = reader.snapshot();
        assert_eq!(snapshot.persisted_sample_count, 65);
        assert_eq!(snapshot.gap_count, 1);
        assert_eq!(snapshot.missing_sample_count, 11);
        assert_eq!(snapshot.continuity_break_count, 1);
    }
}
