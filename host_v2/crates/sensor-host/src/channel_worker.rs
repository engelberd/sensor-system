use core::fmt;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use crate::capture_profile::{CaptureInstallationProfile, CaptureProfileError};
use crate::capture_repository::CaptureRepository;
use crate::capture_repository::CaptureRepositoryError;
use crate::capture_runtime::CaptureRuntimeError;
use crate::channel_activation::{
    activate_recording_nodes, stop_recording_nodes, RecordingActivationError,
    RecordingActivationPolicy, RecordingShutdownError, RecordingShutdownPolicy,
};
use crate::channel_bootstrap::{BootstrappedChannel, NodeBootstrapEvidence};
use crate::channel_endpoint::MonotonicClock;
use crate::channel_metrics::{ChannelMetricsPublisher, ChannelMetricsReader};
use crate::channel_recording_loop::{
    ThreadedChannelRecordingLoop, ThreadedChannelRecordingLoopError,
};
use crate::channel_recovery::{
    prepare_capture_stream, reconcile_channel_streams, ChannelRecoveryError,
};
use crate::channel_startup::{
    assemble_recording_coordinator, ChannelAssemblyError, ChannelStartupPlan,
    RecordingAssemblyPolicy,
};
use crate::channel_supervisor::SupervisedChannelTask;
use crate::discovery::DiscoveryTargetId;
use crate::handshake::HandshakeRequirements;
use crate::measurement_coordinator::MeasurementCoordinatorError;
use crate::node_metrics::NodeMetricsReader;
use crate::operations::{
    ChannelCommandDecision, ChannelOperationsController, OperationKind, OperationsError,
};
use crate::persistence_executor::{
    GroupedPersistenceExecutor, PersistenceExecutorError, PersistenceShutdown,
};
use crate::time_persistence::TimeObservationSubmitter;

pub type ProductionChannelLoop<P, C, T> =
    ThreadedChannelRecordingLoop<P, C, GroupedPersistenceExecutor<CaptureRepository>, T>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlledChannelShutdownPolicy {
    pub barrier_deadline: Duration,
    pub node: RecordingShutdownPolicy,
    pub drain_deadline: Duration,
}

impl ControlledChannelShutdownPolicy {
    pub fn validate(self) -> Result<Self, ControlledChannelShutdownError> {
        if self.barrier_deadline.is_zero() {
            return Err(ControlledChannelShutdownError::ZeroBarrierDeadline);
        }
        self.node
            .validate()
            .map_err(ControlledChannelShutdownError::Stop)?;
        if self.drain_deadline.is_zero() {
            return Err(ControlledChannelShutdownError::ZeroDrainDeadline);
        }
        Ok(self)
    }
}

