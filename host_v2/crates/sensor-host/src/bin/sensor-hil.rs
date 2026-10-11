use core::fmt;
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sensor_core::{ClockObservation, ContinuityBreak, Gap, HardwareId, SequenceCursor};
use sensor_host::capture_segments::{
    capture_segment_paths, validate_capture_segment, CaptureSegmentStream,
};
use sensor_host::capture_wal::CaptureWalConfig;
use sensor_host::capture_window::CaptureWindowPolicy;
use sensor_host::channel_agent::{ChannelAgent, ChannelEvent, ChannelPolicy};
use sensor_host::channel_endpoint::{
    ChannelEndpoint, ChannelEndpointEvent, MonotonicClock, SystemMonotonicClock,
};
use sensor_host::channel_port::{ReceiveEvent, SerialChannelPort, SerialPortConfig};
use sensor_host::handshake::{capabilities_request, identity_request, HandshakeRequirements};
use sensor_host::measurement_persistence::{persist_pending, DurableMeasurementSink};
use sensor_host::measurement_transfer::{MeasurementTransfer, PersistenceKind, RecoveryReason};
use sensor_host::node_session::NodeSession;
use sensor_host::transaction::{DurationMicros, ResponseDisposition, TimerEvent, TransactionCore};
use sensor_protocol::{
    decode_stream_state_success, FirmwareVersion, Frame, FrameError, IdentityContractError,
    NodeCapabilities, NodeIdentity, ObservabilityProfile, OutputRateCode, RuntimeOperation,
    RuntimeResultCode, RuntimeState, SampleChunk, ServiceId, StreamState, FEATURE_SAMPLE_JOURNAL,
    FEATURE_TRIAL_BOOT_CONFIRMATION, GET_STREAM_STATE_MESSAGE_ID,
};

const DEFAULT_BAUD_RATE: u32 = 115_200;
const PROBE_LIMIT: Duration = Duration::from_secs(5);
const TRIAL_CONFIRM_LIMIT: Duration = Duration::from_secs(45);
const DEFAULT_RECORD_CYCLE_LIMIT: Duration = Duration::from_secs(30);
const HIL_RECORD_LIMIT_ENV: &str = "SENSOR_SYSTEM_HIL_RECORD_LIMIT_SECONDS";
const DEFAULT_SAMPLE_TARGET: u64 = 250;
const DEFAULT_TIME_OBSERVATIONS: u16 = 6;
const DEFAULT_TIME_INTERVAL_MS: u64 = 1_000;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sensor-hil: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), HilError> {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if arguments.len() == 1 && arguments[0] == "--help" {
        print_usage();
        return Ok(());
    }
    let command = parse_arguments(&arguments)?;
    match command {
        Command::Identify {
            device,
            node_address,
            baud_rate,
        } => run_identify(SerialPortConfig::rs485(device, baud_rate), node_address),
        Command::Handshake {
            device,
            node_address,
            hardware_id,
            board_revision,
            baud_rate,
        } => run_probe(
            SerialPortConfig::rs485(device, baud_rate),
            HandshakeRequirements::recording_node(
                hardware_id,
                node_address,
                board_revision,
                FEATURE_SAMPLE_JOURNAL,
            ),
            false,
            None,
        ),
        Command::ConfirmTrial {
            device,
            node_address,
            hardware_id,
            board_revision,
            expected_firmware,
            baud_rate,
        } => run_probe(
            SerialPortConfig::rs485(device, baud_rate),
            HandshakeRequirements::recording_node(
                hardware_id,
                node_address,
                board_revision,
                FEATURE_SAMPLE_JOURNAL | FEATURE_TRIAL_BOOT_CONFIRMATION,
            ),
            true,
            Some(expected_firmware),
        ),
        Command::RecordCycle {
            device,
            node_address,
            hardware_id,
            board_revision,
            output,
            sample_target,
            chunk_limit,
            baud_rate,
        } => run_record_cycle(
            SerialPortConfig::rs485(device, baud_rate),
            HandshakeRequirements::recording_node(
                hardware_id,
                node_address,
                board_revision,
                FEATURE_SAMPLE_JOURNAL,
            ),
            HilOutput::Diagnostic(output),
            sample_target,
            chunk_limit,
        ),
        Command::CaptureCycle {
            device,
            node_address,
            hardware_id,
            board_revision,
            output_directory,
            window_duration_us,
            sample_target,
            chunk_limit,
            baud_rate,
        } => run_record_cycle(
            SerialPortConfig::rs485(device, baud_rate),
            HandshakeRequirements::recording_node(
                hardware_id,
                node_address,
                board_revision,
                FEATURE_SAMPLE_JOURNAL,
            ),
            HilOutput::Capture {
                directory: output_directory,
                window_duration_us,
            },
            sample_target,
            chunk_limit,
        ),
        Command::SafeStop {
            device,
            node_address,
            hardware_id,
            board_revision,
            baud_rate,
        } => run_safe_stop(
            SerialPortConfig::rs485(device, baud_rate),
            HandshakeRequirements::recording_node(
                hardware_id,
                node_address,
                board_revision,
                FEATURE_SAMPLE_JOURNAL,
            ),
        ),
        Command::PrepareUpdate {
            device,
            node_address,
            hardware_id,
            board_revision,
            baud_rate,
        } => run_prepare_update(
            SerialPortConfig::rs485(device, baud_rate),
            HandshakeRequirements::recording_node(
                hardware_id,
                node_address,
                board_revision,
                FEATURE_SAMPLE_JOURNAL,
            )
            .for_schema_1_migration(FirmwareVersion {
                major: 0,
                minor: 5,
                patch: 4,
            }),
        ),
        Command::TimeProbe {
            device,
            node_address,
            hardware_id,
            board_revision,
            observation_count,
            interval_ms,
            baud_rate,
        } => {
            let mut requirements = HandshakeRequirements::recording_node(
                hardware_id,
                node_address,
                board_revision,
                FEATURE_SAMPLE_JOURNAL,
            );
            requirements.required_services |= ServiceId::Time.capability_bit();
            run_time_probe(
                SerialPortConfig::rs485(device, baud_rate),
                requirements,
                observation_count,
                Duration::from_millis(interval_ms),
            )
        }
    }
}

fn run_time_probe(
    config: SerialPortConfig,
    requirements: HandshakeRequirements,
    observation_count: u16,
    interval: Duration,
) -> Result<(), HilError> {
    let port = SerialChannelPort::open(&config).map_err(HilError::Port)?;
    let policy = ChannelPolicy {
        transaction_timeout: DurationMicros::new(250_000).map_err(HilError::Transaction)?,
        retry_delay: DurationMicros::new(25_000).map_err(HilError::Transaction)?,
        measurement_attempts_per_slot: 2,
        max_pending_work_items: 4,
    };
    let session_seed = process_session_seed();
    let mut agent = ChannelAgent::new(
        session_seed,
        session_seed.rotate_left(13),
        DurationMicros::new(50_000).map_err(HilError::Transaction)?,
        policy,
    )
    .map_err(HilError::Agent)?;
    agent
        .submit_handshake(requirements)
        .map_err(HilError::Agent)?;
    let mut endpoint = ChannelEndpoint::new(agent, port, SystemMonotonicClock::new());
    let handshake_deadline = Instant::now() + PROBE_LIMIT;
    let boot_epoch = loop {
        if Instant::now() >= handshake_deadline {
            return Err(HilError::TimeProbeHandshakeTimedOut);
        }
        match endpoint.step().map_err(HilError::Endpoint)? {
            ChannelEndpointEvent::Agent(ChannelEvent::HandshakeComplete { identity, .. }) => {
                break identity.boot_epoch;
            }
            ChannelEndpointEvent::Disconnected => return Err(HilError::Disconnected),
            _ => {}
        }
    };

    let mut observations = Vec::with_capacity(usize::from(observation_count));
    let mut decode_errors = 0_u32;
    let mut transaction_timeouts = 0_u32;
    for index in 0..observation_count {
        if index != 0 {
            std::thread::sleep(interval);
        }
        endpoint
            .agent_mut()
            .submit_time_sync(requirements.node_address, boot_epoch)
            .map_err(HilError::Agent)?;
        let observation_deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if Instant::now() >= observation_deadline {
                return Err(HilError::TimeProbeObservationTimedOut(index));
            }
            match endpoint.step().map_err(HilError::Endpoint)? {
                ChannelEndpointEvent::Agent(ChannelEvent::TimeObservation {
                    observation, ..
                }) => {
                    println!(
                        "time observation {}/{}: t1={} t2={} t3={} t4={} path_delay_us={} uncertainty_us={}",
                        index + 1,
                        observation_count,
                        observation.host_send_us,
                        observation.node_receive_us,
                        observation.node_send_us,
                        observation.host_receive_us,
                        observation.path_delay_us(),
                        observation.uncertainty_us(),
                    );
                    observations.push(observation);
                    break;
                }
                ChannelEndpointEvent::Agent(ChannelEvent::TimeSyncTimedOut { .. }) => {
                    transaction_timeouts = transaction_timeouts.saturating_add(1);
                }
                ChannelEndpointEvent::Agent(ChannelEvent::TimeSyncNotAccepted { .. }) => {
                    return Err(HilError::TimeProbeNotAccepted);
                }
                ChannelEndpointEvent::DecodeError(_) => {
                    decode_errors = decode_errors.saturating_add(1);
                }
                ChannelEndpointEvent::Disconnected => return Err(HilError::Disconnected),
                _ => {}
            }
        }
    }

    report_time_probe(&observations, decode_errors, transaction_timeouts)?;
    Ok(())
}

