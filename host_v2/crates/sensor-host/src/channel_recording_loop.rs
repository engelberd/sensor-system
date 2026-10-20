use core::fmt;
use std::io::{Read, Write};

use crate::bounded_writer::{
    GroupDurableMeasurementSink, PersistenceSubmitter, WriteCompletion, WriteJobId,
};
use crate::channel_agent::ChannelEvent;
use crate::channel_endpoint::{
    ChannelEndpoint, ChannelEndpointError, ChannelEndpointEvent, MonotonicClock,
};
use crate::measurement_coordinator::{
    ChannelWorkCoordinator, MeasurementCoordinatorError, MeasurementHandoffError,
    MeasurementSettlement, MeasurementSettlementError, MeasurementWriteSettlementError,
    TelemetrySettlement, TimeSyncSettlement,
};
use crate::measurement_persistence::DurableMeasurementSink;
use crate::persistence_executor::{
    classify_recovery, GroupedExecutorRecoveryError, GroupedPersistenceExecutor,
    PersistenceExecutor, PersistenceExecutorError, RecoveredCursorEvidence,
    RecoveryClassificationError, WriteRecoveryRange,
};
use crate::telemetry_persistence::{
    RejectingTelemetryObservationSubmitter, TelemetryObservationSubmitter,
};
use crate::time_persistence::{RejectingTimeObservationSubmitter, TimeObservationSubmitter};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChannelRecordingEvent {
    Endpoint(ChannelEndpointEvent),
    Measurement(MeasurementSettlement),
    TimeSync(TimeSyncSettlement),
    Telemetry(TelemetrySettlement),
}

/// One production recording lane for one communication channel.
///
/// A call performs at most one wire action or one persistence settlement. The
/// caller owns pacing and shutdown; no hidden thread or unbounded queue exists.
pub struct ChannelRecordingLoop<P, C, S, T = RejectingTimeObservationSubmitter> {
    endpoint: ChannelEndpoint<P, C>,
    measurements: ChannelWorkCoordinator,
    sink: S,
    time_observations: T,
    telemetry_observations: Box<dyn TelemetryObservationSubmitter>,
}

impl<P, C, S> ChannelRecordingLoop<P, C, S, RejectingTimeObservationSubmitter>
where
    P: Read + Write,
    C: MonotonicClock,
    S: DurableMeasurementSink,
{
    pub fn new(
        endpoint: ChannelEndpoint<P, C>,
        measurements: ChannelWorkCoordinator,
        sink: S,
    ) -> Self {
        Self {
            endpoint,
            measurements,
            sink,
            time_observations: RejectingTimeObservationSubmitter,
            telemetry_observations: Box::new(RejectingTelemetryObservationSubmitter),
        }
    }
}

