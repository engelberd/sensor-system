use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sensor_core::{BootEpoch, HardwareId, SampleAxes, SampleAxis, SampleRate, SequenceCursor};
use sensor_protocol::{
    decode_configuration, decode_lifecycle_request, decode_pull_request,
    encode_get_config_response, encode_lifecycle_response, encode_pull_response,
    encode_stream_state_success, encode_time_success, encode_validate_config_response,
    validate_time_request, Adxl355Odr, ConfigField, ConfigValidationCode, ConfigValidationResult,
    DecimationFilterProfile, FirmwareVersion, Frame, FrameRole, MeasurementConfiguration,
    NodeCapabilities, NodeIdentity, ObservabilityProfile, ObservedConfiguration, OutputRateCode,
    PullResponse, PullSamplesRequest, RuntimeOperation, RuntimeResult, RuntimeResultCode,
    RuntimeState, SampleChunk, SampleChunkHeader, SensorStatusCode, ServiceId, StreamDecoder,
    StreamState, TimeExchangeResponse, TimingEvidence, CAPABILITIES_SCHEMA_VERSION,
    CONFIGURATION_SERVICE_ID, GET_CAPABILITIES_MESSAGE_ID, GET_CONFIG_MESSAGE_ID,
    GET_IDENTITY_MESSAGE_ID, GET_STREAM_STATE_MESSAGE_ID, HOST_ADDRESS, IDENTITY_SERVICE_ID,
    LIFECYCLE_SERVICE_ID, LOCAL_PROTOCOL_VERSION, MAX_PAYLOAD_BYTES, MAX_WIRE_FRAME_BYTES,
    OBSERVE_CLOCK_MESSAGE_ID, PULL_SAMPLES_MESSAGE_ID, TIME_SERVICE_ID, TIMING_VALID,
    VALIDATE_CONFIG_MESSAGE_ID,
};

use crate::channel_endpoint::MonotonicClock;
use crate::transaction::MonotonicMicros;

const NANO_HERTZ_MICROS_PER_SAMPLE: u128 = 1_000_000_000_000_000;
const DEFAULT_BAUD_RATE: u32 = 115_200;
const DEFAULT_TURNAROUND_US: u64 = 1_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SimulatedPull {
    Chunk {
        header: SampleChunkHeader,
        samples: Vec<SampleAxes>,
    },
    NoData(StreamState),
    RangeUnavailable(StreamState),
    StreamChanged,
    InvalidRelease,
}

#[derive(Clone, Debug)]
pub struct SimulatedNodeLink {
    boot_epoch: BootEpoch,
    capacity: usize,
    retained_from: u64,
    produced: u64,
    released: u64,
    delivered: u64,
    overwritten: u64,
    samples: VecDeque<SampleAxes>,
}

impl SimulatedNodeLink {
    pub fn new(boot_epoch: BootEpoch, capacity: usize) -> Self {
        assert!(capacity > 0, "simulation journal capacity must be non-zero");
        Self {
            boot_epoch,
            capacity,
            retained_from: 0,
            produced: 0,
            released: 0,
            delivered: 0,
            overwritten: 0,
            samples: VecDeque::with_capacity(capacity),
        }
    }

    pub fn produce(&mut self, samples: impl IntoIterator<Item = SampleAxes>) {
        for sample in samples {
            self.samples.push_back(sample);
            self.produced += 1;
            while self.samples.len() > self.capacity {
                self.samples.pop_front();
                self.retained_from += 1;
                self.overwritten += 1;
            }
        }
    }

    pub fn reboot(&mut self, boot_epoch: BootEpoch) {
        self.boot_epoch = boot_epoch;
        self.retained_from = 0;
        self.produced = 0;
        self.released = 0;
        self.delivered = 0;
        self.overwritten = 0;
        self.samples.clear();
    }