pub struct ProductionChannelTask<P, C, T> {
    recording: ProductionChannelLoop<P, C, T>,
    nodes: Vec<ChannelControlNode>,
    shutdown_policy: ControlledChannelShutdownPolicy,
    metrics: ChannelMetricsPublisher,
    metrics_reader: ChannelMetricsReader,
    node_metrics_readers: Vec<NodeMetricsReader>,
    operations: ChannelOperationsController,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelControlNode {
    pub id: DiscoveryTargetId,
    pub requirements: HandshakeRequirements,
    pub boot_epoch: sensor_core::BootEpoch,
}

impl<P, C, T> ProductionChannelTask<P, C, T>
where
    P: Read + Write,
    C: MonotonicClock,
    T: TimeObservationSubmitter,
{
    pub fn new(
        recording: ProductionChannelLoop<P, C, T>,
        startup: &ChannelStartupPlan,
        shutdown_policy: ControlledChannelShutdownPolicy,
        operations: ChannelOperationsController,
    ) -> Result<Self, ControlledChannelShutdownError> {
        let shutdown_policy = shutdown_policy.validate()?;
        let nodes = startup
            .nodes()
            .iter()
            .map(|node| ChannelControlNode {
                id: DiscoveryTargetId {
                    channel_id: node.discovered.channel_id,
                    node_address: node.discovered.node_address,
                },
                requirements: node.requirements,
                boot_epoch: node.discovered.boot_epoch,
            })
            .collect();
        let storage_metrics = recording.persistence().metrics_reader();
        let node_metrics_readers = recording.measurements().node_metrics_readers();
        let (metrics, metrics_reader) =
            ChannelMetricsPublisher::new(startup.channel_id(), Some(storage_metrics));
        Ok(Self {
            recording,
            nodes,
            shutdown_policy,
            metrics,
            metrics_reader,
            node_metrics_readers,
            operations,
        })
    }

    pub fn metrics_reader(&self) -> ChannelMetricsReader {
        self.metrics_reader.clone()
    }

    pub fn node_metrics_readers(&self) -> Vec<NodeMetricsReader> {
        self.node_metrics_readers.clone()
    }

    pub fn control_nodes(&self) -> &[ChannelControlNode] {
        &self.nodes
    }
}

impl<P, C, T> SupervisedChannelTask for ProductionChannelTask<P, C, T>
where
    P: Read + Write + Send + 'static,
    C: MonotonicClock + Send + 'static,
    T: TimeObservationSubmitter + Send + 'static,
{
    type StepError = ProductionChannelStepError;
    type Shutdown = ControlledChannelShutdownReport;

    fn step(&mut self) -> Result<(), Self::StepError> {
        match self
            .operations
            .poll_command()
            .map_err(ProductionChannelStepError::Operations)?
        {
            ChannelCommandDecision::Started(request) => {
                debug_assert_ne!(request.kind, OperationKind::ReadStatus);
                self.operations
                    .fail_active(OperationsError::ExecutorUnavailable)
                    .map_err(ProductionChannelStepError::Operations)?;
            }
            ChannelCommandDecision::None
            | ChannelCommandDecision::Existing { .. }
            | ChannelCommandDecision::Rejected { .. } => {}
        }
        let started = Instant::now();
        let event = self
            .recording
            .step()
            .map_err(ProductionChannelStepError::Recording)?;
        self.metrics.observe_step(&event, started.elapsed());
        Ok(())
    }

    fn shutdown(self) -> Self::Shutdown {
        let node_ids = self.nodes.iter().map(|node| node.id).collect::<Vec<_>>();
        controlled_shutdown_channel(self.recording, &node_ids, self.shutdown_policy)
    }
}

#[derive(Debug)]
pub enum ProductionChannelStepError {
    Recording(ThreadedChannelRecordingLoopError<CaptureRepositoryError>),
    Operations(OperationsError),
}

impl fmt::Display for ProductionChannelStepError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "production channel step error: {self:?}")
    }
}

impl std::error::Error for ProductionChannelStepError {}

pub struct ControlledChannelShutdownReport {
    pub outcome: Result<(), ControlledChannelShutdownError>,
    pub persistence: Result<
        PersistenceShutdown<CaptureRepository, CaptureRepositoryError>,
        PersistenceExecutorError,
    >,
}

pub fn controlled_shutdown_channel<P, C, T>(
    recording: ProductionChannelLoop<P, C, T>,
    nodes: &[DiscoveryTargetId],
    policy: ControlledChannelShutdownPolicy,
) -> ControlledChannelShutdownReport
where
    P: Read + Write,
    C: MonotonicClock,
    T: TimeObservationSubmitter,
{
    let (recording, mut outcome) = match policy.validate() {
        Ok(policy) => run_controlled_shutdown(recording, nodes, policy),
        Err(error) => (recording, Err(error)),
    };
    let (_endpoint, _measurements, persistence, _time_observations) = recording.into_parts();
    let persistence = match persistence.shutdown() {
        Ok(mut shutdown) => {
            if let Err(error) = shutdown.sink.finish_all_streams() {
                remember_first_shutdown_error(
                    &mut outcome,
                    ControlledChannelShutdownError::CaptureFinalization(error),
                );
            }
            Ok(shutdown)
        }
        Err(error) => Err(error),
    };
    ControlledChannelShutdownReport {
        outcome,
        persistence,
    }
}

