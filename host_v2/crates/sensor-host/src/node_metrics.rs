use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use sensor_core::DeliveryCursors;

use crate::channel_runtime::{NodeAvailability, OfflineCause, RecoveryCause};
use crate::measurement_transfer::PersistenceKind;
use crate::node_session::NodeSession;
use crate::transaction::MonotonicMicros;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeMetricsSnapshot {
    pub channel_id: u8,
    pub node_address: u8,
    pub boot_epoch: Option<u64>,
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
    pub availability: NodeAvailability,
    pub latest_telemetry: Option<NodeTelemetryMetricsSnapshot>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeTelemetryMetricsSnapshot {
    pub observed_monotonic_us: u64,
    pub captured_device_time_us: u64,
    pub adxl355_temperature_raw: u16,
}

#[derive(Default)]
struct NodeMetricsAtomic {
    boot_epoch: AtomicU64,
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
    availability_code: AtomicU64,
    availability_failures: AtomicU64,
    recovery_cause_code: AtomicU64,
    telemetry_generation: AtomicU64,
    telemetry_observed_monotonic_us: AtomicU64,
    telemetry_captured_device_time_us: AtomicU64,
    telemetry_adxl355_temperature_raw: AtomicU64,
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
            boot_epoch: match self.metrics.boot_epoch.load(Ordering::Acquire) {
                0 => None,
                value => Some(value),
            },
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
            availability: decode_availability(
                load(&self.metrics.availability_code),
                load(&self.metrics.availability_failures) as u8,
                load(&self.metrics.recovery_cause_code),
            ),
            latest_telemetry: self.latest_telemetry(),
        }
    }

    fn latest_telemetry(&self) -> Option<NodeTelemetryMetricsSnapshot> {
        // The channel worker is the only writer. A bounded seqlock read keeps
        // status publication non-blocking while preventing mixed snapshots.
        for _ in 0..3 {
            let before = self.metrics.telemetry_generation.load(Ordering::Acquire);
            if before == 0 {
                return None;
            }
            if before & 1 != 0 {
                continue;
            }
            let snapshot = NodeTelemetryMetricsSnapshot {
                observed_monotonic_us: self
                    .metrics
                    .telemetry_observed_monotonic_us
                    .load(Ordering::Relaxed),
                captured_device_time_us: self
                    .metrics
                    .telemetry_captured_device_time_us
                    .load(Ordering::Relaxed),
                adxl355_temperature_raw: self
                    .metrics
                    .telemetry_adxl355_temperature_raw
                    .load(Ordering::Relaxed)
                    .try_into()
                    .expect("telemetry temperature originates as u16"),
            };
            let after = self.metrics.telemetry_generation.load(Ordering::Acquire);
            if before == after {
                return Some(snapshot);
            }
        }
        None
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
        self.metrics
            .boot_epoch
            .store(session.boot_epoch().get(), Ordering::Release);
    }

    pub(crate) fn observe_session_cursors(
        &self,
        session: &NodeSession,
        produced_cursor: Option<u64>,
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
            .boot_epoch
            .store(session.boot_epoch().get(), Ordering::Release);
    }

    pub(crate) fn observe_time_sync(&self, now: MonotonicMicros) {
        self.metrics
            .last_time_sync_monotonic_us
            .store(now.get(), Ordering::Relaxed);
        self.metrics.has_time_sync.store(true, Ordering::Release);
    }

    pub(crate) fn observe_availability(&self, availability: NodeAvailability) {
        let (code, failures, cause) = encode_availability(availability);
        self.metrics
            .availability_failures
            .store(u64::from(failures), Ordering::Relaxed);
        self.metrics
            .recovery_cause_code
            .store(cause, Ordering::Relaxed);
        self.metrics
            .availability_code
            .store(code, Ordering::Release);
    }

    pub(crate) fn observe_telemetry(
        &self,
        snapshot: sensor_protocol::TelemetrySnapshot,
        now: MonotonicMicros,
    ) {
        self.metrics
            .telemetry_generation
            .fetch_add(1, Ordering::AcqRel);
        self.metrics
            .telemetry_observed_monotonic_us
            .store(now.get(), Ordering::Relaxed);
        self.metrics
            .telemetry_captured_device_time_us
            .store(snapshot.captured_device_time_us, Ordering::Relaxed);
        self.metrics.telemetry_adxl355_temperature_raw.store(
            u64::from(snapshot.adxl355_temperature.get()),
            Ordering::Relaxed,
        );
        self.metrics
            .telemetry_generation
            .fetch_add(1, Ordering::Release);
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

fn encode_recovery_cause(cause: RecoveryCause) -> u64 {
    match cause {
        RecoveryCause::StreamChanged => 1,
        RecoveryCause::InvalidRelease => 2,
        RecoveryCause::InvalidResponse => 3,
        RecoveryCause::StreamRegressed => 4,
    }
}

fn decode_recovery_cause(value: u64) -> RecoveryCause {
    match value {
        1 => RecoveryCause::StreamChanged,
        2 => RecoveryCause::InvalidRelease,
        4 => RecoveryCause::StreamRegressed,
        _ => RecoveryCause::InvalidResponse,
    }
}

fn encode_availability(availability: NodeAvailability) -> (u64, u8, u64) {
    match availability {
        NodeAvailability::Online => (0, 0, 0),
        NodeAvailability::RetryBackoff { failures } => (1, failures, 0),
        NodeAvailability::Recovering { cause, failures } => {
            (2, failures, encode_recovery_cause(cause))
        }
        NodeAvailability::Offline {
            cause: OfflineCause::Unresponsive,
            failures,
        } => (3, failures, 0),
        NodeAvailability::Offline {
            cause: OfflineCause::Recovery(cause),
            failures,
        } => (4, failures, encode_recovery_cause(cause)),
    }
}

fn decode_availability(code: u64, failures: u8, cause: u64) -> NodeAvailability {
    match code {
        1 => NodeAvailability::RetryBackoff { failures },
        2 => NodeAvailability::Recovering {
            cause: decode_recovery_cause(cause),
            failures,
        },
        3 => NodeAvailability::Offline {
            cause: OfflineCause::Unresponsive,
            failures,
        },
        4 => NodeAvailability::Offline {
            cause: OfflineCause::Recovery(decode_recovery_cause(cause)),
            failures,
        },
        _ => NodeAvailability::Online,
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
                boot_epoch: Some(9),
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
                availability: NodeAvailability::Online,
                latest_telemetry: None,
            }
        );
    }

    #[test]
    fn publishes_typed_recovery_and_offline_state() {
        let (publisher, reader) = NodeMetricsPublisher::new(2, 7, DeliveryCursors::empty());
        publisher.observe_availability(NodeAvailability::Recovering {
            cause: RecoveryCause::InvalidRelease,
            failures: 2,
        });
        assert_eq!(
            reader.snapshot().availability,
            NodeAvailability::Recovering {
                cause: RecoveryCause::InvalidRelease,
                failures: 2,
            }
        );
        publisher.observe_availability(NodeAvailability::Offline {
            cause: OfflineCause::Recovery(RecoveryCause::InvalidRelease),
            failures: 4,
        });
        assert_eq!(
            reader.snapshot().availability,
            NodeAvailability::Offline {
                cause: OfflineCause::Recovery(RecoveryCause::InvalidRelease),
                failures: 4,
            }
        );
    }

    #[test]
    fn publishes_the_current_session_epoch_after_rollover() {
        let (publisher, reader) = NodeMetricsPublisher::new(2, 7, DeliveryCursors::empty());
        let first = NodeSession::recovered(
            HardwareId::from_bytes([1; 8]),
            BootEpoch::new(9),
            SequenceCursor::new(12),
            SequenceCursor::new(10),
        )
        .unwrap();
        publisher.observe_session_cursors(&first, Some(15));
        assert_eq!(reader.snapshot().boot_epoch, Some(9));

        let replacement = NodeSession::recovered(
            HardwareId::from_bytes([1; 8]),
            BootEpoch::new(10),
            SequenceCursor::new(4),
            SequenceCursor::new(3),
        )
        .unwrap();
        publisher.observe_session_cursors(&replacement, Some(6));
        let snapshot = reader.snapshot();
        assert_eq!(snapshot.boot_epoch, Some(10));
        assert_eq!(snapshot.received_cursor, 4);
        assert_eq!(snapshot.produced_cursor, Some(6));
    }

    #[test]
    fn publishes_a_coherent_temperature_snapshot_without_locks() {
        let (publisher, reader) = NodeMetricsPublisher::new(2, 7, DeliveryCursors::empty());
        assert_eq!(reader.snapshot().latest_telemetry, None);

        publisher.observe_telemetry(
            sensor_protocol::TelemetrySnapshot {
                captured_device_time_us: 123_456,
                adxl355_temperature: sensor_protocol::Adxl355TemperatureCode::new(1885).unwrap(),
            },
            MonotonicMicros::new(987_654),
        );

        assert_eq!(
            reader.snapshot().latest_telemetry,
            Some(NodeTelemetryMetricsSnapshot {
                observed_monotonic_us: 987_654,
                captured_device_time_us: 123_456,
                adxl355_temperature_raw: 1885,
            })
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