impl<P, C, S, T> ChannelRecordingLoop<P, C, S, T>
where
    P: Read + Write,
    C: MonotonicClock,
    S: DurableMeasurementSink,
    T: TimeObservationSubmitter,
{
    pub fn new_with_time_observations(
        endpoint: ChannelEndpoint<P, C>,
        measurements: ChannelWorkCoordinator,
        sink: S,
        time_observations: T,
    ) -> Self {
        Self {
            endpoint,
            measurements,
            sink,
            time_observations,
            telemetry_observations: Box::new(RejectingTelemetryObservationSubmitter),
        }
    }

    pub fn with_telemetry_observations(
        mut self,
        telemetry_observations: Box<dyn TelemetryObservationSubmitter>,
    ) -> Self {
        self.telemetry_observations = telemetry_observations;
        self
    }

    pub fn step(&mut self) -> Result<ChannelRecordingEvent, ChannelRecordingLoopError<S::Error>> {
        let now = self.endpoint.now();
        if self.measurements.has_pending_completion() {
            let settlement = self
                .measurements
                .settle_completed(self.endpoint.agent_mut(), &mut self.sink, now)
                .map_err(ChannelRecordingLoopError::Settlement)?;
            return Ok(ChannelRecordingEvent::Measurement(settlement));
        }

        if !self.measurements.has_active_dispatch() {
            self.measurements
                .dispatch_next_work(self.endpoint.agent_mut(), now)
                .map_err(ChannelRecordingLoopError::Coordinator)?;
        }

        let endpoint_event = self
            .endpoint
            .step()
            .map_err(ChannelRecordingLoopError::Endpoint)?;
        if let ChannelEndpointEvent::Agent(agent_event) = &endpoint_event {
            let settled_at = self.endpoint.now();
            if let Some(settlement) = self
                .measurements
                .settle_time_sync_event(agent_event, &mut self.time_observations, settled_at)
                .map_err(ChannelRecordingLoopError::Coordinator)?
            {
                return Ok(ChannelRecordingEvent::TimeSync(settlement));
            }
            if let Some(settlement) = self
                .measurements
                .settle_telemetry_event(
                    agent_event,
                    self.telemetry_observations.as_mut(),
                    settled_at,
                )
                .map_err(ChannelRecordingLoopError::Coordinator)?
            {
                return Ok(ChannelRecordingEvent::Telemetry(settlement));
            }
        }
        if matches!(
            endpoint_event,
            ChannelEndpointEvent::Agent(
                ChannelEvent::MeasurementNoData { .. }
                    | ChannelEvent::MeasurementPersistenceRequired { .. }
                    | ChannelEvent::MeasurementRecoveryRequired { .. }
                    | ChannelEvent::MeasurementAttemptsExhausted { .. }
                    | ChannelEvent::MeasurementHandoffPending { .. }
            )
        ) {
            let settled_at = self.endpoint.now();
            let settlement = self
                .measurements
                .settle_completed(self.endpoint.agent_mut(), &mut self.sink, settled_at)
                .map_err(ChannelRecordingLoopError::Settlement)?;
            Ok(ChannelRecordingEvent::Measurement(settlement))
        } else {
            Ok(ChannelRecordingEvent::Endpoint(endpoint_event))
        }
    }

    pub fn measurements(&self) -> &ChannelWorkCoordinator {
        &self.measurements
    }

    pub fn endpoint_mut(&mut self) -> &mut ChannelEndpoint<P, C> {
        &mut self.endpoint
    }

    pub fn begin_quiesce(&mut self) {
        self.measurements.begin_quiesce();
    }

    pub fn is_quiescent(&self) -> bool {
        self.measurements.is_quiescent()
    }

    pub fn begin_shutdown_drain(&mut self) -> Result<(), MeasurementCoordinatorError> {
        self.measurements.begin_shutdown_drain()
    }

    pub fn is_shutdown_drain_complete(&self) -> bool {
        self.measurements.is_shutdown_drain_complete()
    }

    pub fn finish_shutdown_drain(&mut self) -> Result<(), MeasurementCoordinatorError> {
        self.measurements.finish_shutdown_drain()
    }

    pub fn abort_shutdown_drain(&mut self) -> Result<(), MeasurementCoordinatorError> {
        self.measurements.abort_shutdown_drain()
    }

    pub fn resume_scheduling(&mut self) -> Result<(), MeasurementCoordinatorError> {
        self.measurements.resume_scheduling()
    }

    pub fn reconfigure_node(
        &mut self,
        config: crate::channel_runtime::NodeScheduleConfig,
    ) -> Result<(), MeasurementCoordinatorError> {
        let now = self.endpoint.now();
        self.measurements.reconfigure_node(config, now)
    }

    pub fn observe_configuration(
        &self,
        node_address: u8,
        configuration: sensor_protocol::ObservedConfiguration,
    ) -> Result<(), MeasurementCoordinatorError> {
        self.measurements
            .observe_configuration(node_address, configuration)
    }

    pub fn into_parts(
        self,
    ) -> (
        ChannelEndpoint<P, C>,
        ChannelWorkCoordinator,
        S,
        T,
        Box<dyn TelemetryObservationSubmitter>,
    ) {
        (
            self.endpoint,
            self.measurements,
            self.sink,
            self.time_observations,
            self.telemetry_observations,
        )
    }
}

#[derive(Debug)]
pub enum ChannelRecordingLoopError<E> {
    Endpoint(ChannelEndpointError),
    Coordinator(MeasurementCoordinatorError),
    Settlement(MeasurementSettlementError<E>),
}

impl<E> fmt::Display for ChannelRecordingLoopError<E>
where
    E: fmt::Display,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Endpoint(error) => write!(formatter, "channel endpoint failed: {error}"),
            Self::Coordinator(error) => write!(formatter, "measurement dispatch failed: {error}"),
            Self::Settlement(error) => write!(formatter, "measurement settlement failed: {error}"),
        }
    }
}

