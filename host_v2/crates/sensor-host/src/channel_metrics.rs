use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::channel_agent::ChannelEvent;
use crate::channel_endpoint::ChannelEndpointEvent;
use crate::channel_recording_loop::ThreadedChannelRecordingEvent;
use crate::measurement_coordinator::{
    MeasurementSettlement, TelemetrySettlement, TimeSyncSettlement,
};
use crate::persistence_executor::GroupCommitMetricsReader;

/// Cumulative, saturating production counters for one physical channel.
///
/// The recording worker is the only writer. Readers only load atomics and
/// never acquire a lock or send work back into the RS485 path.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ChannelMetricsSnapshot {
    pub channel_id: u8,
    pub transmissions: u64,
    pub decode_errors: u64,
    pub disconnects: u64,
    pub transaction_timeouts: u64,
    pub wire_attempt_failures: u64,
    pub late_responses: u64,
    pub unrelated_responses: u64,
    pub unsolicited_responses: u64,
    pub measurement_persisted: u64,
    pub measurement_no_data: u64,
    pub measurement_retries: u64,
    pub measurement_recoveries: u64,
    pub persistence_submitted: u64,
    pub persistence_backpressure: u64,
    pub time_sync_submitted: u64,
    pub time_sync_retries: u64,
    pub time_sync_backpressure: u64,
    pub telemetry_snapshots: u64,
    pub telemetry_retries: u64,
    pub telemetry_backpressure: u64,
    pub maximum_step_us: u64,
    pub storage_committed_groups: u64,
    pub storage_committed_records: u64,
    pub storage_failed_groups: u64,
    pub storage_maximum_persist_us: u64,
    pub storage_maximum_group_records: u64,
    pub storage_queue_high_water: u64,
}

struct ChannelMetricsAtomic {
    transmissions: AtomicU64,
    decode_errors: AtomicU64,
    disconnects: AtomicU64,
    transaction_timeouts: AtomicU64,
    wire_attempt_failures: AtomicU64,
    late_responses: AtomicU64,
    unrelated_responses: AtomicU64,
    unsolicited_responses: AtomicU64,
    measurement_persisted: AtomicU64,
    measurement_no_data: AtomicU64,
    measurement_retries: AtomicU64,
    measurement_recoveries: AtomicU64,
    persistence_submitted: AtomicU64,
    persistence_backpressure: AtomicU64,
    time_sync_submitted: AtomicU64,
    time_sync_retries: AtomicU64,
    time_sync_backpressure: AtomicU64,
    telemetry_snapshots: AtomicU64,
    telemetry_retries: AtomicU64,
    telemetry_backpressure: AtomicU64,
    maximum_step_us: AtomicU64,
}

impl Default for ChannelMetricsAtomic {
    fn default() -> Self {
        Self {
            transmissions: AtomicU64::new(0),
            decode_errors: AtomicU64::new(0),
            disconnects: AtomicU64::new(0),
            transaction_timeouts: AtomicU64::new(0),
            wire_attempt_failures: AtomicU64::new(0),
            late_responses: AtomicU64::new(0),
            unrelated_responses: AtomicU64::new(0),
            unsolicited_responses: AtomicU64::new(0),
            measurement_persisted: AtomicU64::new(0),
            measurement_no_data: AtomicU64::new(0),
            measurement_retries: AtomicU64::new(0),
            measurement_recoveries: AtomicU64::new(0),
            persistence_submitted: AtomicU64::new(0),
            persistence_backpressure: AtomicU64::new(0),
            time_sync_submitted: AtomicU64::new(0),
            time_sync_retries: AtomicU64::new(0),
            time_sync_backpressure: AtomicU64::new(0),
            telemetry_snapshots: AtomicU64::new(0),
            telemetry_retries: AtomicU64::new(0),
            telemetry_backpressure: AtomicU64::new(0),
            maximum_step_us: AtomicU64::new(0),
        }
    }
}