fn report_time_probe(
    observations: &[ClockObservation],
    decode_errors: u32,
    transaction_timeouts: u32,
) -> Result<(), HilError> {
    let first = observations.first().ok_or(HilError::TimeProbeInvariant)?;
    let last = observations.last().ok_or(HilError::TimeProbeInvariant)?;
    let min_delay = observations
        .iter()
        .map(|value| value.path_delay_us())
        .min()
        .ok_or(HilError::TimeProbeInvariant)?;
    let max_delay = observations
        .iter()
        .map(|value| value.path_delay_us())
        .max()
        .ok_or(HilError::TimeProbeInvariant)?;
    let max_uncertainty = observations
        .iter()
        .map(|value| value.uncertainty_us())
        .max()
        .ok_or(HilError::TimeProbeInvariant)?;
    let host_delta = last
        .host_midpoint_us()
        .saturating_sub(first.host_midpoint_us());
    let node_delta = last
        .node_midpoint_us()
        .saturating_sub(first.node_midpoint_us());
    let apparent_drift_ppm = if host_delta == 0 {
        0
    } else {
        ((i128::from(node_delta) - i128::from(host_delta)) * 1_000_000 / i128::from(host_delta))
            as i64
    };
    println!(
        "time probe complete: observations={} boot_epoch={} path_delay_us={}..{} max_uncertainty_us={} apparent_drift_ppm={} decode_errors={} transaction_timeouts={}",
        observations.len(),
        first.boot_epoch.get(),
        min_delay,
        max_delay,
        max_uncertainty,
        apparent_drift_ppm,
        decode_errors,
        transaction_timeouts,
    );
    if decode_errors != 0 || transaction_timeouts != 0 {
        return Err(HilError::TimeProbeLinkErrors {
            decode_errors,
            transaction_timeouts,
        });
    }
    Ok(())
}

fn run_safe_stop(
    config: SerialPortConfig,
    requirements: HandshakeRequirements,
) -> Result<(), HilError> {
    let port = SerialChannelPort::open(&config).map_err(HilError::Port)?;
    let policy = ChannelPolicy {
        transaction_timeout: DurationMicros::new(250_000).map_err(HilError::Transaction)?,
        retry_delay: DurationMicros::new(25_000).map_err(HilError::Transaction)?,
        measurement_attempts_per_slot: 2,
        max_pending_work_items: 4,
    };
    let session_seed = process_session_seed();
    let mut agent = ChannelAgent::new(
        session_seed,
        session_seed.rotate_left(13),
        DurationMicros::new(50_000).map_err(HilError::Transaction)?,
        policy,
    )
    .map_err(HilError::Agent)?;
    agent
        .submit_handshake(requirements)
        .map_err(HilError::Agent)?;
    let mut endpoint = ChannelEndpoint::new(agent, port, SystemMonotonicClock::new());
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut state_operation_id = None;
    let mut stop_operation_id = None;
    let mut maintenance_operation_id = None;

    while Instant::now() < deadline {
        match endpoint.step().map_err(HilError::Endpoint)? {
            ChannelEndpointEvent::Agent(ChannelEvent::HandshakeComplete { .. }) => {
                state_operation_id = Some(
                    endpoint
                        .agent_mut()
                        .submit_lifecycle(
                            requirements.node_address,
                            RuntimeOperation::GetState,
                            ObservabilityProfile::Production,
                        )
                        .map_err(HilError::Agent)?,
                );
            }
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete { result, .. })
                if Some(result.operation_id) == state_operation_id =>
            {
                if result.code != RuntimeResultCode::Ok {
                    return Err(HilError::UnexpectedLifecycleResult {
                        code: result.code,
                        state: result.state,
                        expected_state: RuntimeState::Idle,
                    });
                }
                match result.state {
                    RuntimeState::Recording | RuntimeState::Stopping => {
                        stop_operation_id = Some(
                            endpoint
                                .agent_mut()
                                .submit_lifecycle(
                                    requirements.node_address,
                                    RuntimeOperation::Stop,
                                    ObservabilityProfile::Production,
                                )
                                .map_err(HilError::Agent)?,
                        );
                    }
                    RuntimeState::Idle => {
                        maintenance_operation_id = Some(
                            endpoint
                                .agent_mut()
                                .submit_lifecycle(
                                    requirements.node_address,
                                    RuntimeOperation::EnterMaintenance,
                                    ObservabilityProfile::Production,
                                )
                                .map_err(HilError::Agent)?,
                        );
                    }
                    RuntimeState::Maintenance => {
                        println!("safe-stop complete: state=Maintenance");
                        return Ok(());
                    }
                    RuntimeState::Faulted => return Err(HilError::NodeFaulted),
                }
            }
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete { result, .. })
                if Some(result.operation_id) == stop_operation_id =>
            {
                require_lifecycle_result(result.code, result.state, RuntimeState::Idle)?;
                maintenance_operation_id = Some(
                    endpoint
                        .agent_mut()
                        .submit_lifecycle(
                            requirements.node_address,
                            RuntimeOperation::EnterMaintenance,
                            ObservabilityProfile::Production,
                        )
                        .map_err(HilError::Agent)?,
                );
            }
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete { result, .. })
                if Some(result.operation_id) == maintenance_operation_id =>
            {
                require_lifecycle_result(result.code, result.state, RuntimeState::Maintenance)?;
                println!("safe-stop complete: state=Maintenance");
                return Ok(());
            }
            ChannelEndpointEvent::Disconnected => return Err(HilError::Disconnected),
            _ => {}
        }
    }
    Err(HilError::SafeStopTimedOut)
}

fn run_prepare_update(
    config: SerialPortConfig,
    requirements: HandshakeRequirements,
) -> Result<(), HilError> {
    let port = SerialChannelPort::open(&config).map_err(HilError::Port)?;
    let policy = ChannelPolicy {
        transaction_timeout: DurationMicros::new(250_000).map_err(HilError::Transaction)?,
        retry_delay: DurationMicros::new(25_000).map_err(HilError::Transaction)?,
        measurement_attempts_per_slot: 2,
        max_pending_work_items: 4,
    };
    let session_seed = process_session_seed();
    let mut agent = ChannelAgent::new(
        session_seed,
        session_seed.rotate_left(13),
        DurationMicros::new(50_000).map_err(HilError::Transaction)?,
        policy,
    )
    .map_err(HilError::Agent)?;
    agent
        .submit_handshake(requirements)
        .map_err(HilError::Agent)?;
    let mut endpoint = ChannelEndpoint::new(agent, port, SystemMonotonicClock::new());
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut state_operation_id = None;
    let mut stop_operation_id = None;
    let mut maintenance_operation_id = None;
    let mut prepare_operation_id = None;

    while Instant::now() < deadline {
        match endpoint.step().map_err(HilError::Endpoint)? {
            ChannelEndpointEvent::Agent(ChannelEvent::HandshakeComplete { .. }) => {
                state_operation_id = Some(
                    endpoint
                        .agent_mut()
                        .submit_lifecycle(
                            requirements.node_address,
                            RuntimeOperation::GetState,
                            ObservabilityProfile::Production,
                        )
                        .map_err(HilError::Agent)?,
                );
            }
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete { result, .. })
                if Some(result.operation_id) == state_operation_id =>
            {
                if result.code != RuntimeResultCode::Ok {
                    return Err(HilError::UnexpectedLifecycleResult {
                        code: result.code,
                        state: result.state,
                        expected_state: RuntimeState::Idle,
                    });
                }
                match result.state {
                    RuntimeState::Recording | RuntimeState::Stopping => {
                        stop_operation_id = Some(
                            endpoint
                                .agent_mut()
                                .submit_lifecycle(
                                    requirements.node_address,
                                    RuntimeOperation::Stop,
                                    ObservabilityProfile::Production,
                                )
                                .map_err(HilError::Agent)?,
                        );
                    }
                    RuntimeState::Idle => {
                        maintenance_operation_id = Some(
                            endpoint
                                .agent_mut()
                                .submit_lifecycle(
                                    requirements.node_address,
                                    RuntimeOperation::EnterMaintenance,
                                    ObservabilityProfile::Production,
                                )
                                .map_err(HilError::Agent)?,
                        );
                    }
                    RuntimeState::Maintenance => {
                        prepare_operation_id = Some(
                            endpoint
                                .agent_mut()
                                .submit_lifecycle(
                                    requirements.node_address,
                                    RuntimeOperation::PrepareFirmwareUpdate,
                                    ObservabilityProfile::Production,
                                )
                                .map_err(HilError::Agent)?,
                        );
                    }
                    RuntimeState::Faulted => return Err(HilError::NodeFaulted),
                }
            }
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete { result, .. })
                if Some(result.operation_id) == stop_operation_id =>
            {
                require_lifecycle_result(result.code, result.state, RuntimeState::Idle)?;
                maintenance_operation_id = Some(
                    endpoint
                        .agent_mut()
                        .submit_lifecycle(
                            requirements.node_address,
                            RuntimeOperation::EnterMaintenance,
                            ObservabilityProfile::Production,
                        )
                        .map_err(HilError::Agent)?,
                );
            }
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete { result, .. })
                if Some(result.operation_id) == maintenance_operation_id =>
            {
                require_lifecycle_result(result.code, result.state, RuntimeState::Maintenance)?;
                prepare_operation_id = Some(
                    endpoint
                        .agent_mut()
                        .submit_lifecycle(
                            requirements.node_address,
                            RuntimeOperation::PrepareFirmwareUpdate,
                            ObservabilityProfile::Production,
                        )
                        .map_err(HilError::Agent)?,
                );
            }
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete { result, .. })
                if Some(result.operation_id) == prepare_operation_id =>
            {
                require_lifecycle_result(result.code, result.state, RuntimeState::Maintenance)?;
                println!(
                    "firmware update prepared: operation_id={} state=Maintenance",
                    result.operation_id
                );
                return Ok(());
            }
            ChannelEndpointEvent::Disconnected => return Err(HilError::Disconnected),
            _ => {}
        }
    }
    Err(HilError::PrepareUpdateTimedOut)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordCyclePhase {
    Handshake,
    ReadingState,
    ExitingMaintenance,
    Starting,
    Recording,
    Stopping,
    Draining,
    EnteringMaintenance,
}