fn run_controlled_shutdown<P, C, T>(
    mut recording: ProductionChannelLoop<P, C, T>,
    nodes: &[DiscoveryTargetId],
    policy: ControlledChannelShutdownPolicy,
) -> (
    ProductionChannelLoop<P, C, T>,
    Result<(), ControlledChannelShutdownError>,
)
where
    P: Read + Write,
    C: MonotonicClock,
    T: TimeObservationSubmitter,
{
    recording.begin_quiesce();
    let mut outcome = Ok(());
    let barrier_deadline = Instant::now() + policy.barrier_deadline;
    while !recording.is_quiescent() {
        if Instant::now() >= barrier_deadline {
            remember_first_shutdown_error(
                &mut outcome,
                ControlledChannelShutdownError::BarrierDeadline,
            );
            break;
        }
        if let Err(error) = recording.step() {
            remember_first_shutdown_error(
                &mut outcome,
                ControlledChannelShutdownError::BarrierLoop(error),
            );
            break;
        }
    }

    // Stop needs exclusive access to the endpoint while retaining the same
    // coordinator, persistence executor and Time submitter.
    let (mut endpoint, measurements, persistence, time_observations) = recording.into_parts();
    let stop_result = stop_recording_nodes(&mut endpoint, nodes, policy.node);
    let mut recording = ThreadedChannelRecordingLoop::new_with_time_observations(
        endpoint,
        measurements,
        persistence,
        time_observations,
    );
    if let Err(error) = stop_result {
        remember_first_shutdown_error(&mut outcome, ControlledChannelShutdownError::Stop(error));
        return (recording, outcome);
    }

    if let Err(error) = recording.begin_shutdown_drain() {
        remember_first_shutdown_error(
            &mut outcome,
            ControlledChannelShutdownError::DrainCoordinator(error),
        );
        return (recording, outcome);
    }
    let drain_deadline = Instant::now() + policy.drain_deadline;
    while !recording.is_shutdown_drain_complete() {
        if Instant::now() >= drain_deadline {
            remember_first_shutdown_error(
                &mut outcome,
                ControlledChannelShutdownError::DrainDeadline,
            );
            return (recording, outcome);
        }
        if let Err(error) = recording.step() {
            remember_first_shutdown_error(
                &mut outcome,
                ControlledChannelShutdownError::DrainLoop(error),
            );
            return (recording, outcome);
        }
    }
    if let Err(error) = recording.finish_shutdown_drain() {
        remember_first_shutdown_error(
            &mut outcome,
            ControlledChannelShutdownError::DrainCoordinator(error),
        );
    }
    (recording, outcome)
}

fn remember_first_shutdown_error(
    outcome: &mut Result<(), ControlledChannelShutdownError>,
    error: ControlledChannelShutdownError,
) {
    if outcome.is_ok() {
        *outcome = Err(error);
    }
}

#[derive(Debug)]
pub enum ControlledChannelShutdownError {
    ZeroBarrierDeadline,
    ZeroDrainDeadline,
    BarrierDeadline,
    DrainDeadline,
    BarrierLoop(ThreadedChannelRecordingLoopError<CaptureRepositoryError>),
    DrainLoop(ThreadedChannelRecordingLoopError<CaptureRepositoryError>),
    DrainCoordinator(MeasurementCoordinatorError),
    Stop(RecordingShutdownError),
    CaptureFinalization(CaptureRepositoryError),
}

impl fmt::Display for ControlledChannelShutdownError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "controlled channel shutdown error: {self:?}")
    }
}

impl std::error::Error for ControlledChannelShutdownError {}