    pub fn pull(&mut self, request: PullSamplesRequest) -> SimulatedPull {
        if request.boot_epoch != self.boot_epoch {
            return SimulatedPull::StreamChanged;
        }
        // A durably recorded RANGE_UNAVAILABLE may advance the host across
        // samples the node already overwrote. Such a release may reach the
        // retention boundary even though those samples were never delivered.
        if request.release_before.get() > self.delivered.max(self.retained_from)
            || request.release_before > request.from
        {
            return SimulatedPull::InvalidRelease;
        }

        self.release_before(request.release_before.get());
        let state = self.state();
        if request.from.get() < self.retained_from {
            return SimulatedPull::RangeUnavailable(state);
        }
        if request.from.get() == self.produced {
            return SimulatedPull::NoData(state);
        }
        if request.from.get() > self.produced {
            return SimulatedPull::InvalidRelease;
        }

        let offset = (request.from.get() - self.retained_from) as usize;
        let count = usize::from(request.max_samples).min(self.samples.len() - offset);
        let samples = self
            .samples
            .iter()
            .skip(offset)
            .take(count)
            .copied()
            .collect();
        let end = request.from.get() + count as u64;
        self.delivered = self.delivered.max(end);
        SimulatedPull::Chunk {
            header: SampleChunkHeader {
                boot_epoch: self.boot_epoch,
                retained_from: SequenceCursor::new(self.retained_from),
                produced: SequenceCursor::new(self.produced),
                first: request.from,
                sample_count: count as u16,
            },
            samples,
        }
    }

    pub fn state(&self) -> StreamState {
        StreamState {
            boot_epoch: self.boot_epoch,
            retained_from: SequenceCursor::new(self.retained_from),
            produced: SequenceCursor::new(self.produced),
            released: SequenceCursor::new(self.released),
            capacity_samples: self.capacity as u32,
            overwritten_samples: self.overwritten,
        }
    }

    fn release_before(&mut self, cursor: u64) {
        self.released = self.released.max(cursor);
        while self.retained_from < cursor {
            self.samples.pop_front();
            self.retained_from += 1;
        }
    }
}

/// Shared deterministic time source for integration simulations.
#[derive(Clone, Debug, Default)]
pub struct SimulatedClock(Arc<AtomicU64>);

impl SimulatedClock {
    pub fn new(now_us: u64) -> Self {
        Self(Arc::new(AtomicU64::new(now_us)))
    }

    pub fn advance(&self, elapsed_us: u64) {
        let _ = self
            .0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(elapsed_us))
            });
    }
}