impl<E> std::error::Error for ChannelRecordingLoopError<E> where E: std::error::Error + 'static {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThreadedChannelRecordingEvent {
    Endpoint(ChannelEndpointEvent),
    Measurement(MeasurementSettlement),
    PersistenceSubmitted {
        node_address: u8,
        job_id: crate::bounded_writer::WriteJobId,
    },
    PersistenceBackpressured,
    TimeSync(TimeSyncSettlement),
    Telemetry(TelemetrySettlement),
}

pub trait RecordingPersistence: PersistenceSubmitter {
    type Sink: DurableMeasurementSink;

    fn try_recording_completion(
        &self,
    ) -> Result<
        Option<WriteCompletion<<Self::Sink as DurableMeasurementSink>::Error>>,
        PersistenceExecutorError,
    >;

    fn recover_recording_sink(
        &self,
        sink: Self::Sink,
        pending: WriteRecoveryRange,
        recovered: RecoveredCursorEvidence,
    ) -> Result<(), PersistenceRecoveryError>;
}

impl<S> RecordingPersistence for PersistenceExecutor<S>
where
    S: DurableMeasurementSink + Send + 'static,
    S::Error: Send + 'static,
{
    type Sink = S;

    fn try_recording_completion(
        &self,
    ) -> Result<Option<WriteCompletion<S::Error>>, PersistenceExecutorError> {
        self.try_completion()
    }

    fn recover_recording_sink(
        &self,
        sink: S,
        pending: WriteRecoveryRange,
        recovered: RecoveredCursorEvidence,
    ) -> Result<(), PersistenceRecoveryError> {
        let disposition =
            classify_recovery(pending, recovered).map_err(PersistenceRecoveryError::Classify)?;
        self.replace_recovered_sink(sink, disposition)
            .map_err(PersistenceRecoveryError::Executor)
    }
}

impl<S> RecordingPersistence for GroupedPersistenceExecutor<S>
where
    S: GroupDurableMeasurementSink + Send + 'static,
    S::Error: Send + 'static,
{
    type Sink = S;

    fn try_recording_completion(
        &self,
    ) -> Result<Option<WriteCompletion<S::Error>>, PersistenceExecutorError> {
        self.try_completion()
    }

    fn recover_recording_sink(
        &self,
        sink: S,
        _pending: WriteRecoveryRange,
        recovered: RecoveredCursorEvidence,
    ) -> Result<(), PersistenceRecoveryError> {
        self.replace_recovered_sink(sink, recovered)
            .map(|_| ())
            .map_err(PersistenceRecoveryError::Grouped)
    }
}

/// Production lane whose disk I/O runs exclusively on a bounded executor.
/// A step only exchanges bounded queue messages or performs one wire action.
pub struct ThreadedChannelRecordingLoop<P, C, E, T = RejectingTimeObservationSubmitter>
where
    E: RecordingPersistence,
{
    endpoint: ChannelEndpoint<P, C>,
    measurements: ChannelWorkCoordinator,
    persistence: E,
    time_observations: T,
    telemetry_observations: Box<dyn TelemetryObservationSubmitter>,
}

impl<P, C, E> ThreadedChannelRecordingLoop<P, C, E, RejectingTimeObservationSubmitter>
where
    P: Read + Write,
    C: MonotonicClock,
    E: RecordingPersistence,
    <E::Sink as DurableMeasurementSink>::Error: Send + 'static,
{
    pub fn new(
        endpoint: ChannelEndpoint<P, C>,
        measurements: ChannelWorkCoordinator,
        persistence: E,
    ) -> Self {
        Self {
            endpoint,
            measurements,
            persistence,
            time_observations: RejectingTimeObservationSubmitter,
            telemetry_observations: Box::new(RejectingTelemetryObservationSubmitter),
        }
    }
}