/// Performs the irreversible ordering constraints for one physical channel:
/// persistent port ownership, Capture replay, cursor reconciliation, lifecycle
/// activation, scheduler assembly, and only then recording-loop construction.
pub fn compose_channel_recording<P, C, T>(
    bootstrapped: BootstrappedChannel<P, C>,
    startup: &ChannelStartupPlan,
    capture: &CaptureInstallationProfile,
    activation_policy: RecordingActivationPolicy,
    assembly_policy: RecordingAssemblyPolicy,
    work_id_seed: u64,
    time_observations: T,
) -> Result<ProductionChannelLoop<P, C, T>, ChannelWorkerError>
where
    P: Read + Write,
    C: MonotonicClock,
    T: TimeObservationSubmitter,
{
    validate_bootstrap(startup, bootstrapped.nodes())?;
    let (mut endpoint, bootstrap_nodes) = bootstrapped.into_parts();

    let mut streams = Vec::with_capacity(bootstrap_nodes.len());
    for node in &bootstrap_nodes {
        let origin = capture
            .inspect_stream_origin(
                startup.channel_name(),
                node.identity.hardware_id,
                node.identity.boot_epoch,
            )
            .map_err(ChannelWorkerError::CaptureProfile)?;
        streams.push(
            prepare_capture_stream(*node, origin)
                .map_err(ChannelWorkerError::PrepareCapture)?
                .stream,
        );
    }

    let runtime = capture
        .runtime_config(startup.channel_name(), streams)
        .map_err(ChannelWorkerError::CaptureProfile)?
        .start()
        .map_err(ChannelWorkerError::CaptureRuntime)?;
    let (recovered_streams, executor) = runtime.into_parts();

    let mut recovered_nodes = match reconcile_channel_streams(&bootstrap_nodes, &recovered_streams)
    {
        Ok(nodes) => nodes,
        Err(source) => {
            let shutdown = executor.shutdown().err();
            return Err(ChannelWorkerError::Reconcile { source, shutdown });
        }
    };
    let activation_targets = startup
        .nodes()
        .iter()
        .map(|node| DiscoveryTargetId {
            channel_id: node.discovered.channel_id,
            node_address: node.discovered.node_address,
        })
        .collect::<Vec<_>>();

    if let Err(source) =
        activate_recording_nodes(&mut endpoint, &mut recovered_nodes, activation_policy)
    {
        let rollback = stop_recording_nodes(
            &mut endpoint,
            &activation_targets,
            RecordingShutdownPolicy {
                node_deadline: activation_policy.node_deadline,
            },
        )
        .err();
        let shutdown = executor.shutdown().err();
        return Err(ChannelWorkerError::Activation {
            source,
            rollback,
            shutdown,
        });
    }

    let now = endpoint.now();
    let coordinator = match assemble_recording_coordinator(
        startup,
        recovered_nodes,
        assembly_policy,
        now,
        work_id_seed,
    ) {
        Ok(value) => value,
        Err(source) => {
            let rollback = stop_recording_nodes(
                &mut endpoint,
                &activation_targets,
                RecordingShutdownPolicy {
                    node_deadline: activation_policy.node_deadline,
                },
            )
            .err();
            let shutdown = executor.shutdown().err();
            return Err(ChannelWorkerError::Assembly {
                source,
                rollback,
                shutdown,
            });
        }
    };

    Ok(ThreadedChannelRecordingLoop::new_with_time_observations(
        endpoint,
        coordinator,
        executor,
        time_observations,
    ))
}

fn validate_bootstrap(
    startup: &ChannelStartupPlan,
    nodes: &[NodeBootstrapEvidence],
) -> Result<(), ChannelWorkerError> {
    if nodes.len() != startup.nodes().len() {
        return Err(ChannelWorkerError::BootstrapNodeCountMismatch {
            planned: startup.nodes().len(),
            actual: nodes.len(),
        });
    }
    for planned in startup.nodes() {
        let node = nodes
            .iter()
            .find(|node| {
                node.id.channel_id == planned.discovered.channel_id
                    && node.id.node_address == planned.discovered.node_address
            })
            .ok_or(ChannelWorkerError::MissingBootstrapNode {
                channel_id: planned.discovered.channel_id,
                node_address: planned.discovered.node_address,
            })?;
        if node.identity.hardware_id != planned.discovered.hardware_id
            || node.identity.boot_epoch != planned.discovered.boot_epoch
        {
            return Err(ChannelWorkerError::BootstrapIdentityMismatch {
                channel_id: planned.discovered.channel_id,
                node_address: planned.discovered.node_address,
            });
        }
    }
    Ok(())
}

#[derive(Debug)]
pub enum ChannelWorkerError {
    BootstrapNodeCountMismatch {
        planned: usize,
        actual: usize,
    },
    MissingBootstrapNode {
        channel_id: u8,
        node_address: u8,
    },
    BootstrapIdentityMismatch {
        channel_id: u8,
        node_address: u8,
    },
    CaptureProfile(CaptureProfileError),
    PrepareCapture(ChannelRecoveryError),
    CaptureRuntime(CaptureRuntimeError),
    Reconcile {
        source: ChannelRecoveryError,
        shutdown: Option<PersistenceExecutorError>,
    },
    Activation {
        source: RecordingActivationError,
        rollback: Option<RecordingShutdownError>,
        shutdown: Option<PersistenceExecutorError>,
    },
    Assembly {
        source: ChannelAssemblyError,
        rollback: Option<RecordingShutdownError>,
        shutdown: Option<PersistenceExecutorError>,
    },
}