#[derive(Debug, Default)]
struct DecodeStats {
    payload_too_large: u32,
    encoded_too_large: u32,
    malformed_cobs: u32,
    frame_too_short: u32,
    unsupported_version: u32,
    invalid_role: u32,
    invalid_route: u32,
    crc_mismatch: u32,
}

impl DecodeStats {
    fn record(&mut self, error: FrameError) {
        let counter = match error {
            FrameError::PayloadTooLarge => &mut self.payload_too_large,
            FrameError::EncodedFrameTooLarge => &mut self.encoded_too_large,
            FrameError::MalformedCobs => &mut self.malformed_cobs,
            FrameError::FrameTooShort => &mut self.frame_too_short,
            FrameError::UnsupportedVersion(_) => &mut self.unsupported_version,
            FrameError::InvalidRole(_) => &mut self.invalid_role,
            FrameError::InvalidRoute => &mut self.invalid_route,
            FrameError::CrcMismatch => &mut self.crc_mismatch,
        };
        *counter = counter.saturating_add(1);
    }

    fn total(&self) -> u32 {
        self.payload_too_large
            .saturating_add(self.encoded_too_large)
            .saturating_add(self.malformed_cobs)
            .saturating_add(self.frame_too_short)
            .saturating_add(self.unsupported_version)
            .saturating_add(self.invalid_role)
            .saturating_add(self.invalid_route)
            .saturating_add(self.crc_mismatch)
    }
}

#[derive(Default)]
struct HilSinkStats {
    samples: u64,
    gaps: u64,
    continuity_breaks: u64,
}

/// Crash-durable diagnostic sink used only by the physical HIL command. Its
/// line format is intentionally not the product Capture format.
enum HilSinkBackend {
    Diagnostic(File),
    Capture {
        directory: PathBuf,
        stream: Box<CaptureSegmentStream>,
    },
}

struct HilDurableSink {
    backend: HilSinkBackend,
    stats: HilSinkStats,
}

impl HilDurableSink {
    fn create_diagnostic(path: &PathBuf) -> Result<Self, HilError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(HilError::Io)?;
        file.write_all(b"sensor-hil-measurement-v1\n")
            .map_err(HilError::Io)?;
        file.sync_data().map_err(HilError::Io)?;
        Ok(Self {
            backend: HilSinkBackend::Diagnostic(file),
            stats: HilSinkStats::default(),
        })
    }

    fn create_capture(
        directory: PathBuf,
        hardware_id: HardwareId,
        boot_epoch: sensor_core::BootEpoch,
        recovered_cursor: SequenceCursor,
        window_duration_us: u64,
    ) -> Result<Self, HilError> {
        if directory.exists() {
            return Err(HilError::CaptureOutputExists(directory));
        }
        let stream = CaptureSegmentStream::open(
            &directory,
            CaptureWalConfig {
                hardware_id,
                boot_epoch,
                recovered_cursor,
                first_window_ordinal: 0,
                window_policy: CaptureWindowPolicy::new(window_duration_us)
                    .map_err(|error| HilError::Persistence(error.to_string()))?,
            },
        )
        .map_err(|error| HilError::Persistence(error.to_string()))?;
        Ok(Self {
            backend: HilSinkBackend::Capture {
                directory,
                stream: Box::new(stream),
            },
            stats: HilSinkStats::default(),
        })
    }

    fn finalize(&mut self) -> Result<(), HilError> {
        match &mut self.backend {
            HilSinkBackend::Diagnostic(file) => file.sync_all().map_err(HilError::Io),
            HilSinkBackend::Capture { directory, stream } => {
                let last_ordinal = stream.current_window_ordinal();
                stream
                    .finish_window()
                    .map_err(|error| HilError::Persistence(error.to_string()))?;
                for ordinal in 0..=last_ordinal {
                    let paths = capture_segment_paths(&*directory, ordinal);
                    validate_capture_segment(paths.wal, paths.manifest)
                        .map_err(|error| HilError::Persistence(error.to_string()))?;
                }
                println!(
                    "capture validation complete: sealed_windows={}",
                    last_ordinal.saturating_add(1)
                );
                Ok(())
            }
        }
    }
}

impl DurableMeasurementSink for HilDurableSink {
    type Error = io::Error;

    fn persist_chunk(
        &mut self,
        hardware_id: HardwareId,
        chunk: &SampleChunk,
    ) -> Result<(), Self::Error> {
        if let HilSinkBackend::Capture { stream, .. } = &mut self.backend {
            stream
                .persist_chunk(hardware_id, chunk)
                .map_err(io::Error::other)?;
            self.stats.samples = self
                .stats
                .samples
                .saturating_add(u64::from(chunk.header.sample_count));
            return Ok(());
        }
        let HilSinkBackend::Diagnostic(file) = &mut self.backend else {
            unreachable!();
        };
        writeln!(
            file,
            "chunk hardware={} boot={} first={} count={} config={} segment={} first_us={} last_us={} residual_us={} flags={}",
            format_hardware_id(hardware_id),
            chunk.header.boot_epoch.get(),
            chunk.header.first.get(),
            chunk.header.sample_count,
            chunk.config_revision,
            chunk.timing.segment_id,
            chunk.timing.first_device_time_us,
            chunk.timing.last_device_time_us,
            chunk.timing.max_fit_residual_us,
            chunk.timing.quality_flags,
        )?;
        for sample in &chunk.samples {
            writeln!(
                file,
                "sample {} {} {}",
                sample.x.get(),
                sample.y.get(),
                sample.z.get()
            )?;
        }
        file.sync_data()?;
        self.stats.samples = self
            .stats
            .samples
            .saturating_add(u64::from(chunk.header.sample_count));
        Ok(())
    }

    fn persist_gap(&mut self, gap: Gap) -> Result<(), Self::Error> {
        if let HilSinkBackend::Capture { stream, .. } = &mut self.backend {
            stream.persist_gap(gap).map_err(io::Error::other)?;
            self.stats.gaps = self.stats.gaps.saturating_add(1);
            return Ok(());
        }
        let HilSinkBackend::Diagnostic(file) = &mut self.backend else {
            unreachable!();
        };
        writeln!(
            file,
            "gap hardware={} boot={} first={} count={} reason={:?}",
            format_hardware_id(gap.hardware_id),
            gap.boot_epoch.get(),
            gap.first_missing.get(),
            gap.missing_count,
            gap.reason,
        )?;
        file.sync_data()?;
        self.stats.gaps = self.stats.gaps.saturating_add(1);
        Ok(())
    }

    fn persist_continuity_break(&mut self, observed: ContinuityBreak) -> Result<(), Self::Error> {
        if let HilSinkBackend::Capture { stream, .. } = &mut self.backend {
            stream
                .persist_continuity_break(observed)
                .map_err(io::Error::other)?;
            self.stats.continuity_breaks = self.stats.continuity_breaks.saturating_add(1);
            return Ok(());
        }
        let HilSinkBackend::Diagnostic(file) = &mut self.backend else {
            unreachable!();
        };
        writeln!(
            file,
            "break hardware={} boot={} at={} reason={:?}",
            format_hardware_id(observed.hardware_id),
            observed.boot_epoch.get(),
            observed.at.get(),
            observed.reason,
        )?;
        file.sync_data()?;
        self.stats.continuity_breaks = self.stats.continuity_breaks.saturating_add(1);
        Ok(())
    }
}