impl<P, C, E, T> ThreadedChannelRecordingLoop<P, C, E, T>
where
    P: Read + Write,
    C: MonotonicClock,
    E: RecordingPersistence,
    T: TimeObservationSubmitter,
    <E::Sink as DurableMeasurementSink>::Error: Send + 'static,
{
    pub fn new_with_time_observations(
        endpoint: ChannelEndpoint<P, C>,
        measurements: ChannelWorkCoordinator,
        persistence: E,
        time_observations: T,
    ) -> Self {
        Self {
            endpoint,
            measurements,
            persistence,
            time_observations,
            telemetry_observations: Box::new(RejectingTelemetryObservationSubmitter),
        }
    }

    pub fn with_telemetry_observations(
        mut self,
        telemetry_observations: Box<dyn TelemetryObservationSubmitter>,
    ) -> Self {
        self.telemetry_observations = telemetry_observations;
        self
    }

    pub fn step(
        &mut self,
    ) -> Result<
        ThreadedChannelRecordingEvent,
        ThreadedChannelRecordingLoopError<<E::Sink as DurableMeasurementSink>::Error>,
    > {
        let now = self.endpoint.now();
        if let Some(completion) = self
            .persistence
            .try_recording_completion()
            .map_err(ThreadedChannelRecordingLoopError::Executor)?
        {
            let settlement = self
                .measurements
                .settle_write_completion(completion, now)
                .map_err(ThreadedChannelRecordingLoopError::WriteSettlement)?;
            return Ok(ThreadedChannelRecordingEvent::Measurement(settlement));
        }

        if self.measurements.has_pending_completion() {
            return self.try_handoff();
        }

        if !self.measurements.has_active_dispatch() {
            self.measurements
                .dispatch_next_work(self.endpoint.agent_mut(), now)
                .map_err(ThreadedChannelRecordingLoopError::Coordinator)?;
        }

        let endpoint_event = self
            .endpoint
            .step()
            .map_err(ThreadedChannelRecordingLoopError::Endpoint)?;
        if let ChannelEndpointEvent::Agent(agent_event) = &endpoint_event {
            let settled_at = self.endpoint.now();
            if let Some(settlement) = self
                .measurements
                .settle_time_sync_event(agent_event, &mut self.time_observations, settled_at)
                .map_err(ThreadedChannelRecordingLoopError::Coordinator)?
            {
                return Ok(ThreadedChannelRecordingEvent::TimeSync(settlement));
            }
            if let Some(settlement) = self
                .measurements
                .settle_telemetry_event(
                    agent_event,
                    self.telemetry_observations.as_mut(),
                    settled_at,
                )
                .map_err(ThreadedChannelRecordingLoopError::Coordinator)?
            {
                return Ok(ThreadedChannelRecordingEvent::Telemetry(settlement));
            }
        }
        match endpoint_event {
            ChannelEndpointEvent::Agent(
                ChannelEvent::MeasurementPersistenceRequired { .. }
                | ChannelEvent::MeasurementHandoffPending { .. },
            ) => self.try_handoff(),
            ChannelEndpointEvent::Agent(
                ChannelEvent::MeasurementNoData { .. }
                | ChannelEvent::MeasurementRecoveryRequired { .. }
                | ChannelEvent::MeasurementAttemptsExhausted { .. },
            ) => {
                let mut sink = NoPersistenceExpected;
                let settled_at = self.endpoint.now();
                let settlement = self
                    .measurements
                    .settle_completed(self.endpoint.agent_mut(), &mut sink, settled_at)
                    .map_err(|error| match error {
                        MeasurementSettlementError::Coordinator(error) => {
                            ThreadedChannelRecordingLoopError::Coordinator(error)
                        }
                        MeasurementSettlementError::Persistence(_) => {
                            ThreadedChannelRecordingLoopError::UnexpectedPersistence
                        }
                    })?;
                Ok(ThreadedChannelRecordingEvent::Measurement(settlement))
            }
            event => Ok(ThreadedChannelRecordingEvent::Endpoint(event)),
        }
    }

    pub fn recover_persistence(
        &self,
        job_id: WriteJobId,
        sink: E::Sink,
        recovered: RecoveredCursorEvidence,
    ) -> Result<(), PersistenceRecoveryError> {
        let pending = self
            .measurements
            .write_recovery_range(job_id)
            .map_err(PersistenceRecoveryError::Coordinator)?;
        self.persistence
            .recover_recording_sink(sink, pending, recovered)
    }

    pub const fn persistence(&self) -> &E {
        &self.persistence
    }

    pub fn measurements(&self) -> &ChannelWorkCoordinator {
        &self.measurements
    }

    pub fn endpoint_mut(&mut self) -> &mut ChannelEndpoint<P, C> {
        &mut self.endpoint
    }

    pub fn begin_quiesce(&mut self) {
        self.measurements.begin_quiesce();
    }

    pub fn is_quiescent(&self) -> bool {
        self.measurements.is_quiescent()
    }

    pub fn begin_shutdown_drain(&mut self) -> Result<(), MeasurementCoordinatorError> {
        self.measurements.begin_shutdown_drain()
    }

    pub fn is_shutdown_drain_complete(&self) -> bool {
        self.measurements.is_shutdown_drain_complete()
    }

    pub fn finish_shutdown_drain(&mut self) -> Result<(), MeasurementCoordinatorError> {
        self.measurements.finish_shutdown_drain()
    }

    pub fn abort_shutdown_drain(&mut self) -> Result<(), MeasurementCoordinatorError> {
        self.measurements.abort_shutdown_drain()
    }

    pub fn resume_scheduling(&mut self) -> Result<(), MeasurementCoordinatorError> {
        self.measurements.resume_scheduling()
    }

    pub fn reconfigure_node(
        &mut self,
        config: crate::channel_runtime::NodeScheduleConfig,
    ) -> Result<(), MeasurementCoordinatorError> {
        let now = self.endpoint.now();
        self.measurements.reconfigure_node(config, now)
    }

    pub fn observe_configuration(
        &self,
        node_address: u8,
        configuration: sensor_protocol::ObservedConfiguration,
    ) -> Result<(), MeasurementCoordinatorError> {
        self.measurements
            .observe_configuration(node_address, configuration)
    }

    pub fn reconcile_resume_stream(
        &mut self,
        node_address: u8,
        journal_capacity_samples: u32,
        stream: sensor_protocol::StreamState,
    ) -> Result<crate::stream_recovery::RecoveryContinuity, MeasurementCoordinatorError> {
        let now = self.endpoint.now();
        self.measurements.reconcile_resume_stream(
            node_address,
            journal_capacity_samples,
            stream,
            now,
        )
    }

    pub fn replace_resume_stream(
        &mut self,
        node_address: u8,
        expected: crate::stream_recovery::ExpectedStreamState,
        durable_cursor: sensor_core::SequenceCursor,
        stream: sensor_protocol::StreamState,
    ) -> Result<crate::stream_recovery::RecoveryContinuity, MeasurementCoordinatorError> {
        let now = self.endpoint.now();
        self.measurements
            .replace_resume_stream(node_address, expected, durable_cursor, stream, now)
    }

    pub fn into_parts(
        self,
    ) -> (
        ChannelEndpoint<P, C>,
        ChannelWorkCoordinator,
        E,
        T,
        Box<dyn TelemetryObservationSubmitter>,
    ) {
        (
            self.endpoint,
            self.measurements,
            self.persistence,
            self.time_observations,
            self.telemetry_observations,
        )
    }

    fn try_handoff(
        &mut self,
    ) -> Result<
        ThreadedChannelRecordingEvent,
        ThreadedChannelRecordingLoopError<<E::Sink as DurableMeasurementSink>::Error>,
    > {
        let node_address = self
            .measurements
            .pending_completion_node()
            .or_else(|| self.endpoint.agent().completed_measurement_node())
            .ok_or(ThreadedChannelRecordingLoopError::Coordinator(
                MeasurementCoordinatorError::CompletedTransferUnavailable,
            ))?;
        match self
            .measurements
            .handoff_completed(self.endpoint.agent_mut(), &mut self.persistence)
        {
            Ok(job_id) => Ok(ThreadedChannelRecordingEvent::PersistenceSubmitted {
                node_address,
                job_id,
            }),
            Err(MeasurementHandoffError::Submit(
                crate::bounded_writer::SubmitErrorKind::QueueFull,
            )) => Ok(ThreadedChannelRecordingEvent::PersistenceBackpressured),
            Err(error) => Err(ThreadedChannelRecordingLoopError::Handoff(error)),
        }
    }
}