impl fmt::Display for ChannelWorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "channel worker startup error: {self:?}")
    }
}

impl std::error::Error for ChannelWorkerError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use sensor_core::{BootEpoch, HardwareId, SampleAxes, SampleAxis, SequenceCursor};
    use sensor_protocol::{
        decode_lifecycle_request, encode_lifecycle_response, encode_pull_response, FirmwareVersion,
        Frame, FrameRole, NodeCapabilities, NodeIdentity, ObservabilityProfile, OutputRateCode,
        PullResponse, RuntimeOperation, RuntimeResult, RuntimeResultCode, RuntimeState,
        SampleChunk, SampleChunkHeader, SensorStatusCode, ServiceId, StreamDecoder, StreamState,
        TimingEvidence, CAPABILITIES_SCHEMA_VERSION, HOST_ADDRESS, LIFECYCLE_SERVICE_ID,
        LOCAL_PROTOCOL_VERSION, MAX_PAYLOAD_BYTES, MAX_WIRE_FRAME_BYTES, TIMING_VALID,
    };
    use serde_json::json;

    use crate::capture_segments::{capture_segment_paths, validate_capture_segment};
    use crate::capture_window::{CaptureWindowPolicy, WindowSealReason};
    use crate::channel_admission::AdmissionPolicy;
    use crate::channel_agent::{ChannelAgent, ChannelPolicy};
    use crate::channel_bootstrap::NodeBootstrapEvidence;
    use crate::channel_endpoint::{ChannelEndpoint, SystemMonotonicClock};
    use crate::channel_port::ChannelPort;
    use crate::channel_runtime::TimeSyncScheduleConfig;
    use crate::channel_scheduler::ChannelSchedulerPolicy;
    use crate::channel_startup::{build_channel_startup_plans, ChannelStartupPolicy};
    use crate::discovery::{
        DiscoveryTargetId, InstallationDiscoveryPlan, REQUIRED_RECORDING_FEATURES,
    };
    use crate::installation_topology::InstallationTopologyProfile;
    use crate::persistence_executor::GroupCommitPolicy;
    use crate::time_persistence::RejectingTimeObservationSubmitter;
    use crate::transaction::DurationMicros;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    const HARDWARE: HardwareId = HardwareId::from_bytes([1, 1, 2, 3, 4, 5, 6, 7]);
    const EPOCH: BootEpoch = BootEpoch::new(101);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sensor-host-channel-worker-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct StartBus {
        reads: VecDeque<Vec<u8>>,
        decoder: StreamDecoder,
        starts: usize,
        stops: Arc<AtomicUsize>,
        pulls: usize,
    }

    impl Read for StartBus {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let Some(bytes) = self.reads.pop_front() else {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no response"));
            };
            let count = bytes.len().min(output.len());
            output[..count].copy_from_slice(&bytes[..count]);
            Ok(count)
        }
    }

    impl Write for StartBus {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            for byte in bytes {
                let Some(decoded) = self.decoder.push(*byte) else {
                    continue;
                };
                let request = decoded.map_err(io::Error::other)?;
                let payload = if request.service == LIFECYCLE_SERVICE_ID {
                    let command =
                        decode_lifecycle_request(request.message, request.payload()).unwrap();
                    let state = match command.operation {
                        RuntimeOperation::Start => {
                            self.starts += 1;
                            RuntimeState::Recording
                        }
                        RuntimeOperation::Stop => {
                            self.stops.fetch_add(1, Ordering::SeqCst);
                            RuntimeState::Idle
                        }
                        _ => return Err(io::Error::other("unexpected lifecycle operation")),
                    };
                    encode_lifecycle_response(RuntimeResult {
                        operation_id: command.operation_id,
                        code: RuntimeResultCode::Ok,
                        state,
                        observability: ObservabilityProfile::Production,
                        sensor_status: SensorStatusCode::Ok,
                    })
                    .unwrap()
                    .to_vec()
                } else if request.service == ServiceId::Measurement as u8 {
                    self.pulls += 1;
                    let mut payload = [0_u8; MAX_PAYLOAD_BYTES];
                    let response = if self.pulls == 1 {
                        let axis = SampleAxis::new(17).unwrap();
                        PullResponse::SampleChunk(SampleChunk {
                            header: SampleChunkHeader {
                                boot_epoch: EPOCH,
                                retained_from: SequenceCursor::new(140),
                                produced: SequenceCursor::new(141),
                                first: SequenceCursor::new(140),
                                sample_count: 1,
                            },
                            config_revision: 1,
                            timing: TimingEvidence {
                                segment_id: 1,
                                first_device_time_us: 1_000,
                                last_device_time_us: 1_000,
                                max_fit_residual_us: 0,
                                quality_flags: TIMING_VALID,
                            },
                            samples: vec![SampleAxes::new(axis, axis, axis)],
                        })
                    } else {
                        PullResponse::NoData(StreamState {
                            boot_epoch: EPOCH,
                            retained_from: SequenceCursor::new(141),
                            produced: SequenceCursor::new(141),
                            released: SequenceCursor::new(141),
                            capacity_samples: 2048,
                            overwritten_samples: 80,
                        })
                    };
                    let length = encode_pull_response(&response, &mut payload).unwrap();
                    payload[..length].to_vec()
                } else {
                    return Err(io::Error::other("unexpected service"));
                };
                let response = Frame::new(
                    FrameRole::Response,
                    HOST_ADDRESS,
                    request.destination,
                    request.service,
                    request.message,
                    request.transaction_id,
                    &payload,
                )
                .unwrap();
                let mut wire = [0_u8; MAX_WIRE_FRAME_BYTES];
                let length = response.encode(&mut wire);
                self.reads.push_back(wire[..length].to_vec());
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn duration(value: u64) -> DurationMicros {
        DurationMicros::new(value).unwrap()
    }

    fn identity() -> NodeIdentity {
        NodeIdentity {
            hardware_id: HARDWARE,
            boot_epoch: EPOCH,
            node_address: 1,
            board_revision: 2,
            firmware: FirmwareVersion {
                major: 0,
                minor: 5,
                patch: 6,
            },
            protocol: LOCAL_PROTOCOL_VERSION,
        }
    }

    fn capabilities() -> NodeCapabilities {
        NodeCapabilities {
            schema_version: CAPABILITIES_SCHEMA_VERSION,
            service_flags: ServiceId::Identity.capability_bit()
                | ServiceId::Measurement.capability_bit()
                | ServiceId::Lifecycle.capability_bit()
                | ServiceId::Time.capability_bit(),
            feature_flags: REQUIRED_RECORDING_FEATURES,
            max_payload_bytes: MAX_PAYLOAD_BYTES as u16,
            journal_capacity_samples: 2048,
            output_rate_mask: OutputRateCode::Hz125.capability_bit(),
            max_sample_chunk_samples: 48,
        }
    }

    fn topology() -> InstallationTopologyProfile {
        InstallationTopologyProfile::from_document(&json!({
            "system":{"name":"test"},
            "channels":[{
                "name":"line-a", "channel_id":1, "enabled":true,
                "transport":{"kind":"serial-rs485", "device_id":"adapter", "endpoint":"/dev/test", "baud_rate":115200},
                "nodes":[{"id":1,"hardware_id":"0101020304050607","board_revision":2,"enabled":true,"output_rate_hz":125}]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn composes_replay_activation_and_scheduler_without_reentering_the_cursor() {
        let discovery = InstallationDiscoveryPlan::from_topology(&topology());
        let mut collector = discovery.collector();
        collector
            .accept(
                DiscoveryTargetId {
                    channel_id: 1,
                    node_address: 1,
                },
                identity(),
                capabilities(),
            )
            .unwrap();
        let inventory = collector.finish().unwrap();
        let startup = build_channel_startup_plans(
            &inventory,
            ChannelStartupPolicy {
                admission: AdmissionPolicy {
                    maximum_utilization_ppm: 900_000,
                    recovery_reserve_ppm: 100_000,
                },
                target_chunk_samples: 32,
                measurement_transaction_time: duration(36_900),
                measurement_slot: duration(40_000),
                maximum_service_interval: duration(500_000),
                retry_base_delay: duration(10_000),
                retry_max_delay: duration(1_000_000),
                retry_attempt_limit: 3,
                offline_probe_interval: duration(5_000_000),
                time_sync_schedule: TimeSyncScheduleConfig::new(
                    duration(30_000_000),
                    duration(4_000),
                    duration(250_000),
                )
                .unwrap(),
                time_sync_transaction_time: duration(4_000),
            },
        )
        .unwrap();

        let agent = ChannelAgent::new(
            10,
            20,
            duration(50),
            ChannelPolicy {
                transaction_timeout: duration(100_000),
                retry_delay: duration(1_000),
                measurement_attempts_per_slot: 2,
                max_pending_work_items: 2,
            },
        )
        .unwrap();
        let stops = Arc::new(AtomicUsize::new(0));
        let endpoint = ChannelEndpoint::new(
            agent,
            ChannelPort::from_io(StartBus {
                reads: VecDeque::new(),
                decoder: StreamDecoder::new(),
                starts: 0,
                stops: Arc::clone(&stops),
                pulls: 0,
            }),
            SystemMonotonicClock::new(),
        );
        let bootstrapped = BootstrappedChannel::from_parts(
            endpoint,
            vec![NodeBootstrapEvidence {
                id: DiscoveryTargetId {
                    channel_id: 1,
                    node_address: 1,
                },
                identity: identity(),
                capabilities: capabilities(),
                runtime: RuntimeResult {
                    operation_id: 1,
                    code: RuntimeResultCode::Ok,
                    state: RuntimeState::Idle,
                    observability: ObservabilityProfile::Production,
                    sensor_status: SensorStatusCode::Ok,
                },
                stream: StreamState {
                    boot_epoch: EPOCH,
                    retained_from: SequenceCursor::new(80),
                    produced: SequenceCursor::new(140),
                    released: SequenceCursor::new(90),
                    capacity_samples: 2048,
                    overwritten_samples: 80,
                },
            }],
        );
        let temp = TempDirectory::new();
        let capture = CaptureInstallationProfile::new(
            &temp.0,
            CaptureWindowPolicy::new(600_000_000).unwrap(),
            8,
            GroupCommitPolicy::new(4, Duration::from_millis(2)).unwrap(),
            "capture",
        )
        .unwrap();

        let recording = compose_channel_recording(
            bootstrapped,
            &startup[0],
            &capture,
            RecordingActivationPolicy {
                node_deadline: Duration::from_secs(1),
            },
            RecordingAssemblyPolicy {
                scheduler: ChannelSchedulerPolicy {
                    overflow_guard: duration(100_000),
                    max_slot: duration(50_000),
                },
                first_time_sync_delay: duration(1_000_000),
                time_sync_phase_step: duration(100_000),
            },
            100,
            RejectingTimeObservationSubmitter,
        )
        .unwrap();

        let now = crate::transaction::MonotonicMicros::new(1_000_000);
        assert_eq!(
            recording
                .measurements()
                .runtime()
                .snapshot(1, now)
                .unwrap()
                .received,
            SequenceCursor::new(140)
        );
        let origin = capture
            .inspect_stream_origin("line-a", HARDWARE, EPOCH)
            .unwrap()
            .unwrap();
        assert_eq!(origin.first_cursor, SequenceCursor::new(140));
        let report = controlled_shutdown_channel(
            recording,
            &[DiscoveryTargetId {
                channel_id: 1,
                node_address: 1,
            }],
            ControlledChannelShutdownPolicy {
                barrier_deadline: Duration::from_secs(1),
                node: RecordingShutdownPolicy {
                    node_deadline: Duration::from_secs(1),
                },
                drain_deadline: Duration::from_secs(1),
            },
        );
        assert!(report.outcome.is_ok());
        assert_eq!(stops.load(Ordering::SeqCst), 1);
        let shutdown = report.persistence.unwrap();
        assert_eq!(
            shutdown.sink.durable_cursors().next().unwrap().2,
            SequenceCursor::new(141)
        );
        let stream_directory = capture.stream_directory("line-a", HARDWARE, EPOCH).unwrap();
        let paths = capture_segment_paths(stream_directory, 0);
        let validation = validate_capture_segment(paths.wal, paths.manifest).unwrap();
        assert_eq!(validation.manifest.end_exclusive, SequenceCursor::new(141));
        assert_eq!(
            validation.manifest.seal_reason,
            WindowSealReason::ExplicitFinish
        );
    }
}