fn run_probe(
    config: SerialPortConfig,
    requirements: HandshakeRequirements,
    confirm_trial: bool,
    expected_firmware: Option<FirmwareVersion>,
) -> Result<(), HilError> {
    let port = SerialChannelPort::open(&config).map_err(HilError::Port)?;
    let policy = ChannelPolicy {
        transaction_timeout: DurationMicros::new(250_000).map_err(HilError::Transaction)?,
        retry_delay: DurationMicros::new(25_000).map_err(HilError::Transaction)?,
        measurement_attempts_per_slot: 2,
        max_pending_work_items: 4,
    };
    let session_seed = process_session_seed();
    let mut agent = ChannelAgent::new(
        session_seed,
        session_seed.rotate_left(13),
        DurationMicros::new(50_000).map_err(HilError::Transaction)?,
        policy,
    )
    .map_err(HilError::Agent)?;
    agent
        .submit_handshake(requirements)
        .map_err(HilError::Agent)?;
    let mut endpoint = ChannelEndpoint::new(agent, port, SystemMonotonicClock::new());
    let deadline = Instant::now()
        + if confirm_trial {
            TRIAL_CONFIRM_LIMIT
        } else {
            PROBE_LIMIT
        };
    let mut decode_errors = 0u32;
    let mut timeouts = 0u32;
    let mut confirm_operation_id = None;

    while Instant::now() < deadline {
        match endpoint.step().map_err(HilError::Endpoint)? {
            ChannelEndpointEvent::Agent(ChannelEvent::HandshakeComplete {
                identity,
                capabilities,
                ..
            }) => {
                println!(
                    "handshake ok: node={} hardware={} board={} firmware={}.{}.{} protocol={}/{} capability_schema={} services=0x{:08x} features=0x{:016x} journal={} max_chunk={} output_rates_hz={} decode_errors={} timeouts={}",
                    identity.node_address,
                    format_hardware_id(identity.hardware_id),
                    identity.board_revision,
                    identity.firmware.major,
                    identity.firmware.minor,
                    identity.firmware.patch,
                    identity.protocol.framing,
                    identity.protocol.application,
                    capabilities.schema_version,
                    capabilities.service_flags,
                    capabilities.feature_flags,
                    capabilities.journal_capacity_samples,
                    capabilities.max_sample_chunk_samples,
                    format_output_rates(capabilities),
                    decode_errors,
                    timeouts,
                );
                if !confirm_trial {
                    return Ok(());
                }
                if Some(identity.firmware) != expected_firmware {
                    return Err(HilError::FirmwareVersionMismatch {
                        expected: expected_firmware.ok_or(HilError::MissingExpectedFirmware)?,
                        received: identity.firmware,
                    });
                }
                let operation_id = endpoint
                    .agent_mut()
                    .submit_lifecycle(
                        identity.node_address,
                        RuntimeOperation::ConfirmTrialBoot,
                        ObservabilityProfile::Production,
                    )
                    .map_err(HilError::Agent)?;
                confirm_operation_id = Some(operation_id);
            }
            ChannelEndpointEvent::Agent(ChannelEvent::HandshakeTimedOut { .. }) => {
                timeouts = timeouts.saturating_add(1);
            }
            ChannelEndpointEvent::DecodeError(_) => {
                decode_errors = decode_errors.saturating_add(1);
            }
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete { result, .. })
                if Some(result.operation_id) == confirm_operation_id =>
            {
                if result.code != RuntimeResultCode::Ok {
                    return Err(HilError::TrialConfirmationRejected(result.code));
                }
                println!(
                    "trial boot confirmed: operation_id={} state={:?}",
                    result.operation_id, result.state
                );
                return Ok(());
            }
            ChannelEndpointEvent::Disconnected => return Err(HilError::Disconnected),
            _ => {}
        }
    }

    Err(HilError::ProbeTimedOut {
        decode_errors,
        transaction_timeouts: timeouts,
    })
}

enum HilOutput {
    Diagnostic(PathBuf),
    Capture {
        directory: PathBuf,
        window_duration_us: u64,
    },
}

impl HilOutput {
    fn path(&self) -> &PathBuf {
        match self {
            Self::Diagnostic(path) => path,
            Self::Capture { directory, .. } => directory,
        }
    }
}