#[derive(Debug)]
struct UnexpectedPersistence;

impl fmt::Display for UnexpectedPersistence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("persistence was not expected in this settlement path")
    }
}

impl std::error::Error for UnexpectedPersistence {}

struct NoPersistenceExpected;

impl DurableMeasurementSink for NoPersistenceExpected {
    type Error = UnexpectedPersistence;

    fn persist_chunk(
        &mut self,
        _hardware_id: sensor_core::HardwareId,
        _chunk: &sensor_protocol::SampleChunk,
    ) -> Result<(), Self::Error> {
        Err(UnexpectedPersistence)
    }

    fn persist_gap(&mut self, _gap: sensor_core::Gap) -> Result<(), Self::Error> {
        Err(UnexpectedPersistence)
    }

    fn persist_continuity_break(
        &mut self,
        _observed: sensor_core::ContinuityBreak,
    ) -> Result<(), Self::Error> {
        Err(UnexpectedPersistence)
    }
}

#[derive(Debug)]
pub enum ThreadedChannelRecordingLoopError<E> {
    Endpoint(ChannelEndpointError),
    Coordinator(MeasurementCoordinatorError),
    Handoff(MeasurementHandoffError),
    Executor(PersistenceExecutorError),
    WriteSettlement(MeasurementWriteSettlementError<E>),
    UnexpectedPersistence,
}