impl MonotonicClock for SimulatedClock {
    fn now(&self) -> MonotonicMicros {
        MonotonicMicros::new(self.0.load(Ordering::Relaxed))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SimulatedNodeStats {
    pub requests: u64,
    pub responses: u64,
    pub dropped_responses: u64,
    pub delayed_responses: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SimulatedTimeProfile {
    /// Node clock value at the instant this profile is installed.
    pub node_origin_us: u64,
    /// Signed node rate error relative to the simulated host clock.
    pub drift_ppm: i32,
    /// One-way delay after the host's complete request leaves its transmitter.
    pub request_path_us: u64,
    /// One-way delay after the node starts its response transmitter.
    pub response_path_us: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SimulatedNodeClock {
    host_origin_us: u64,
    node_origin_us: u64,
    drift_ppm: i32,
}

impl SimulatedNodeClock {
    fn new(host_origin_us: u64, profile: SimulatedTimeProfile) -> Self {
        assert!(
            profile.drift_ppm > -1_000_000,
            "simulated node clock rate must remain positive"
        );
        Self {
            host_origin_us,
            node_origin_us: profile.node_origin_us,
            drift_ppm: profile.drift_ppm,
        }
    }

    fn at(self, host_us: u64) -> u64 {
        let elapsed = host_us.saturating_sub(self.host_origin_us);
        let rate = i128::from(1_000_000_i64 + i64::from(self.drift_ppm));
        let scaled = i128::from(elapsed).saturating_mul(rate) / 1_000_000;
        i128::from(self.node_origin_us)
            .saturating_add(scaled)
            .clamp(0, i128::from(u64::MAX)) as u64
    }
}

/// One node attached to `SimulatedRs485Bus`.
pub struct SimulatedWireNode {
    address: u8,
    link: SimulatedNodeLink,
    output_rate: SampleRate,
    last_production_us: u64,
    production_remainder: u128,
    responses_to_drop: u32,
    next_response_delay_us: u64,
    offline: bool,
    stats: SimulatedNodeStats,
    time_profile: SimulatedTimeProfile,
    clock: SimulatedNodeClock,
    runtime_state: RuntimeState,
    observability: ObservabilityProfile,
    identity: NodeIdentity,
    capabilities: NodeCapabilities,
    reboot_on_maintenance: Option<BootEpoch>,
    configuration: ObservedConfiguration,
}

impl SimulatedWireNode {
    pub fn new(
        address: u8,
        boot_epoch: BootEpoch,
        journal_capacity: usize,
        output_rate: SampleRate,
    ) -> Self {
        assert!(
            address != HOST_ADDRESS,
            "node address must not be the host address"
        );
        let identity = NodeIdentity {
            hardware_id: HardwareId::from_bytes([address; 8]),
            boot_epoch,
            node_address: address,
            board_revision: 2,
            firmware: FirmwareVersion {
                major: 0,
                minor: 5,
                patch: 8,
            },
            protocol: LOCAL_PROTOCOL_VERSION,
        };
        let capabilities = NodeCapabilities {
            schema_version: CAPABILITIES_SCHEMA_VERSION,
            service_flags: u32::MAX,
            feature_flags: u64::MAX,
            max_payload_bytes: MAX_PAYLOAD_BYTES as u16,
            journal_capacity_samples: journal_capacity as u32,
            output_rate_mask: u16::MAX,
            max_sample_chunk_samples: 48,
        };
        let configuration = ObservedConfiguration {
            revision: 1,
            desired: MeasurementConfiguration {
                output_rate: OutputRateCode::Hz125,
                range_g: 2,
                high_pass_corner: 0,
                filter_profile: DecimationFilterProfile::Balanced,
                fifo_watermark: 30,
                offset_x: 0,
                offset_y: 0,
                offset_z: 0,
            },
            sensor_odr: Adxl355Odr::Hz125,
            decimation_factor: 1,
        };
        Self {
            address,
            link: SimulatedNodeLink::new(boot_epoch, journal_capacity),
            output_rate,
            last_production_us: 0,
            production_remainder: 0,
            responses_to_drop: 0,
            next_response_delay_us: 0,
            offline: false,
            stats: SimulatedNodeStats::default(),
            time_profile: SimulatedTimeProfile::default(),
            clock: SimulatedNodeClock::new(0, SimulatedTimeProfile::default()),
            runtime_state: RuntimeState::Recording,
            observability: ObservabilityProfile::Production,
            identity,
            capabilities,
            reboot_on_maintenance: None,
            configuration,
        }
    }

    pub fn with_identity_contract(
        mut self,
        identity: NodeIdentity,
        capabilities: NodeCapabilities,
    ) -> Self {
        assert_eq!(identity.node_address, self.address);
        assert_eq!(identity.boot_epoch, self.link.state().boot_epoch);
        assert_eq!(
            capabilities.journal_capacity_samples,
            self.link.state().capacity_samples
        );
        self.identity = identity;
        self.capabilities = capabilities;
        self
    }

    pub const fn address(&self) -> u8 {
        self.address
    }

    pub fn with_reboot_on_maintenance(mut self, boot_epoch: BootEpoch) -> Self {
        assert!(boot_epoch.get() != 0, "boot epoch must be non-zero");
        self.reboot_on_maintenance = Some(boot_epoch);
        self
    }

    pub const fn stats(&self) -> SimulatedNodeStats {
        self.stats
    }

    pub fn state(&self) -> StreamState {
        self.link.state()
    }

    pub const fn runtime_state(&self) -> RuntimeState {
        self.runtime_state
    }

    fn produce_until(&mut self, now_us: u64) {
        let elapsed_us = now_us.saturating_sub(self.last_production_us);
        self.last_production_us = now_us;
        if self.runtime_state != RuntimeState::Recording {
            return;
        }
        let scaled = u128::from(self.output_rate.nanohertz())
            .saturating_mul(u128::from(elapsed_us))
            .saturating_add(self.production_remainder);
        let sample_count = scaled / NANO_HERTZ_MICROS_PER_SAMPLE;
        self.production_remainder = scaled % NANO_HERTZ_MICROS_PER_SAMPLE;
        let first = self.link.state().produced.get();
        self.link.produce((0..sample_count).map(|offset| {
            let sequence = first.saturating_add(u64::try_from(offset).unwrap_or(u64::MAX));
            let value = i32::try_from(sequence % 1_000_000).expect("bounded sample value");
            let x = SampleAxis::new(value).expect("bounded sample value");
            let y = SampleAxis::new(-value).expect("bounded sample value");
            let z = SampleAxis::new(value / 2).expect("bounded sample value");
            SampleAxes::new(x, y, z)
        }));
    }

    fn device_time_us(&self, sequence: SequenceCursor) -> u64 {
        let value = u128::from(sequence.get()).saturating_mul(NANO_HERTZ_MICROS_PER_SAMPLE)
            / u128::from(self.output_rate.nanohertz());
        u64::try_from(value).unwrap_or(u64::MAX)
    }

    fn configure_time(&mut self, host_now_us: u64, profile: SimulatedTimeProfile) {
        self.time_profile = profile;
        self.clock = SimulatedNodeClock::new(host_now_us, profile);
    }

    fn reboot(&mut self, host_now_us: u64, boot_epoch: BootEpoch) {
        self.link.reboot(boot_epoch);
        self.identity.boot_epoch = boot_epoch;
        self.last_production_us = host_now_us;
        self.production_remainder = 0;
        self.clock = SimulatedNodeClock::new(
            host_now_us,
            SimulatedTimeProfile {
                node_origin_us: 0,
                ..self.time_profile
            },
        );
    }
}

/// Byte-level, half-duplex RS485 simulation used by the real host composition.
///
/// Requests pass through framing, request decoding, node journal semantics and
/// response framing. This deliberately does not model analog signal integrity
/// or operating-system serial latency; those remain HIL concerns.
pub struct SimulatedRs485Bus {
    clock: SimulatedClock,
    nodes: Vec<SimulatedWireNode>,
    request_decoder: StreamDecoder,
    receive_bytes: VecDeque<u8>,
    response_ready_at_us: Option<u64>,
    baud_rate: u32,
    turnaround_us: u64,
    auto_advance_on_read: bool,
}

impl SimulatedRs485Bus {
    pub fn new(clock: SimulatedClock, nodes: Vec<SimulatedWireNode>) -> Self {
        Self::with_timing(clock, nodes, DEFAULT_BAUD_RATE, DEFAULT_TURNAROUND_US)
    }

    pub fn with_timing(
        clock: SimulatedClock,
        nodes: Vec<SimulatedWireNode>,
        baud_rate: u32,
        turnaround_us: u64,
    ) -> Self {
        assert!(!nodes.is_empty(), "simulation bus needs at least one node");
        assert!(baud_rate > 0, "simulation baud rate must be non-zero");
        for (index, node) in nodes.iter().enumerate() {
            assert!(
                !nodes[..index]
                    .iter()
                    .any(|other| other.address == node.address),
                "simulation node addresses must be unique"
            );
        }
        Self {
            clock,
            nodes,
            request_decoder: StreamDecoder::new(),
            receive_bytes: VecDeque::new(),
            response_ready_at_us: None,
            baud_rate,
            turnaround_us,
            auto_advance_on_read: false,
        }
    }

    /// Lets a background worker use the deterministic bus without an external
    /// simulation driver. A read advances directly to the already scheduled
    /// response; no arbitrary time or sample production is introduced.
    pub fn set_auto_advance_on_read(&mut self, enabled: bool) {
        self.auto_advance_on_read = enabled;
    }

    pub fn drop_next_responses(&mut self, node_address: u8, count: u32) {
        self.node_mut(node_address).responses_to_drop = count;
    }

    pub fn set_offline(&mut self, node_address: u8, offline: bool) {
        self.node_mut(node_address).offline = offline;
    }

    pub fn delay_next_response(&mut self, node_address: u8, extra_delay_us: u64) {
        self.node_mut(node_address).next_response_delay_us = extra_delay_us;
    }

    pub fn configure_time(&mut self, node_address: u8, profile: SimulatedTimeProfile) {
        let now = self.clock.now().get();
        self.node_mut(node_address).configure_time(now, profile);
    }

    pub fn reboot_node(&mut self, node_address: u8, boot_epoch: BootEpoch) {
        assert!(
            boot_epoch.get() != 0,
            "simulated boot epoch must be non-zero"
        );
        let now = self.clock.now().get();
        self.node_mut(node_address).reboot(now, boot_epoch);
    }

    pub fn node(&self, node_address: u8) -> Option<&SimulatedWireNode> {
        self.nodes.iter().find(|node| node.address == node_address)
    }

    fn node_mut(&mut self, node_address: u8) -> &mut SimulatedWireNode {
        self.nodes
            .iter_mut()
            .find(|node| node.address == node_address)
            .expect("simulation node address must exist")
    }

    fn handle_request(&mut self, request: Frame) -> io::Result<()> {
        let now_us = self.clock.now().get();
        for node in &mut self.nodes {
            node.produce_until(now_us);
        }
        if request.role != FrameRole::Request {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "simulation accepts request frames only",
            ));
        }

        let turnaround_us = self.turnaround_us;
        let node = self
            .nodes
            .iter_mut()
            .find(|node| node.address == request.destination)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown node address"))?;
        node.stats.requests = node.stats.requests.saturating_add(1);
        if node.offline {
            return Ok(());
        }

        let mut payload = [0; MAX_PAYLOAD_BYTES];
        let (payload_len, response_started_at_us) = match (request.service, request.message) {
            (IDENTITY_SERVICE_ID, GET_IDENTITY_MESSAGE_ID) => {
                if !request.payload().is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "identity request payload must be empty",
                    ));
                }
                let encoded = node.identity.encode_success().map_err(io::Error::other)?;
                payload[..encoded.len()].copy_from_slice(&encoded);
                (encoded.len(), now_us.saturating_add(turnaround_us))
            }
            (IDENTITY_SERVICE_ID, GET_CAPABILITIES_MESSAGE_ID) => {
                if !request.payload().is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "capabilities request payload must be empty",
                    ));
                }
                let encoded = node
                    .capabilities
                    .encode_success()
                    .map_err(io::Error::other)?;
                payload[..encoded.len()].copy_from_slice(&encoded);
                (encoded.len(), now_us.saturating_add(turnaround_us))
            }
            (CONFIGURATION_SERVICE_ID, GET_CONFIG_MESSAGE_ID) => {
                if !request.payload().is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "get-configuration request payload must be empty",
                    ));
                }
                let encoded =
                    encode_get_config_response(node.configuration).map_err(io::Error::other)?;
                payload[..encoded.len()].copy_from_slice(&encoded);
                (encoded.len(), now_us.saturating_add(turnaround_us))
            }
            (CONFIGURATION_SERVICE_ID, VALIDATE_CONFIG_MESSAGE_ID) => {
                decode_configuration(request.payload()).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid validate-configuration request: {error}"),
                    )
                })?;
                let encoded = encode_validate_config_response(ConfigValidationResult {
                    code: ConfigValidationCode::Valid,
                    field: ConfigField::None,
                });
                payload[..encoded.len()].copy_from_slice(&encoded);
                (encoded.len(), now_us.saturating_add(turnaround_us))
            }
            (service, PULL_SAMPLES_MESSAGE_ID) if service == ServiceId::Measurement as u8 => {
                let pull = decode_pull_request(request.payload()).map_err(|error| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("invalid pull: {error}"))
                })?;
                let response = match node.link.pull(pull) {
                    SimulatedPull::Chunk { header, samples } => {
                        let first_device_time_us = node.device_time_us(header.first);
                        let last_sequence = header
                            .first
                            .checked_advance(u64::from(header.sample_count.saturating_sub(1)))
                            .expect("valid simulated chunk range");
                        PullResponse::SampleChunk(SampleChunk {
                            header,
                            config_revision: 1,
                            timing: TimingEvidence {
                                segment_id: 1,
                                first_device_time_us,
                                last_device_time_us: node.device_time_us(last_sequence),
                                max_fit_residual_us: 0,
                                quality_flags: TIMING_VALID,
                            },
                            samples,
                        })
                    }
                    SimulatedPull::NoData(state) => PullResponse::NoData(state),
                    SimulatedPull::RangeUnavailable(state) => PullResponse::RangeUnavailable(state),
                    SimulatedPull::StreamChanged => {
                        PullResponse::StreamChanged(node.link.state().boot_epoch)
                    }
                    SimulatedPull::InvalidRelease => {
                        PullResponse::InvalidRelease(node.link.state())
                    }
                };
                let payload_len =
                    encode_pull_response(&response, &mut payload).map_err(|error| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("cannot encode simulated response: {error}"),
                        )
                    })?;
                (payload_len, now_us.saturating_add(turnaround_us))
            }
            (service, GET_STREAM_STATE_MESSAGE_ID) if service == ServiceId::Measurement as u8 => {
                if !request.payload().is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "stream-state request payload must be empty",
                    ));
                }
                let encoded =
                    encode_stream_state_success(node.link.state()).map_err(io::Error::other)?;
                payload[..encoded.len()].copy_from_slice(&encoded);
                (encoded.len(), now_us.saturating_add(turnaround_us))
            }
            (TIME_SERVICE_ID, OBSERVE_CLOCK_MESSAGE_ID) => {
                validate_time_request(request.payload()).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid time request: {error}"),
                    )
                })?;
                let node_receive_host_us = now_us.saturating_add(node.time_profile.request_path_us);
                let node_send_host_us = node_receive_host_us.saturating_add(turnaround_us);
                let encoded = encode_time_success(TimeExchangeResponse {
                    boot_epoch: node.link.state().boot_epoch,
                    node_receive_us: node.clock.at(node_receive_host_us),
                    node_send_us: node.clock.at(node_send_host_us),
                })
                .map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("cannot encode simulated time response: {error}"),
                    )
                })?;
                payload[..encoded.len()].copy_from_slice(&encoded);
                (
                    encoded.len(),
                    node_send_host_us.saturating_add(node.time_profile.response_path_us),
                )
            }
            (LIFECYCLE_SERVICE_ID, message) => {
                let command =
                    decode_lifecycle_request(message, request.payload()).map_err(|error| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("invalid lifecycle request: {error}"),
                        )
                    })?;
                match command.operation {
                    RuntimeOperation::GetState => {}
                    RuntimeOperation::Start => {
                        node.runtime_state = RuntimeState::Recording;
                        node.observability = command.requested_profile;
                    }
                    RuntimeOperation::Stop => {
                        if node.runtime_state != RuntimeState::Maintenance {
                            node.runtime_state = RuntimeState::Idle;
                        }
                    }
                    RuntimeOperation::EnterMaintenance => {
                        if node.runtime_state != RuntimeState::Idle {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "simulation requires Idle before Maintenance",
                            ));
                        }
                        node.runtime_state = RuntimeState::Maintenance;
                        if let Some(boot_epoch) = node.reboot_on_maintenance.take() {
                            node.reboot(now_us, boot_epoch);
                        }
                    }
                    RuntimeOperation::ExitMaintenance => {
                        node.runtime_state = RuntimeState::Idle;
                    }
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "simulation does not support this lifecycle operation",
                        ));
                    }
                }
                let encoded = encode_lifecycle_response(RuntimeResult {
                    operation_id: command.operation_id,
                    code: RuntimeResultCode::Ok,
                    state: node.runtime_state,
                    observability: node.observability,
                    sensor_status: SensorStatusCode::Ok,
                })
                .map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("cannot encode simulated lifecycle response: {error}"),
                    )
                })?;
                payload[..encoded.len()].copy_from_slice(&encoded);
                (encoded.len(), now_us.saturating_add(turnaround_us))
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "simulation does not support this service/message",
                ));
            }
        };

        if node.responses_to_drop > 0 {
            node.responses_to_drop -= 1;
            node.stats.dropped_responses = node.stats.dropped_responses.saturating_add(1);
            return Ok(());
        }

        let response = Frame::new(
            FrameRole::Response,
            HOST_ADDRESS,
            request.destination,
            request.service,
            request.message,
            request.transaction_id,
            &payload[..payload_len],
        )
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let mut wire = [0; MAX_WIRE_FRAME_BYTES];
        let wire_len = response.encode(&mut wire);
        self.receive_bytes.extend(&wire[..wire_len]);
        let extra_delay_us = node.next_response_delay_us;
        node.next_response_delay_us = 0;
        if extra_delay_us > 0 {
            node.stats.delayed_responses = node.stats.delayed_responses.saturating_add(1);
        }
        self.response_ready_at_us = Some(
            response_started_at_us
                .saturating_add(extra_delay_us)
                .saturating_add(wire_time_us(wire_len, self.baud_rate)),
        );
        node.stats.responses = node.stats.responses.saturating_add(1);
        Ok(())
    }
}