fn run_record_cycle(
    config: SerialPortConfig,
    requirements: HandshakeRequirements,
    output: HilOutput,
    sample_target: u64,
    chunk_limit: u16,
) -> Result<(), HilError> {
    let mut port = SerialChannelPort::open(&config).map_err(HilError::Port)?;
    let initial_stream_state = inspect_stream_state(&mut port, requirements.node_address)?;
    let policy = ChannelPolicy {
        transaction_timeout: DurationMicros::new(250_000).map_err(HilError::Transaction)?,
        retry_delay: DurationMicros::new(25_000).map_err(HilError::Transaction)?,
        measurement_attempts_per_slot: 2,
        max_pending_work_items: 4,
    };
    let session_seed = process_session_seed();
    let mut agent = ChannelAgent::new(
        session_seed,
        session_seed.rotate_left(13),
        DurationMicros::new(50_000).map_err(HilError::Transaction)?,
        policy,
    )
    .map_err(HilError::Agent)?;
    agent
        .submit_handshake(requirements)
        .map_err(HilError::Agent)?;
    let mut endpoint = ChannelEndpoint::new(agent, port, SystemMonotonicClock::new());
    let mut sink = match &output {
        HilOutput::Diagnostic(path) => Some(HilDurableSink::create_diagnostic(path)?),
        HilOutput::Capture { .. } => None,
    };
    let deadline = Instant::now() + record_cycle_limit();
    let mut phase = RecordCyclePhase::Handshake;
    let mut state_operation_id = None;
    let mut exit_maintenance_operation_id = None;
    let mut start_operation_id = None;
    let mut stop_operation_id = None;
    let mut maintenance_operation_id = None;
    let mut held_transfer = None;
    let mut final_cursors = None;
    let mut decode = DecodeStats::default();
    let mut transaction_timeouts = 0u32;

    while Instant::now() < deadline {
        let event = endpoint.step().map_err(HilError::Endpoint)?;
        match event {
            ChannelEndpointEvent::Agent(ChannelEvent::HandshakeComplete {
                identity,
                capabilities,
                ..
            }) if phase == RecordCyclePhase::Handshake => {
                if initial_stream_state.boot_epoch != identity.boot_epoch {
                    return Err(HilError::InitialStreamChanged {
                        inspected: initial_stream_state.boot_epoch,
                        handshaken: identity.boot_epoch,
                    });
                }
                if let HilOutput::Capture {
                    directory,
                    window_duration_us,
                } = &output
                {
                    sink = Some(HilDurableSink::create_capture(
                        directory.clone(),
                        identity.hardware_id,
                        identity.boot_epoch,
                        initial_stream_state.produced,
                        *window_duration_us,
                    )?);
                }
                let operation_id = endpoint
                    .agent_mut()
                    .submit_lifecycle(
                        identity.node_address,
                        RuntimeOperation::GetState,
                        ObservabilityProfile::Production,
                    )
                    .map_err(HilError::Agent)?;
                state_operation_id = Some(operation_id);
                let effective_chunk_limit = capabilities.max_sample_chunk_samples.min(chunk_limit);
                held_transfer = Some(
                    MeasurementTransfer::new(
                        identity.node_address,
                        effective_chunk_limit,
                        NodeSession::recovered(
                            identity.hardware_id,
                            identity.boot_epoch,
                            initial_stream_state.produced,
                            initial_stream_state.produced,
                        )
                        .map_err(HilError::Session)?,
                    )
                    .map_err(HilError::Measurement)?,
                );
                phase = RecordCyclePhase::ReadingState;
                println!(
                    "record-cycle handshake: node={} boot_epoch={} baseline_cursor={} chunk_limit={}",
                    identity.node_address,
                    identity.boot_epoch.get(),
                    initial_stream_state.produced.get(),
                    effective_chunk_limit,
                );
            }
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete { result, .. })
                if Some(result.operation_id) == state_operation_id =>
            {
                if result.code != RuntimeResultCode::Ok {
                    return Err(HilError::UnexpectedLifecycleResult {
                        code: result.code,
                        state: result.state,
                        expected_state: RuntimeState::Idle,
                    });
                }
                if result.state == RuntimeState::Maintenance {
                    exit_maintenance_operation_id = Some(
                        endpoint
                            .agent_mut()
                            .submit_lifecycle(
                                requirements.node_address,
                                RuntimeOperation::ExitMaintenance,
                                ObservabilityProfile::Production,
                            )
                            .map_err(HilError::Agent)?,
                    );
                    phase = RecordCyclePhase::ExitingMaintenance;
                } else if result.state == RuntimeState::Idle {
                    start_operation_id = Some(
                        endpoint
                            .agent_mut()
                            .submit_lifecycle(
                                requirements.node_address,
                                RuntimeOperation::Start,
                                ObservabilityProfile::Production,
                            )
                            .map_err(HilError::Agent)?,
                    );
                    phase = RecordCyclePhase::Starting;
                } else if result.state == RuntimeState::Faulted {
                    return Err(HilError::NodeFaulted);
                } else {
                    return Err(HilError::NodeNotIdle(result.state));
                }
            }
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete { result, .. })
                if Some(result.operation_id) == exit_maintenance_operation_id =>
            {
                require_lifecycle_result(result.code, result.state, RuntimeState::Idle)?;
                start_operation_id = Some(
                    endpoint
                        .agent_mut()
                        .submit_lifecycle(
                            requirements.node_address,
                            RuntimeOperation::Start,
                            ObservabilityProfile::Production,
                        )
                        .map_err(HilError::Agent)?,
                );
                phase = RecordCyclePhase::Starting;
            }
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete { result, .. })
                if Some(result.operation_id) == start_operation_id =>
            {
                require_lifecycle_result(result.code, result.state, RuntimeState::Recording)?;
                let transfer = held_transfer.take().ok_or(HilError::RecordCycleInvariant)?;
                endpoint
                    .agent_mut()
                    .submit_measurement(transfer)
                    .map_err(HilError::Agent)?;
                phase = RecordCyclePhase::Recording;
                println!("record-cycle started: operation_id={}", result.operation_id);
            }
            ChannelEndpointEvent::Agent(ChannelEvent::MeasurementPersistenceRequired {
                kind,
                ..
            }) if phase == RecordCyclePhase::Recording || phase == RecordCyclePhase::Draining => {
                let mut transfer = endpoint
                    .agent_mut()
                    .take_completed_measurement()
                    .ok_or(HilError::RecordCycleInvariant)?;
                let sink = sink.as_mut().ok_or(HilError::RecordCycleInvariant)?;
                persist_pending(&mut transfer, sink)
                    .map_err(|error| HilError::Persistence(error.to_string()))?;
                match kind {
                    PersistenceKind::SampleChunk => {}
                    PersistenceKind::Gap(gap) => println!(
                        "record-cycle persisted gap: first={} count={} reason={:?}",
                        gap.first_missing.get(),
                        gap.missing_count,
                        gap.reason
                    ),
                    PersistenceKind::ContinuityBreak(observed) => println!(
                        "record-cycle persisted continuity break: at={} reason={:?}",
                        observed.at.get(),
                        observed.reason
                    ),
                }

                if phase == RecordCyclePhase::Recording && sink.stats.samples >= sample_target {
                    held_transfer = Some(transfer);
                    let operation_id = endpoint
                        .agent_mut()
                        .submit_lifecycle(
                            requirements.node_address,
                            RuntimeOperation::Stop,
                            ObservabilityProfile::Production,
                        )
                        .map_err(HilError::Agent)?;
                    stop_operation_id = Some(operation_id);
                    phase = RecordCyclePhase::Stopping;
                } else {
                    endpoint
                        .agent_mut()
                        .submit_measurement(transfer)
                        .map_err(HilError::Agent)?;
                }
            }
            ChannelEndpointEvent::Agent(ChannelEvent::MeasurementNoData { .. })
                if phase == RecordCyclePhase::Recording =>
            {
                let transfer = endpoint
                    .agent_mut()
                    .take_completed_measurement()
                    .ok_or(HilError::RecordCycleInvariant)?;
                endpoint
                    .agent_mut()
                    .submit_measurement(transfer)
                    .map_err(HilError::Agent)?;
            }
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete { result, .. })
                if Some(result.operation_id) == stop_operation_id =>
            {
                require_lifecycle_result(result.code, result.state, RuntimeState::Idle)?;
                let transfer = held_transfer.take().ok_or(HilError::RecordCycleInvariant)?;
                endpoint
                    .agent_mut()
                    .submit_measurement(transfer)
                    .map_err(HilError::Agent)?;
                phase = RecordCyclePhase::Draining;
                println!("record-cycle stopped: operation_id={}", result.operation_id);
            }
            ChannelEndpointEvent::Agent(ChannelEvent::MeasurementNoData { .. })
                if phase == RecordCyclePhase::Draining =>
            {
                let transfer = endpoint
                    .agent_mut()
                    .take_completed_measurement()
                    .ok_or(HilError::RecordCycleInvariant)?;
                let cursors = transfer.session().cursors();
                if cursors.released() != cursors.durable()
                    || cursors.durable() != cursors.received()
                {
                    return Err(HilError::ReleaseNotAcknowledged);
                }
                final_cursors = Some(cursors);
                let operation_id = endpoint
                    .agent_mut()
                    .submit_lifecycle(
                        requirements.node_address,
                        RuntimeOperation::EnterMaintenance,
                        ObservabilityProfile::Production,
                    )
                    .map_err(HilError::Agent)?;
                maintenance_operation_id = Some(operation_id);
                phase = RecordCyclePhase::EnteringMaintenance;
            }
            ChannelEndpointEvent::Agent(ChannelEvent::LifecycleComplete { result, .. })
                if Some(result.operation_id) == maintenance_operation_id =>
            {
                require_lifecycle_result(result.code, result.state, RuntimeState::Maintenance)?;
                let sink = sink.as_mut().ok_or(HilError::RecordCycleInvariant)?;
                sink.finalize()?;
                let cursors = final_cursors.ok_or(HilError::RecordCycleInvariant)?;
                println!(
                    "record-cycle complete: samples={} gaps={} continuity_breaks={} received={} durable={} released={} decode_errors={} transaction_timeouts={} state={:?} output={}",
                    sink.stats.samples,
                    sink.stats.gaps,
                    sink.stats.continuity_breaks,
                    cursors.received().get(),
                    cursors.durable().get(),
                    cursors.released().get(),
                    decode.total(),
                    transaction_timeouts,
                    result.state,
                    output.path().display(),
                );
                return Ok(());
            }
            ChannelEndpointEvent::Agent(ChannelEvent::MeasurementRecoveryRequired {
                reason,
                ..
            }) => return Err(HilError::MeasurementRecovery(reason)),
            ChannelEndpointEvent::Agent(ChannelEvent::MeasurementTimedOut { .. })
            | ChannelEndpointEvent::Agent(ChannelEvent::LifecycleTimedOut { .. })
            | ChannelEndpointEvent::Agent(ChannelEvent::HandshakeTimedOut { .. }) => {
                transaction_timeouts = transaction_timeouts.saturating_add(1);
            }
            ChannelEndpointEvent::DecodeError(error) => {
                decode.record(error);
            }
            ChannelEndpointEvent::Disconnected => return Err(HilError::Disconnected),
            _ => {}
        }
    }

    Err(HilError::RecordCycleTimedOut {
        phase,
        samples: sink.as_ref().map_or(0, |value| value.stats.samples),
        decode_errors: decode.total(),
        transaction_timeouts,
        decode,
    })
}

fn require_lifecycle_result(
    code: RuntimeResultCode,
    state: RuntimeState,
    expected_state: RuntimeState,
) -> Result<(), HilError> {
    if code != RuntimeResultCode::Ok || state != expected_state {
        Err(HilError::UnexpectedLifecycleResult {
            code,
            state,
            expected_state,
        })
    } else {
        Ok(())
    }
}

/// Read-only discovery for a protocol-5 node whose hardware identity is not
/// yet present in installation inventory. Recording and trial confirmation
/// still require the explicit fail-closed handshake below.
fn run_identify(config: SerialPortConfig, node_address: u8) -> Result<(), HilError> {
    let mut port = SerialChannelPort::open(&config).map_err(HilError::Port)?;
    let clock = SystemMonotonicClock::new();
    let timeout = DurationMicros::new(250_000).map_err(HilError::Transaction)?;
    let quarantine = DurationMicros::new(50_000).map_err(HilError::Transaction)?;
    let mut transactions = TransactionCore::new(process_session_seed(), quarantine);

    let identity_frame = inspect_request(
        &mut port,
        &clock,
        &mut transactions,
        identity_request(node_address, timeout),
    )?;
    let identity = NodeIdentity::decode(identity_frame.payload())
        .map_err(HilError::Identity)?
        .require_local_protocol()
        .map_err(HilError::Identity)?;
    if identity.node_address != node_address {
        return Err(HilError::UnexpectedInspectionResponse);
    }

    println!(
        "identity: node={} hardware={} board={} firmware={}.{}.{} protocol={}/{} boot_epoch={}",
        identity.node_address,
        format_hardware_id(identity.hardware_id),
        identity.board_revision,
        identity.firmware.major,
        identity.firmware.minor,
        identity.firmware.patch,
        identity.protocol.framing,
        identity.protocol.application,
        identity.boot_epoch.get(),
    );

    let capabilities_frame = inspect_request(
        &mut port,
        &clock,
        &mut transactions,
        capabilities_request(node_address, timeout),
    )?;
    let capabilities =
        NodeCapabilities::decode(capabilities_frame.payload()).map_err(HilError::Identity)?;

    println!(
        "capabilities: schema={} services=0x{:08x} features=0x{:016x} journal={} max_chunk={} output_rates_hz={} output_rate_mask=0x{:04x}",
        capabilities.schema_version,
        capabilities.service_flags,
        capabilities.feature_flags,
        capabilities.journal_capacity_samples,
        capabilities.max_sample_chunk_samples,
        format_output_rates(capabilities),
        capabilities.output_rate_mask,
    );
    Ok(())
}

fn format_output_rates(capabilities: NodeCapabilities) -> String {
    OutputRateCode::ALL
        .iter()
        .copied()
        .filter(|rate| capabilities.supports_output_rate(*rate))
        .map(OutputRateCode::hertz_label)
        .collect::<Vec<_>>()
        .join(",")
}