impl<E> fmt::Display for ThreadedChannelRecordingLoopError<E>
where
    E: fmt::Display,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Endpoint(error) => write!(formatter, "channel endpoint failed: {error}"),
            Self::Coordinator(error) => write!(formatter, "measurement failed: {error}"),
            Self::Handoff(error) => write!(formatter, "persistence handoff failed: {error}"),
            Self::Executor(error) => write!(formatter, "persistence executor failed: {error}"),
            Self::WriteSettlement(error) => write!(formatter, "write settlement failed: {error}"),
            Self::UnexpectedPersistence => {
                formatter.write_str("record unexpectedly entered synchronous persistence")
            }
        }
    }
}

impl<E> std::error::Error for ThreadedChannelRecordingLoopError<E> where
    E: std::error::Error + 'static
{
}

#[derive(Debug)]
pub enum PersistenceRecoveryError {
    Coordinator(MeasurementCoordinatorError),
    Classify(RecoveryClassificationError),
    Executor(PersistenceExecutorError),
    Grouped(GroupedExecutorRecoveryError),
}

impl fmt::Display for PersistenceRecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Coordinator(error) => write!(formatter, "pending write lookup failed: {error}"),
            Self::Classify(error) => write!(formatter, "recovered cursor is invalid: {error}"),
            Self::Executor(error) => write!(formatter, "executor recovery failed: {error}"),
            Self::Grouped(error) => write!(formatter, "grouped executor recovery failed: {error}"),
        }
    }
}