impl Read for SimulatedRs485Bus {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.receive_bytes.is_empty() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "no response"));
        }
        if let Some(ready_at) = self.response_ready_at_us {
            let now = self.clock.now().get();
            if now < ready_at {
                if self.auto_advance_on_read {
                    self.clock.advance(ready_at - now);
                } else {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "no response"));
                }
            }
        }
        let count = output.len().min(self.receive_bytes.len());
        for byte in &mut output[..count] {
            *byte = self.receive_bytes.pop_front().expect("checked byte count");
        }
        if self.receive_bytes.is_empty() {
            self.response_ready_at_us = None;
        }
        Ok(count)
    }
}

impl Write for SimulatedRs485Bus {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        self.clock
            .advance(wire_time_us(input.len(), self.baud_rate));
        for &byte in input {
            if let Some(decoded) = self.request_decoder.push(byte) {
                let frame =
                    decoded.map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                self.handle_request(frame)?;
            }
        }
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn wire_time_us(byte_count: usize, baud_rate: u32) -> u64 {
    let bit_count = u128::from(u64::try_from(byte_count).unwrap_or(u64::MAX)).saturating_mul(10);
    let numerator = bit_count.saturating_mul(1_000_000);
    let denominator = u128::from(baud_rate);
    u64::try_from(numerator.div_ceil(denominator)).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    use sensor_core::{
        ClockModel, ClockModelError, ClockModelPolicy, ClockModelState, ClockObservation,
    };

    use crate::channel_agent::{ChannelAgent, ChannelEvent, ChannelPolicy};
    use crate::channel_endpoint::{ChannelEndpoint, ChannelEndpointError, ChannelEndpointEvent};
    use crate::channel_port::ChannelPort;
    use crate::transaction::DurationMicros;

    const NODE_ADDRESS: u8 = 7;

    fn duration(value: u64) -> DurationMicros {
        DurationMicros::new(value).unwrap()
    }

    fn endpoint(
        clock: SimulatedClock,
        bus: SimulatedRs485Bus,
        timeout_us: u64,
    ) -> ChannelEndpoint<SimulatedRs485Bus, SimulatedClock> {
        let agent = ChannelAgent::new(
            10,
            100,
            duration(10_000),
            ChannelPolicy {
                transaction_timeout: duration(timeout_us),
                retry_delay: duration(1_000),
                measurement_attempts_per_slot: 2,
                max_pending_work_items: 4,
            },
        )
        .unwrap();
        ChannelEndpoint::new(agent, ChannelPort::from_io(bus), clock)
    }

    fn next_observation(
        endpoint: &mut ChannelEndpoint<SimulatedRs485Bus, SimulatedClock>,
        clock: &SimulatedClock,
        epoch: BootEpoch,
    ) -> ClockObservation {
        endpoint
            .agent_mut()
            .submit_time_sync(NODE_ADDRESS, epoch)
            .unwrap();
        assert_eq!(
            endpoint.step().unwrap(),
            ChannelEndpointEvent::TransmissionComplete {
                node_address: NODE_ADDRESS
            }
        );
        for _ in 0..2_000 {
            match endpoint.step().unwrap() {
                ChannelEndpointEvent::Agent(ChannelEvent::TimeObservation {
                    node_address,
                    observation,
                }) => {
                    assert_eq!(node_address, NODE_ADDRESS);
                    return observation;
                }
                ChannelEndpointEvent::Idle => clock.advance(100),
                other => panic!("unexpected time simulation event: {other:?}"),
            }
        }
        panic!("time observation did not complete within the deterministic bound")
    }

    #[test]
    fn wire_time_models_eight_n_one_and_rounds_up() {
        assert_eq!(wire_time_us(42, 115_200), 3_646);
        assert_eq!(wire_time_us(516, 115_200), 44_792);
    }

    #[test]
    fn real_time_wire_path_tracks_bounded_drift_and_asymmetry() {
        let clock = SimulatedClock::new(0);
        let epoch = BootEpoch::new(11);
        let node = SimulatedWireNode::new(NODE_ADDRESS, epoch, 2_048, SampleRate::HZ_125);
        let mut bus = SimulatedRs485Bus::new(clock.clone(), vec![node]);
        let profile = SimulatedTimeProfile {
            node_origin_us: 2_000_000,
            drift_ppm: 65,
            request_path_us: 400,
            response_path_us: 120,
        };
        bus.configure_time(NODE_ADDRESS, profile);
        let mut endpoint = endpoint(clock.clone(), bus, 100_000);

        let first = next_observation(&mut endpoint, &clock, epoch);
        clock.advance(10_000_000);
        let second = next_observation(&mut endpoint, &clock, epoch);
        assert!(first.path_delay_us() > 0);
        assert!(second.path_delay_us() > 0);

        let mut model = ClockModel::new(
            epoch,
            ClockModelPolicy {
                max_observation_uncertainty_us: 10_000,
                minimum_anchor_separation_us: 5_000_000,
                maximum_drift_ppm: 100,
                maximum_holdover_us: 30_000_000,
            },
        )
        .unwrap();
        model.observe(first).unwrap();
        model.observe(second).unwrap();
        assert_eq!(model.state(), ClockModelState::Locked);

        let truth_host_us = clock.now().get();
        let truth_node_us = profile.node_origin_us
            + (u128::from(truth_host_us) * u128::from(1_000_000_u32 + 65) / 1_000_000) as u64;
        let estimate = model.estimate(truth_node_us).unwrap();
        assert!(
            estimate.host_monotonic_us.abs_diff(truth_host_us) <= estimate.uncertainty_us,
            "estimate={estimate:?} truth={truth_host_us}"
        );
    }

    #[test]
    fn excessive_response_jitter_is_preserved_and_rejected_by_model_policy() {
        let clock = SimulatedClock::new(0);
        let epoch = BootEpoch::new(12);
        let node = SimulatedWireNode::new(NODE_ADDRESS, epoch, 2_048, SampleRate::HZ_125);
        let mut bus = SimulatedRs485Bus::new(clock.clone(), vec![node]);
        bus.delay_next_response(NODE_ADDRESS, 20_000);
        let mut endpoint = endpoint(clock.clone(), bus, 100_000);
        let observation = next_observation(&mut endpoint, &clock, epoch);
        assert!(observation.uncertainty_us() > 5_000);

        let mut model = ClockModel::new(
            epoch,
            ClockModelPolicy {
                max_observation_uncertainty_us: 5_000,
                minimum_anchor_separation_us: 1_000_000,
                maximum_drift_ppm: 100,
                maximum_holdover_us: 30_000_000,
            },
        )
        .unwrap();
        assert_eq!(
            model.observe(observation),
            Err(ClockModelError::ObservationTooUncertain)
        );
        assert_eq!(model.state(), ClockModelState::Acquiring);
    }

    #[test]
    fn timed_out_time_response_is_late_and_never_becomes_an_observation() {
        let clock = SimulatedClock::new(0);
        let epoch = BootEpoch::new(13);
        let node = SimulatedWireNode::new(NODE_ADDRESS, epoch, 2_048, SampleRate::HZ_125);
        let mut bus = SimulatedRs485Bus::new(clock.clone(), vec![node]);
        bus.delay_next_response(NODE_ADDRESS, 3_000);
        let mut endpoint = endpoint(clock.clone(), bus, 5_000);
        endpoint
            .agent_mut()
            .submit_time_sync(NODE_ADDRESS, epoch)
            .unwrap();
        assert!(matches!(
            endpoint.step().unwrap(),
            ChannelEndpointEvent::TransmissionComplete { .. }
        ));
        clock.advance(5_001);
        assert_eq!(
            endpoint.step().unwrap(),
            ChannelEndpointEvent::Agent(ChannelEvent::TimeSyncTimedOut {
                node_address: NODE_ADDRESS
            })
        );
        clock.advance(3_000);
        assert_eq!(
            endpoint.step().unwrap(),
            ChannelEndpointEvent::Agent(ChannelEvent::LateResponse)
        );
    }

    #[test]
    fn node_restart_cannot_be_folded_into_the_previous_clock_epoch() {
        let clock = SimulatedClock::new(50_000);
        let old_epoch = BootEpoch::new(14);
        let new_epoch = BootEpoch::new(15);
        let node = SimulatedWireNode::new(NODE_ADDRESS, old_epoch, 2_048, SampleRate::HZ_125);
        let mut bus = SimulatedRs485Bus::new(clock.clone(), vec![node]);
        bus.reboot_node(NODE_ADDRESS, new_epoch);
        let mut endpoint = endpoint(clock.clone(), bus, 100_000);
        endpoint
            .agent_mut()
            .submit_time_sync(NODE_ADDRESS, old_epoch)
            .unwrap();
        assert!(matches!(
            endpoint.step().unwrap(),
            ChannelEndpointEvent::TransmissionComplete { .. }
        ));
        for _ in 0..2_000 {
            match endpoint.step() {
                Err(ChannelEndpointError::Agent(
                    crate::channel_agent::ChannelAgentError::TimeBootEpochMismatch,
                )) => return,
                Ok(ChannelEndpointEvent::Idle) => clock.advance(100),
                Ok(other) => panic!("unexpected restart simulation event: {other:?}"),
                Err(error) => panic!("unexpected restart simulation error: {error:?}"),
            }
        }
        panic!("restarted node response did not complete within the deterministic bound")
    }
}