fn inspect_stream_state(
    port: &mut SerialChannelPort,
    node_address: u8,
) -> Result<StreamState, HilError> {
    let clock = SystemMonotonicClock::new();
    let timeout = DurationMicros::new(250_000).map_err(HilError::Transaction)?;
    let quarantine = DurationMicros::new(50_000).map_err(HilError::Transaction)?;
    let mut transactions = TransactionCore::new(process_session_seed(), quarantine);
    let frame = inspect_request(
        port,
        &clock,
        &mut transactions,
        sensor_host::transaction::RequestSpec {
            node_address,
            service: ServiceId::Measurement as u8,
            request_message: GET_STREAM_STATE_MESSAGE_ID,
            response_message: GET_STREAM_STATE_MESSAGE_ID,
            timeout,
            timeout_recovery: sensor_host::transaction::TimeoutRecovery::RetrySafe,
        },
    )?;
    decode_stream_state_success(frame.payload()).map_err(HilError::MeasurementWire)
}

fn inspect_request(
    port: &mut SerialChannelPort,
    clock: &SystemMonotonicClock,
    transactions: &mut TransactionCore,
    request: sensor_host::transaction::RequestSpec,
) -> Result<Frame, HilError> {
    let prepared = transactions
        .prepare(request)
        .map_err(HilError::Transaction)?;
    let frame = prepared.request_frame(&[]).map_err(HilError::Frame)?;
    if let Err(error) = port.transmit(&frame) {
        transactions
            .cancel_transmit()
            .map_err(HilError::Transaction)?;
        return Err(HilError::Port(error));
    }
    transactions
        .mark_transmitted(clock.now())
        .map_err(HilError::Transaction)?;
    let mut decode_errors = 0u32;

    loop {
        match port.poll_receive().map_err(HilError::Port)? {
            ReceiveEvent::NoData => {}
            ReceiveEvent::FrameReady => {
                let response = port
                    .take_received()
                    .ok_or(HilError::UnexpectedInspectionResponse)?;
                if transactions
                    .receive(clock.now(), &response)
                    .map_err(HilError::Transaction)?
                    == ResponseDisposition::Accepted
                {
                    return Ok(response);
                }
            }
            ReceiveEvent::DecodeError(_) => {
                decode_errors = decode_errors.saturating_add(1);
            }
            ReceiveEvent::Disconnected => return Err(HilError::Disconnected),
        }
        if matches!(
            transactions
                .on_timer(clock.now())
                .map_err(HilError::Transaction)?,
            TimerEvent::TimedOut { .. }
        ) {
            return Err(HilError::InspectionTimedOut { decode_errors });
        }
    }
}

enum Command {
    Identify {
        device: PathBuf,
        node_address: u8,
        baud_rate: u32,
    },
    Handshake {
        device: PathBuf,
        node_address: u8,
        hardware_id: HardwareId,
        board_revision: u8,
        baud_rate: u32,
    },
    ConfirmTrial {
        device: PathBuf,
        node_address: u8,
        hardware_id: HardwareId,
        board_revision: u8,
        expected_firmware: FirmwareVersion,
        baud_rate: u32,
    },
    RecordCycle {
        device: PathBuf,
        node_address: u8,
        hardware_id: HardwareId,
        board_revision: u8,
        output: PathBuf,
        sample_target: u64,
        chunk_limit: u16,
        baud_rate: u32,
    },
    CaptureCycle {
        device: PathBuf,
        node_address: u8,
        hardware_id: HardwareId,
        board_revision: u8,
        output_directory: PathBuf,
        window_duration_us: u64,
        sample_target: u64,
        chunk_limit: u16,
        baud_rate: u32,
    },
    SafeStop {
        device: PathBuf,
        node_address: u8,
        hardware_id: HardwareId,
        board_revision: u8,
        baud_rate: u32,
    },
    PrepareUpdate {
        device: PathBuf,
        node_address: u8,
        hardware_id: HardwareId,
        board_revision: u8,
        baud_rate: u32,
    },
    TimeProbe {
        device: PathBuf,
        node_address: u8,
        hardware_id: HardwareId,
        board_revision: u8,
        observation_count: u16,
        interval_ms: u64,
        baud_rate: u32,
    },
}

fn parse_arguments(arguments: &[String]) -> Result<Command, HilError> {
    let identify = arguments.first().is_some_and(|value| value == "identify");
    let handshake = arguments.first().is_some_and(|value| value == "handshake");
    let confirm = arguments
        .first()
        .is_some_and(|value| value == "confirm-trial");
    let record_cycle = arguments
        .first()
        .is_some_and(|value| value == "record-cycle");
    let capture_cycle = arguments
        .first()
        .is_some_and(|value| value == "capture-cycle");
    let safe_stop = arguments.first().is_some_and(|value| value == "safe-stop");
    let prepare_update = arguments
        .first()
        .is_some_and(|value| value == "prepare-update");
    let time_probe = arguments.first().is_some_and(|value| value == "time-probe");
    if (!identify
        && !handshake
        && !confirm
        && !record_cycle
        && !capture_cycle
        && !safe_stop
        && !prepare_update
        && !time_probe)
        || (identify && !(arguments.len() == 3 || arguments.len() == 4))
        || (handshake && !(arguments.len() == 5 || arguments.len() == 6))
        || (confirm && !(arguments.len() == 6 || arguments.len() == 7))
        || (record_cycle && !(arguments.len() >= 6 && arguments.len() <= 9))
        || (capture_cycle && !(arguments.len() >= 7 && arguments.len() <= 10))
        || (safe_stop && !(arguments.len() == 5 || arguments.len() == 6))
        || (prepare_update && !(arguments.len() == 5 || arguments.len() == 6))
        || (time_probe && !(arguments.len() >= 5 && arguments.len() <= 8))
    {
        return Err(HilError::Usage);
    }
    let node_address = arguments[2]
        .parse::<u8>()
        .map_err(|_| HilError::InvalidNodeAddress)?;
    if node_address == 0 || node_address == u8::MAX {
        return Err(HilError::InvalidNodeAddress);
    }
    if identify {
        let baud_rate = arguments
            .get(3)
            .map(|value| value.parse::<u32>().map_err(|_| HilError::InvalidBaudRate))
            .transpose()?
            .unwrap_or(DEFAULT_BAUD_RATE);
        if baud_rate == 0 {
            return Err(HilError::InvalidBaudRate);
        }
        return Ok(Command::Identify {
            device: PathBuf::from(&arguments[1]),
            node_address,
            baud_rate,
        });
    }
    let hardware_id = parse_hardware_id(&arguments[3])?;
    let board_revision = arguments[4]
        .parse::<u8>()
        .map_err(|_| HilError::InvalidBoardRevision)?;
    if board_revision == 0 {
        return Err(HilError::InvalidBoardRevision);
    }
    if record_cycle || capture_cycle {
        let optional_offset = usize::from(capture_cycle);
        let window_duration_us = if capture_cycle {
            let value = arguments[6]
                .parse::<u64>()
                .map_err(|_| HilError::InvalidWindowDuration)?;
            if value == 0 {
                return Err(HilError::InvalidWindowDuration);
            }
            Some(value)
        } else {
            None
        };
        let sample_target = arguments
            .get(6 + optional_offset)
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| HilError::InvalidSampleTarget)
            })
            .transpose()?
            .unwrap_or(DEFAULT_SAMPLE_TARGET);
        if sample_target == 0 {
            return Err(HilError::InvalidSampleTarget);
        }
        let chunk_limit = arguments
            .get(7 + optional_offset)
            .map(|value| {
                value
                    .parse::<u16>()
                    .map_err(|_| HilError::InvalidChunkLimit)
            })
            .transpose()?
            .unwrap_or(48);
        if chunk_limit == 0 || chunk_limit > 48 {
            return Err(HilError::InvalidChunkLimit);
        }
        let baud_rate = arguments
            .get(8 + optional_offset)
            .map(|value| value.parse::<u32>().map_err(|_| HilError::InvalidBaudRate))
            .transpose()?
            .unwrap_or(DEFAULT_BAUD_RATE);
        if baud_rate == 0 {
            return Err(HilError::InvalidBaudRate);
        }
        let device = PathBuf::from(&arguments[1]);
        let output = PathBuf::from(&arguments[5]);
        return Ok(if let Some(window_duration_us) = window_duration_us {
            Command::CaptureCycle {
                device,
                node_address,
                hardware_id,
                board_revision,
                output_directory: output,
                window_duration_us,
                sample_target,
                chunk_limit,
                baud_rate,
            }
        } else {
            Command::RecordCycle {
                device,
                node_address,
                hardware_id,
                board_revision,
                output,
                sample_target,
                chunk_limit,
                baud_rate,
            }
        });
    }
    if time_probe {
        let observation_count = arguments
            .get(5)
            .map(|value| {
                value
                    .parse::<u16>()
                    .map_err(|_| HilError::InvalidTimeObservationCount)
            })
            .transpose()?
            .unwrap_or(DEFAULT_TIME_OBSERVATIONS);
        if observation_count < 2 {
            return Err(HilError::InvalidTimeObservationCount);
        }
        let interval_ms = arguments
            .get(6)
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| HilError::InvalidTimeInterval)
            })
            .transpose()?
            .unwrap_or(DEFAULT_TIME_INTERVAL_MS);
        if interval_ms == 0 {
            return Err(HilError::InvalidTimeInterval);
        }
        let baud_rate = arguments
            .get(7)
            .map(|value| value.parse::<u32>().map_err(|_| HilError::InvalidBaudRate))
            .transpose()?
            .unwrap_or(DEFAULT_BAUD_RATE);
        if baud_rate == 0 {
            return Err(HilError::InvalidBaudRate);
        }
        return Ok(Command::TimeProbe {
            device: PathBuf::from(&arguments[1]),
            node_address,
            hardware_id,
            board_revision,
            observation_count,
            interval_ms,
            baud_rate,
        });
    }
    if safe_stop || prepare_update {
        let baud_rate = arguments
            .get(5)
            .map(|value| value.parse::<u32>().map_err(|_| HilError::InvalidBaudRate))
            .transpose()?
            .unwrap_or(DEFAULT_BAUD_RATE);
        if baud_rate == 0 {
            return Err(HilError::InvalidBaudRate);
        }
        return Ok(if safe_stop {
            Command::SafeStop {
                device: PathBuf::from(&arguments[1]),
                node_address,
                hardware_id,
                board_revision,
                baud_rate,
            }
        } else {
            Command::PrepareUpdate {
                device: PathBuf::from(&arguments[1]),
                node_address,
                hardware_id,
                board_revision,
                baud_rate,
            }
        });
    }
    let expected_firmware = if confirm {
        Some(parse_firmware_version(&arguments[5])?)
    } else {
        None
    };
    let baud_rate_index = if confirm { 6 } else { 5 };
    let baud_rate = arguments
        .get(baud_rate_index)
        .map(|value| value.parse::<u32>().map_err(|_| HilError::InvalidBaudRate))
        .transpose()?
        .unwrap_or(DEFAULT_BAUD_RATE);
    if baud_rate == 0 {
        return Err(HilError::InvalidBaudRate);
    }
    let fields = (
        PathBuf::from(&arguments[1]),
        node_address,
        hardware_id,
        board_revision,
        baud_rate,
    );
    Ok(if handshake {
        Command::Handshake {
            device: fields.0,
            node_address: fields.1,
            hardware_id: fields.2,
            board_revision: fields.3,
            baud_rate: fields.4,
        }
    } else {
        Command::ConfirmTrial {
            device: fields.0,
            node_address: fields.1,
            hardware_id: fields.2,
            board_revision: fields.3,
            expected_firmware: expected_firmware.ok_or(HilError::MissingExpectedFirmware)?,
            baud_rate: fields.4,
        }
    })
}