impl std::error::Error for PersistenceRecoveryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt;

    use sensor_core::{BootEpoch, ContinuityBreak, Gap, HardwareId, SampleRate, SequenceCursor};
    use sensor_protocol::{ObservabilityProfile, SampleChunk};

    use crate::bounded_writer::{GroupPersistError, GroupPersistSuccess, PersistenceEnvelope};
    use crate::channel_agent::{ChannelAgent, ChannelPolicy};
    use crate::channel_port::ChannelPort;
    use crate::channel_runtime::{ChannelRuntime, NodeScheduleConfig};
    use crate::channel_scheduler::{ChannelScheduler, ChannelSchedulerPolicy};
    use crate::measurement_coordinator::MeasurementCoordinator;
    use crate::node_session::NodeSession;
    use crate::simulation::{SimulatedClock, SimulatedRs485Bus, SimulatedWireNode};
    use crate::transaction::{DurationMicros, MonotonicMicros};

    const NODE_COUNT: usize = 4;

    #[derive(Debug, Eq, PartialEq)]
    struct SinkError;

    impl fmt::Display for SinkError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("injected durable storage failure")
        }
    }

    impl std::error::Error for SinkError {}

    #[derive(Default)]
    struct CountingSink {
        samples: [u64; NODE_COUNT],
        gaps: usize,
        continuity_breaks: usize,
        failures_remaining: usize,
    }

    impl DurableMeasurementSink for CountingSink {
        type Error = SinkError;

        fn persist_chunk(
            &mut self,
            hardware_id: HardwareId,
            chunk: &SampleChunk,
        ) -> Result<(), Self::Error> {
            if self.failures_remaining > 0 {
                self.failures_remaining -= 1;
                return Err(SinkError);
            }
            let index = usize::from(hardware_id.bytes()[0] - 1);
            self.samples[index] =
                self.samples[index].saturating_add(u64::from(chunk.header.sample_count));
            Ok(())
        }

        fn persist_gap(&mut self, _gap: Gap) -> Result<(), Self::Error> {
            self.gaps += 1;
            Ok(())
        }

        fn persist_continuity_break(
            &mut self,
            _observed: ContinuityBreak,
        ) -> Result<(), Self::Error> {
            self.continuity_breaks += 1;
            Ok(())
        }
    }

    impl GroupDurableMeasurementSink for CountingSink {
        fn persist_group(
            &mut self,
            envelopes: &[&PersistenceEnvelope],
        ) -> Result<GroupPersistSuccess, GroupPersistError<Self::Error>> {
            for envelope in envelopes {
                envelope
                    .persist(self)
                    .map_err(GroupPersistError::Preparation)?;
            }
            Ok(GroupPersistSuccess {
                persisted_records: envelopes.len(),
            })
        }
    }

    fn duration(value: u64) -> DurationMicros {
        DurationMicros::new(value).unwrap()
    }

    fn build_loop(
        offline_node: Option<u8>,
        dropped_node: Option<u8>,
        delayed_node: Option<u8>,
        storage_failures: usize,
    ) -> (
        SimulatedClock,
        ChannelRecordingLoop<SimulatedRs485Bus, SimulatedClock, CountingSink>,
    ) {
        let clock = SimulatedClock::new(0);
        let nodes = (1..=NODE_COUNT as u8)
            .map(|address| {
                SimulatedWireNode::new(address, BootEpoch::new(1), 2_048, SampleRate::HZ_125)
            })
            .collect();
        let mut bus = SimulatedRs485Bus::new(clock.clone(), nodes);
        if let Some(address) = offline_node {
            bus.set_offline(address, true);
        }
        if let Some(address) = dropped_node {
            bus.drop_next_responses(address, 1);
        }
        if let Some(address) = delayed_node {
            bus.delay_next_response(address, 70_000);
        }

        let agent = ChannelAgent::new(
            1,
            1,
            duration(60_000),
            ChannelPolicy {
                transaction_timeout: duration(55_000),
                retry_delay: duration(2_000),
                measurement_attempts_per_slot: 2,
                max_pending_work_items: 1,
            },
        )
        .unwrap();
        let endpoint = ChannelEndpoint::new(agent, ChannelPort::from_io(bus), clock.clone());
        let runtime = ChannelRuntime::new(
            ChannelScheduler::new(ChannelSchedulerPolicy {
                overflow_guard: duration(250_000),
                max_slot: duration(60_000),
            }),
            ObservabilityProfile::Production,
            NODE_COUNT,
            1,
        )
        .unwrap();
        let mut coordinator = MeasurementCoordinator::new(runtime, 1);
        for address in 1..=NODE_COUNT as u8 {
            coordinator
                .register_node(
                    NodeScheduleConfig {
                        node_address: address,
                        output_rate: SampleRate::HZ_125,
                        journal_capacity_samples: 2_048,
                        max_sample_chunk_samples: 48,
                        target_chunk_samples: 32,
                        measurement_slot: duration(60_000),
                        maximum_service_interval: duration(500_000),
                        retry_base_delay: duration(10_000),
                        retry_max_delay: duration(80_000),
                        retry_attempt_limit: 3,
                        offline_probe_interval: duration(2_000_000),
                    },
                    NodeSession::recovered(
                        HardwareId::from_bytes([address, 0, 0, 0, 0, 0, 0, 0]),
                        BootEpoch::new(1),
                        SequenceCursor::ORIGIN,
                        SequenceCursor::ORIGIN,
                    )
                    .unwrap(),
                    MonotonicMicros::new(0),
                )
                .unwrap();
        }
        let sink = CountingSink {
            failures_remaining: storage_failures,
            ..CountingSink::default()
        };
        (
            clock,
            ChannelRecordingLoop::new(endpoint, coordinator, sink),
        )
    }

    #[derive(Default)]
    struct RunStats {
        persistence_failures: usize,
        late_responses: usize,
    }

    fn run_for(
        clock: &SimulatedClock,
        recording_loop: &mut ChannelRecordingLoop<SimulatedRs485Bus, SimulatedClock, CountingSink>,
        duration_us: u64,
    ) -> RunStats {
        let mut stats = RunStats::default();
        let end = clock.now().get().saturating_add(duration_us);
        while clock.now().get() < end {
            match recording_loop.step() {
                Err(ChannelRecordingLoopError::Settlement(
                    MeasurementSettlementError::Persistence(_),
                )) => stats.persistence_failures += 1,
                Ok(ChannelRecordingEvent::Endpoint(ChannelEndpointEvent::Agent(
                    ChannelEvent::LateResponse,
                ))) => stats.late_responses += 1,
                _ => {}
            }
            clock.advance(1_000);
        }
        stats
    }

    #[test]
    fn multi_node_recording_survives_one_lost_response_and_storage_retry() {
        let (clock, mut recording_loop) = build_loop(None, Some(2), Some(3), 1);
        let run = run_for(&clock, &mut recording_loop, 10_000_000);
        let (endpoint, coordinator, sink, _time_observations, _telemetry_observations) =
            recording_loop.into_parts();
        let (_, port, _) = endpoint.into_parts();
        let bus = port.into_inner();

        assert_eq!(run.persistence_failures, 1);
        assert_eq!(run.late_responses, 1);
        assert_eq!(sink.gaps, 0);
        assert_eq!(sink.continuity_breaks, 0);
        let minimum = *sink.samples.iter().min().unwrap();
        let maximum = *sink.samples.iter().max().unwrap();
        assert!(minimum >= 1_150, "persisted samples: {:?}", sink.samples);
        assert!(
            maximum - minimum <= 48,
            "persisted samples: {:?}",
            sink.samples
        );
        assert_eq!(bus.node(2).unwrap().stats().dropped_responses, 1);
        assert_eq!(bus.node(3).unwrap().stats().delayed_responses, 1);
        for address in 1..=NODE_COUNT as u8 {
            assert_eq!(
                bus.node(address).unwrap().state().overwritten_samples,
                0,
                "node {address} overflowed"
            );
            assert!(
                !coordinator
                    .runtime()
                    .snapshot(address, clock.now())
                    .unwrap()
                    .offline
            );
        }
    }

    #[test]
    fn offline_node_enters_probe_mode_without_starving_healthy_nodes() {
        let (clock, mut recording_loop) = build_loop(Some(1), None, None, 0);
        run_for(&clock, &mut recording_loop, 5_000_000);
        let (endpoint, coordinator, sink, _time_observations, _telemetry_observations) =
            recording_loop.into_parts();
        let (_, port, _) = endpoint.into_parts();
        let bus = port.into_inner();

        assert_eq!(sink.samples[0], 0);
        for samples in &sink.samples[1..] {
            assert!(*samples >= 560, "healthy samples: {:?}", sink.samples);
        }
        assert!(
            coordinator
                .runtime()
                .snapshot(1, clock.now())
                .unwrap()
                .offline
        );
        assert!(bus.node(1).unwrap().stats().requests < 15);
        for address in 2..=NODE_COUNT as u8 {
            assert_eq!(bus.node(address).unwrap().state().overwritten_samples, 0);
        }
    }

    #[test]
    fn threaded_loop_keeps_collecting_while_storage_owns_previous_chunks() {
        let (clock, recording_loop) = build_loop(None, None, None, 0);
        let (endpoint, measurements, sink, _time_observations, _telemetry_observations) =
            recording_loop.into_parts();
        let persistence = GroupedPersistenceExecutor::start(
            sink,
            8,
            crate::persistence_executor::GroupCommitPolicy::new(
                4,
                std::time::Duration::from_micros(100),
            )
            .unwrap(),
            "test-channel-storage",
        )
        .unwrap();
        let mut recording_loop =
            ThreadedChannelRecordingLoop::new(endpoint, measurements, persistence);
        let end = clock.now().get() + 5_000_000;
        while clock.now().get() < end {
            recording_loop.step().unwrap();
            clock.advance(1_000);
            std::thread::yield_now();
        }

        let (endpoint, coordinator, persistence, _time_observations, _telemetry_observations) =
            recording_loop.into_parts();
        let shutdown = persistence.shutdown().unwrap();
        let sink = shutdown.sink;
        let (_, port, _) = endpoint.into_parts();
        let bus = port.into_inner();
        for (index, samples) in sink.samples.iter().enumerate() {
            assert!(*samples >= 500, "persisted samples: {:?}", sink.samples);
            assert_eq!(
                bus.node(u8::try_from(index + 1).unwrap())
                    .unwrap()
                    .state()
                    .overwritten_samples,
                0
            );
        }
        assert!(coordinator.in_flight_write_count() <= 8);
    }
}