#[derive(Clone)]
pub struct ChannelMetricsReader {
    channel_id: u8,
    metrics: Arc<ChannelMetricsAtomic>,
    storage: Option<GroupCommitMetricsReader>,
}

impl ChannelMetricsReader {
    pub fn snapshot(&self) -> ChannelMetricsSnapshot {
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        let storage = self
            .storage
            .as_ref()
            .map(GroupCommitMetricsReader::snapshot)
            .unwrap_or_default();
        ChannelMetricsSnapshot {
            channel_id: self.channel_id,
            transmissions: load(&self.metrics.transmissions),
            decode_errors: load(&self.metrics.decode_errors),
            disconnects: load(&self.metrics.disconnects),
            transaction_timeouts: load(&self.metrics.transaction_timeouts),
            wire_attempt_failures: load(&self.metrics.wire_attempt_failures),
            late_responses: load(&self.metrics.late_responses),
            unrelated_responses: load(&self.metrics.unrelated_responses),
            unsolicited_responses: load(&self.metrics.unsolicited_responses),
            measurement_persisted: load(&self.metrics.measurement_persisted),
            measurement_no_data: load(&self.metrics.measurement_no_data),
            measurement_retries: load(&self.metrics.measurement_retries),
            measurement_recoveries: load(&self.metrics.measurement_recoveries),
            persistence_submitted: load(&self.metrics.persistence_submitted),
            persistence_backpressure: load(&self.metrics.persistence_backpressure),
            time_sync_submitted: load(&self.metrics.time_sync_submitted),
            time_sync_retries: load(&self.metrics.time_sync_retries),
            time_sync_backpressure: load(&self.metrics.time_sync_backpressure),
            telemetry_snapshots: load(&self.metrics.telemetry_snapshots),
            telemetry_retries: load(&self.metrics.telemetry_retries),
            telemetry_backpressure: load(&self.metrics.telemetry_backpressure),
            maximum_step_us: load(&self.metrics.maximum_step_us),
            storage_committed_groups: storage.committed_groups,
            storage_committed_records: storage.committed_records,
            storage_failed_groups: storage.failed_groups,
            storage_maximum_persist_us: storage.maximum_persist_us,
            storage_maximum_group_records: storage.maximum_group_records,
            storage_queue_high_water: storage.queue_high_water,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ChannelMetricsPublisher {
    metrics: Arc<ChannelMetricsAtomic>,
}

impl ChannelMetricsPublisher {
    pub(crate) fn new(
        channel_id: u8,
        storage: Option<GroupCommitMetricsReader>,
    ) -> (Self, ChannelMetricsReader) {
        let metrics = Arc::new(ChannelMetricsAtomic::default());
        (
            Self {
                metrics: Arc::clone(&metrics),
            },
            ChannelMetricsReader {
                channel_id,
                metrics,
                storage,
            },
        )
    }

    pub(crate) fn observe_step(&self, event: &ThreadedChannelRecordingEvent, elapsed: Duration) {
        self.observe_event(event);
        let elapsed_us = elapsed.as_micros().min(u128::from(u64::MAX)) as u64;
        self.metrics
            .maximum_step_us
            .fetch_max(elapsed_us, Ordering::Relaxed);
    }

    fn observe_event(&self, event: &ThreadedChannelRecordingEvent) {
        match event {
            ThreadedChannelRecordingEvent::Endpoint(event) => self.observe_endpoint(event),
            ThreadedChannelRecordingEvent::Measurement(settlement) => match settlement {
                MeasurementSettlement::Persisted { .. } => {
                    increment(&self.metrics.measurement_persisted)
                }
                MeasurementSettlement::NoData { .. } => {
                    increment(&self.metrics.measurement_no_data)
                }
                MeasurementSettlement::RetryScheduled { .. } => {
                    increment(&self.metrics.measurement_retries)
                }
                MeasurementSettlement::RecoveryRequired { .. } => {
                    increment(&self.metrics.measurement_recoveries)
                }
            },
            ThreadedChannelRecordingEvent::PersistenceSubmitted { .. } => {
                increment(&self.metrics.persistence_submitted)
            }
            ThreadedChannelRecordingEvent::PersistenceBackpressured => {
                increment(&self.metrics.persistence_backpressure)
            }
            ThreadedChannelRecordingEvent::TimeSync(settlement) => match settlement {
                TimeSyncSettlement::Submitted { .. } => {
                    increment(&self.metrics.time_sync_submitted)
                }
                TimeSyncSettlement::RetryScheduled { .. } => {
                    increment(&self.metrics.time_sync_retries)
                }
                TimeSyncSettlement::Backpressured { .. } => {
                    increment(&self.metrics.time_sync_backpressure)
                }
            },
            ThreadedChannelRecordingEvent::Telemetry(settlement) => match settlement {
                TelemetrySettlement::Submitted { .. } => {
                    increment(&self.metrics.telemetry_snapshots)
                }
                TelemetrySettlement::RetryScheduled { .. } => {
                    increment(&self.metrics.telemetry_retries)
                }
                TelemetrySettlement::Backpressured { .. } => {
                    increment(&self.metrics.telemetry_backpressure)
                }
            },
            ThreadedChannelRecordingEvent::RecoveryDue(_) => {}
        }
    }

    fn observe_endpoint(&self, event: &ChannelEndpointEvent) {
        match event {
            ChannelEndpointEvent::TransmissionComplete { .. } => {
                increment(&self.metrics.transmissions)
            }
            ChannelEndpointEvent::DecodeError(_) => increment(&self.metrics.decode_errors),
            ChannelEndpointEvent::Disconnected => increment(&self.metrics.disconnects),
            ChannelEndpointEvent::Agent(event) => match event {
                ChannelEvent::HandshakeTimedOut { .. }
                | ChannelEvent::StreamStateTimedOut { .. }
                | ChannelEvent::MeasurementTimedOut { .. }
                | ChannelEvent::TimeSyncTimedOut { .. }
                | ChannelEvent::TelemetryTimedOut { .. }
                | ChannelEvent::LifecycleTimedOut { .. } => {
                    increment(&self.metrics.transaction_timeouts)
                }
                ChannelEvent::WireAttemptFailed { .. } => {
                    increment(&self.metrics.wire_attempt_failures)
                }
                ChannelEvent::LateResponse => increment(&self.metrics.late_responses),
                ChannelEvent::UnrelatedResponse => increment(&self.metrics.unrelated_responses),
                ChannelEvent::UnsolicitedResponse => increment(&self.metrics.unsolicited_responses),
                _ => {}
            },
            ChannelEndpointEvent::Idle => {}
        }
    }
}

fn increment(counter: &AtomicU64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(1))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement_coordinator::TimeSyncSettlement;

    #[test]
    fn records_cumulative_events_and_maximum_step_without_locks() {
        let (publisher, reader) = ChannelMetricsPublisher::new(7, None);
        publisher.observe_step(
            &ThreadedChannelRecordingEvent::Endpoint(ChannelEndpointEvent::Agent(
                ChannelEvent::MeasurementTimedOut { node_address: 1 },
            )),
            Duration::from_micros(40),
        );
        publisher.observe_step(
            &ThreadedChannelRecordingEvent::TimeSync(TimeSyncSettlement::RetryScheduled {
                node_address: 1,
            }),
            Duration::from_micros(20),
        );
        publisher.observe_step(
            &ThreadedChannelRecordingEvent::PersistenceBackpressured,
            Duration::from_micros(70),
        );

        let snapshot = reader.snapshot();
        assert_eq!(snapshot.channel_id, 7);
        assert_eq!(snapshot.transaction_timeouts, 1);
        assert_eq!(snapshot.time_sync_retries, 1);
        assert_eq!(snapshot.persistence_backpressure, 1);
        assert_eq!(snapshot.maximum_step_us, 70);
    }
}