fn parse_firmware_version(value: &str) -> Result<FirmwareVersion, HilError> {
    let mut fields = value.split('.');
    let major = fields
        .next()
        .ok_or(HilError::InvalidFirmwareVersion)?
        .parse::<u8>()
        .map_err(|_| HilError::InvalidFirmwareVersion)?;
    let minor = fields
        .next()
        .ok_or(HilError::InvalidFirmwareVersion)?
        .parse::<u8>()
        .map_err(|_| HilError::InvalidFirmwareVersion)?;
    let patch = fields
        .next()
        .ok_or(HilError::InvalidFirmwareVersion)?
        .parse::<u8>()
        .map_err(|_| HilError::InvalidFirmwareVersion)?;
    if fields.next().is_some() {
        return Err(HilError::InvalidFirmwareVersion);
    }
    Ok(FirmwareVersion {
        major,
        minor,
        patch,
    })
}

fn parse_hardware_id(value: &str) -> Result<HardwareId, HilError> {
    if value.len() != 16 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(HilError::InvalidHardwareId);
    }
    let mut bytes = [0; 8];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| HilError::InvalidHardwareId)?;
    }
    Ok(HardwareId::from_bytes(bytes))
}

fn format_hardware_id(hardware_id: HardwareId) -> String {
    hardware_id
        .bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn process_session_seed() -> u32 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let nanos = elapsed.as_nanos();
    let folded = (nanos as u32) ^ ((nanos >> 32) as u32) ^ std::process::id().rotate_left(7);
    if folded == 0 {
        1
    } else {
        folded
    }
}

fn record_cycle_limit() -> Duration {
    std::env::var(HIL_RECORD_LIMIT_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| (1..=3_600).contains(seconds))
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_RECORD_CYCLE_LIMIT)
}

fn print_usage() {
    println!(
        "usage: sensor-hil identify <device> <node-address> [baud-rate]\n       sensor-hil handshake <device> <node-address> <hardware-id-hex> <board-revision> [baud-rate]\n       sensor-hil time-probe <device> <node-address> <hardware-id-hex> <board-revision> [observation-count] [interval-ms] [baud-rate]\n       sensor-hil confirm-trial <device> <node-address> <hardware-id-hex> <board-revision> <expected-firmware> [baud-rate]\n       sensor-hil record-cycle <device> <node-address> <hardware-id-hex> <board-revision> <output> [sample-target] [chunk-limit] [baud-rate]\n       sensor-hil capture-cycle <device> <node-address> <hardware-id-hex> <board-revision> <output-directory> <window-duration-us> [sample-target] [chunk-limit] [baud-rate]\n       sensor-hil safe-stop <device> <node-address> <hardware-id-hex> <board-revision> [baud-rate]\n       sensor-hil prepare-update <device> <node-address> <hardware-id-hex> <board-revision> [baud-rate]"
    );
}

#[derive(Debug)]
enum HilError {
    Usage,
    InvalidNodeAddress,
    InvalidHardwareId,
    InvalidBoardRevision,
    InvalidBaudRate,
    InvalidFirmwareVersion,
    InvalidSampleTarget,
    InvalidChunkLimit,
    InvalidWindowDuration,
    InvalidTimeObservationCount,
    InvalidTimeInterval,
    CaptureOutputExists(PathBuf),
    MissingExpectedFirmware,
    FirmwareVersionMismatch {
        expected: FirmwareVersion,
        received: FirmwareVersion,
    },
    Disconnected,
    ProbeTimedOut {
        decode_errors: u32,
        transaction_timeouts: u32,
    },
    TrialConfirmationRejected(RuntimeResultCode),
    UnexpectedLifecycleResult {
        code: RuntimeResultCode,
        state: RuntimeState,
        expected_state: RuntimeState,
    },
    MeasurementRecovery(RecoveryReason),
    ReleaseNotAcknowledged,
    RecordCycleInvariant,
    NodeFaulted,
    NodeNotIdle(RuntimeState),
    SafeStopTimedOut,
    PrepareUpdateTimedOut,
    TimeProbeHandshakeTimedOut,
    TimeProbeObservationTimedOut(u16),
    TimeProbeNotAccepted,
    TimeProbeInvariant,
    TimeProbeLinkErrors {
        decode_errors: u32,
        transaction_timeouts: u32,
    },
    RecordCycleTimedOut {
        phase: RecordCyclePhase,
        samples: u64,
        decode_errors: u32,
        transaction_timeouts: u32,
        decode: DecodeStats,
    },
    Persistence(String),
    Io(io::Error),
    Session(sensor_host::node_session::SessionError),
    Measurement(sensor_host::measurement_transfer::MeasurementTransferError),
    MeasurementWire(sensor_protocol::MeasurementWireError),
    InitialStreamChanged {
        inspected: sensor_core::BootEpoch,
        handshaken: sensor_core::BootEpoch,
    },
    InspectionTimedOut {
        decode_errors: u32,
    },
    UnexpectedInspectionResponse,
    Identity(IdentityContractError),
    Frame(sensor_protocol::FrameError),
    Transaction(sensor_host::transaction::TransactionError),
    Agent(sensor_host::channel_agent::ChannelAgentError),
    Port(sensor_host::channel_port::ChannelPortError),
    Endpoint(sensor_host::channel_endpoint::ChannelEndpointError),
}

impl fmt::Display for HilError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => formatter.write_str(
                "expected handshake arguments or confirm-trial arguments including expected firmware",
            ),
            Self::InvalidNodeAddress => formatter.write_str("node address must be in 1..254"),
            Self::InvalidHardwareId => {
                formatter.write_str("hardware ID must contain exactly 16 hexadecimal digits")
            }
            Self::InvalidBoardRevision => {
                formatter.write_str("board revision must be a non-zero u8")
            }
            Self::InvalidBaudRate => formatter.write_str("baud rate must be a non-zero u32"),
            Self::InvalidFirmwareVersion => {
                formatter.write_str("firmware version must be MAJOR.MINOR.PATCH with u8 fields")
            }
            Self::InvalidSampleTarget => {
                formatter.write_str("sample target must be a non-zero u64")
            }
            Self::InvalidChunkLimit => {
                formatter.write_str("chunk limit must be in 1..=48")
            }
            Self::InvalidWindowDuration => {
                formatter.write_str("capture window duration must be a non-zero u64")
            }
            Self::InvalidTimeObservationCount => {
                formatter.write_str("time observation count must be in 2..=65535")
            }
            Self::InvalidTimeInterval => {
                formatter.write_str("time observation interval must be a non-zero millisecond value")
            }
            Self::CaptureOutputExists(path) => write!(
                formatter,
                "capture output directory already exists: {}",
                path.display()
            ),
            Self::MissingExpectedFirmware => {
                formatter.write_str("trial confirmation requires an expected firmware version")
            }
            Self::FirmwareVersionMismatch { expected, received } => write!(
                formatter,
                "trial firmware mismatch: expected {}.{}.{}, received {}.{}.{}",
                expected.major,
                expected.minor,
                expected.patch,
                received.major,
                received.minor,
                received.patch,
            ),
            Self::Disconnected => formatter.write_str("serial device disconnected"),
            Self::ProbeTimedOut {
                decode_errors,
                transaction_timeouts,
            } => write!(
                formatter,
                "handshake deadline expired (decode_errors={decode_errors}, transaction_timeouts={transaction_timeouts})"
            ),
            Self::TrialConfirmationRejected(code) => {
                write!(formatter, "node rejected trial confirmation: {code:?}")
            }
            Self::UnexpectedLifecycleResult {
                code,
                state,
                expected_state,
            } => write!(
                formatter,
                "unexpected lifecycle result: code={code:?} state={state:?} expected={expected_state:?}"
            ),
            Self::MeasurementRecovery(reason) => {
                write!(formatter, "measurement requires recovery: {reason:?}")
            }
            Self::ReleaseNotAcknowledged => formatter.write_str(
                "node did not acknowledge release through the durable cursor",
            ),
            Self::RecordCycleInvariant => {
                formatter.write_str("record-cycle internal invariant violated")
            }
            Self::NodeFaulted => formatter.write_str("node is in Faulted state"),
            Self::NodeNotIdle(state) => {
                write!(formatter, "node must be Idle or Maintenance, received {state:?}")
            }
            Self::SafeStopTimedOut => {
                formatter.write_str("safe-stop deadline expired before Maintenance")
            }
            Self::PrepareUpdateTimedOut => {
                formatter.write_str("prepare-update deadline expired")
            }
            Self::TimeProbeHandshakeTimedOut => {
                formatter.write_str("time-probe handshake deadline expired")
            }
            Self::TimeProbeObservationTimedOut(index) => write!(
                formatter,
                "time observation {} did not complete before its deadline",
                index + 1
            ),
            Self::TimeProbeNotAccepted => {
                formatter.write_str("node did not accept the Time Service request")
            }
            Self::TimeProbeInvariant => {
                formatter.write_str("time-probe produced no usable observations")
            }
            Self::TimeProbeLinkErrors {
                decode_errors,
                transaction_timeouts,
            } => write!(
                formatter,
                "time-probe link was not clean (decode_errors={decode_errors}, transaction_timeouts={transaction_timeouts})"
            ),
            Self::RecordCycleTimedOut {
                phase,
                samples,
                decode_errors,
                transaction_timeouts,
                decode,
            } => write!(
                formatter,
                "record-cycle deadline expired (phase={phase:?}, samples={samples}, decode_errors={decode_errors}, transaction_timeouts={transaction_timeouts}, decode={decode:?})"
            ),
            Self::Persistence(error) => write!(formatter, "durable HIL persistence failed: {error}"),
            Self::Io(error) => write!(formatter, "HIL output failed: {error}"),
            Self::Session(error) => write!(formatter, "measurement session failed: {error}"),
            Self::Measurement(error) => write!(formatter, "measurement transfer failed: {error}"),
            Self::MeasurementWire(error) => {
                write!(formatter, "stream-state inspection failed: {error}")
            }
            Self::InitialStreamChanged {
                inspected,
                handshaken,
            } => write!(
                formatter,
                "node rebooted during startup: inspected boot epoch {}, handshake boot epoch {}",
                inspected.get(),
                handshaken.get()
            ),
            Self::InspectionTimedOut { decode_errors } => write!(
                formatter,
                "identity inspection timed out (decode_errors={decode_errors})"
            ),
            Self::UnexpectedInspectionResponse => {
                formatter.write_str("identity inspection received an unexpected response")
            }
            Self::Identity(error) => write!(formatter, "identity inspection failed: {error}"),
            Self::Frame(error) => write!(formatter, "cannot build inspection frame: {error}"),
            Self::Transaction(error) => write!(formatter, "invalid timing policy: {error}"),
            Self::Agent(error) => write!(formatter, "channel agent failed: {error}"),
            Self::Port(error) => write!(formatter, "serial port failed: {error}"),
            Self::Endpoint(error) => write!(formatter, "channel endpoint failed: {error}"),
        }
    }
}

impl std::error::Error for HilError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_read_only_identity_inspection() {
        let arguments = vec![
            "identify".to_owned(),
            "/dev/ttyUSB0".to_owned(),
            "7".to_owned(),
        ];
        assert!(matches!(
            parse_arguments(&arguments),
            Ok(Command::Identify {
                node_address: 7,
                baud_rate: 115_200,
                ..
            })
        ));
    }

    #[test]
    fn parses_explicit_hil_identity_without_hidden_board_default() {
        let arguments = vec![
            "handshake".to_owned(),
            "/dev/ttyUSB0".to_owned(),
            "7".to_owned(),
            "0102030405060708".to_owned(),
            "2".to_owned(),
            "115200".to_owned(),
        ];
        let Command::Handshake {
            node_address,
            hardware_id,
            board_revision,
            baud_rate,
            ..
        } = parse_arguments(&arguments).unwrap()
        else {
            panic!("expected handshake command");
        };
        assert_eq!(node_address, 7);
        assert_eq!(hardware_id.bytes(), [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(board_revision, 2);
        assert_eq!(baud_rate, 115_200);
    }

    #[test]
    fn parses_time_probe_with_explicit_sampling_policy() {
        let arguments = vec![
            "time-probe".to_owned(),
            "/dev/ttyUSB0".to_owned(),
            "7".to_owned(),
            "0102030405060708".to_owned(),
            "2".to_owned(),
            "8".to_owned(),
            "250".to_owned(),
            "115200".to_owned(),
        ];
        assert!(matches!(
            parse_arguments(&arguments),
            Ok(Command::TimeProbe {
                node_address: 7,
                board_revision: 2,
                observation_count: 8,
                interval_ms: 250,
                baud_rate: 115_200,
                ..
            })
        ));
    }

    #[test]
    fn rejects_broadcast_and_ambiguous_hardware_identity() {
        let mut arguments = vec![
            "handshake".to_owned(),
            "/dev/ttyUSB0".to_owned(),
            "255".to_owned(),
            "0102030405060708".to_owned(),
            "2".to_owned(),
            "115200".to_owned(),
        ];
        assert!(matches!(
            parse_arguments(&arguments),
            Err(HilError::InvalidNodeAddress)
        ));
        arguments[2] = "7".to_owned();
        arguments[3] = "1234".to_owned();
        assert!(matches!(
            parse_arguments(&arguments),
            Err(HilError::InvalidHardwareId)
        ));
    }

    #[test]
    fn parses_explicit_trial_confirmation_command() {
        let arguments = vec![
            "confirm-trial".to_owned(),
            "/dev/ttyUSB0".to_owned(),
            "7".to_owned(),
            "0102030405060708".to_owned(),
            "2".to_owned(),
            "0.4.1".to_owned(),
        ];
        assert!(matches!(
            parse_arguments(&arguments),
            Ok(Command::ConfirmTrial {
                node_address: 7,
                board_revision: 2,
                expected_firmware: FirmwareVersion {
                    major: 0,
                    minor: 4,
                    patch: 1,
                },
                baud_rate: 115_200,
                ..
            })
        ));
    }

    #[test]
    fn parses_record_cycle_with_explicit_output_and_limits() {
        let arguments = vec![
            "record-cycle".to_owned(),
            "/dev/ttyUSB0".to_owned(),
            "7".to_owned(),
            "0102030405060708".to_owned(),
            "2".to_owned(),
            "var/tmp/cycle.txt".to_owned(),
            "500".to_owned(),
            "8".to_owned(),
            "115200".to_owned(),
        ];
        let Command::RecordCycle {
            node_address,
            output,
            sample_target,
            chunk_limit,
            baud_rate,
            ..
        } = parse_arguments(&arguments).unwrap()
        else {
            panic!("expected record-cycle command");
        };
        assert_eq!(node_address, 7);
        assert_eq!(output, PathBuf::from("var/tmp/cycle.txt"));
        assert_eq!(sample_target, 500);
        assert_eq!(chunk_limit, 8);
        assert_eq!(baud_rate, 115_200);
    }

    #[test]
    fn parses_production_capture_cycle_with_explicit_window() {
        let arguments = vec![
            "capture-cycle".to_owned(),
            "/dev/ttyUSB0".to_owned(),
            "7".to_owned(),
            "0102030405060708".to_owned(),
            "2".to_owned(),
            "var/tmp/capture".to_owned(),
            "2000000".to_owned(),
            "2500".to_owned(),
            "48".to_owned(),
            "115200".to_owned(),
        ];
        assert!(matches!(
            parse_arguments(&arguments),
            Ok(Command::CaptureCycle {
                node_address: 7,
                window_duration_us: 2_000_000,
                sample_target: 2_500,
                chunk_limit: 48,
                baud_rate: 115_200,
                ..
            })
        ));
    }

    #[test]
    fn process_session_seed_is_never_reserved_zero() {
        assert_ne!(process_session_seed(), 0);
    }
}
