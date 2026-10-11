use core::fmt;

use sensor_core::{SampleRate, SequenceCursor};
use sensor_protocol::{ObservabilityProfile, BROADCAST_ADDRESS, HOST_ADDRESS};

use crate::channel_scheduler::{
    ChannelScheduler, ScheduleContext, SchedulerError, TrafficClass, WorkCandidate,
};
use crate::transaction::{DurationMicros, MonotonicMicros};

const NANO_HERTZ_MICROS_PER_SAMPLE: u128 = 1_000_000_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeScheduleConfig {
    pub node_address: u8,
    pub output_rate: SampleRate,
    pub journal_capacity_samples: u32,
    pub max_sample_chunk_samples: u16,
    /// Preferred fill level used to avoid repeated tiny or empty pulls.
    pub target_chunk_samples: u16,
    /// Conservative upper bound for one pull request/response slot.
    pub measurement_slot: DurationMicros,
    /// Maximum time between completed observations of this node.
    pub maximum_service_interval: DurationMicros,
    pub retry_base_delay: DurationMicros,
    pub retry_max_delay: DurationMicros,
    pub retry_attempt_limit: u8,
    pub offline_probe_interval: DurationMicros,
}

impl NodeScheduleConfig {
    pub fn validate(self) -> Result<Self, ChannelRuntimeError> {
        if self.node_address == HOST_ADDRESS || self.node_address == BROADCAST_ADDRESS {
            return Err(ChannelRuntimeError::InvalidNodeAddress(self.node_address));
        }
        if self.journal_capacity_samples == 0 {
            return Err(ChannelRuntimeError::ZeroJournalCapacity);
        }
        if self.max_sample_chunk_samples == 0
            || u32::from(self.max_sample_chunk_samples) > self.journal_capacity_samples
        {
            return Err(ChannelRuntimeError::InvalidChunkSize);
        }
        if self.target_chunk_samples == 0
            || self.target_chunk_samples > self.max_sample_chunk_samples
        {
            return Err(ChannelRuntimeError::InvalidTargetChunkSize);
        }
        if self.measurement_slot.get() > self.maximum_service_interval.get() {
            return Err(ChannelRuntimeError::SlotExceedsServiceInterval);
        }
        if self.retry_base_delay.get() > self.retry_max_delay.get() {
            return Err(ChannelRuntimeError::InvalidRetryDelays);
        }
        if self.retry_attempt_limit == 0 {
            return Err(ChannelRuntimeError::ZeroRetryLimit);
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeasurementProgress {
    /// First logical item that still has to be delivered to durable storage.
    pub received: SequenceCursor,
    /// Exclusive producer cursor observed in the validated node response.
    pub produced: SequenceCursor,
}

impl MeasurementProgress {
    pub fn validate(self) -> Result<Self, ChannelRuntimeError> {
        if self.received > self.produced {
            Err(ChannelRuntimeError::ReceivedAheadOfProduced)
        } else {
            Ok(self)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeasurementDispatch {
    pub work_id: u64,
    pub node_address: u8,
    pub max_samples: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeSyncScheduleConfig {
    pub interval: DurationMicros,
    pub estimated_slot: DurationMicros,
    pub retry_delay: DurationMicros,
}

impl TimeSyncScheduleConfig {
    pub fn new(
        interval: DurationMicros,
        estimated_slot: DurationMicros,
        retry_delay: DurationMicros,
    ) -> Result<Self, ChannelRuntimeError> {
        Self {
            interval,
            estimated_slot,
            retry_delay,
        }
        .validate()
    }

    pub fn validate(self) -> Result<Self, ChannelRuntimeError> {
        if self.estimated_slot.get() > self.interval.get() {
            return Err(ChannelRuntimeError::TimeSyncSlotExceedsInterval);
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeSyncDispatch {
    pub work_id: u64,
    pub node_address: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelDispatch {
    Measurement(MeasurementDispatch),
    TimeSync(TimeSyncDispatch),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TimeSyncScheduleState {
    node_address: u8,
    config: TimeSyncScheduleConfig,
    deadline: MonotonicMicros,
    ready_at: MonotonicMicros,
    queued_at: MonotonicMicros,
    work_id: u64,
    in_flight: bool,
}

impl TimeSyncScheduleState {
    const fn candidate(self) -> Option<WorkCandidate> {
        if self.in_flight {
            return None;
        }
        Some(WorkCandidate {
            work_id: self.work_id,
            node_address: self.node_address,
            class: TrafficClass::TimeSync,
            ready_at: self.ready_at,
            deadline: Some(self.deadline),
            queued_at: self.queued_at,
            time_to_overflow_us: None,
            estimated_slot: self.config.estimated_slot,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeScheduleSnapshot {
    pub node_address: u8,
    pub received: SequenceCursor,
    pub observed_produced: SequenceCursor,
    pub time_to_overflow_us: u64,
    pub service_deadline: MonotonicMicros,
    pub ready_at: MonotonicMicros,
    pub consecutive_failures: u8,
    pub offline: bool,
    pub in_flight: bool,
    pub recovery_required: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NodeScheduleState {
    config: NodeScheduleConfig,
    received: SequenceCursor,
    observed_produced: SequenceCursor,
    observed_at: MonotonicMicros,
    service_deadline: MonotonicMicros,
    ready_at: MonotonicMicros,
    queued_at: MonotonicMicros,
    consecutive_failures: u8,
    work_id: u64,
    in_flight: bool,
    recovery_required: bool,
}

impl NodeScheduleState {
    fn candidate(self, now: MonotonicMicros) -> Result<Option<WorkCandidate>, ChannelRuntimeError> {
        if self.in_flight || self.recovery_required {
            return Ok(None);
        }
        Ok(Some(WorkCandidate {
            work_id: self.work_id,
            node_address: self.config.node_address,
            class: TrafficClass::Measurement,
            ready_at: self.ready_at,
            deadline: Some(self.service_deadline),
            queued_at: self.queued_at,
            time_to_overflow_us: Some(self.time_to_overflow_us(now)?),
            estimated_slot: self.config.measurement_slot,
        }))
    }

    fn time_to_overflow_us(self, now: MonotonicMicros) -> Result<u64, ChannelRuntimeError> {
        let elapsed_us = now
            .get()
            .checked_sub(self.observed_at.get())
            .ok_or(ChannelRuntimeError::ClockRegressed)?;
        let produced_since_observation = div_ceil(
            u128::from(self.config.output_rate.nanohertz()) * u128::from(elapsed_us),
            NANO_HERTZ_MICROS_PER_SAMPLE,
        );
        let estimated_produced =
            u128::from(self.observed_produced.get()).saturating_add(produced_since_observation);
        let backlog = estimated_produced.saturating_sub(u128::from(self.received.get()));
        let capacity = u128::from(self.config.journal_capacity_samples);
        let free_samples = capacity.saturating_sub(backlog.min(capacity));
        let micros = free_samples.saturating_mul(NANO_HERTZ_MICROS_PER_SAMPLE)
            / u128::from(self.config.output_rate.nanohertz());
        Ok(u64::try_from(micros).unwrap_or(u64::MAX))
    }

    fn snapshot(self, now: MonotonicMicros) -> Result<NodeScheduleSnapshot, ChannelRuntimeError> {
        Ok(NodeScheduleSnapshot {
            node_address: self.config.node_address,
            received: self.received,
            observed_produced: self.observed_produced,
            time_to_overflow_us: self.time_to_overflow_us(now)?,
            service_deadline: self.service_deadline,
            ready_at: self.ready_at,
            consecutive_failures: self.consecutive_failures,
            offline: self.consecutive_failures > self.config.retry_attempt_limit,
            in_flight: self.in_flight,
            recovery_required: self.recovery_required,
        })
    }
}

/// Owns scheduling state for all nodes attached to one channel.
///
/// It does not encode frames, access the serial port, persist samples or infer
/// clock models. A caller turns the selected `ChannelDispatch` into work for
/// the matching service, executes it through `ChannelAgent`, and returns only
/// validated completion here.
pub struct ChannelRuntime {
    scheduler: ChannelScheduler,
    observability: ObservabilityProfile,
    max_nodes: usize,
    next_work_id: u64,
    nodes: Vec<NodeScheduleState>,
    time_sync: Vec<TimeSyncScheduleState>,
    candidates: Vec<WorkCandidate>,
    active_dispatch: Option<ChannelDispatch>,
}

impl ChannelRuntime {
    pub fn new(
        scheduler: ChannelScheduler,
        observability: ObservabilityProfile,
        max_nodes: usize,
        work_id_seed: u64,
    ) -> Result<Self, ChannelRuntimeError> {
        if max_nodes == 0 {
            return Err(ChannelRuntimeError::ZeroNodeCapacity);
        }
        Ok(Self {
            scheduler,
            observability,
            max_nodes,
            next_work_id: work_id_seed.max(1),
            nodes: Vec::with_capacity(max_nodes),
            time_sync: Vec::with_capacity(max_nodes),
            candidates: Vec::with_capacity(max_nodes.saturating_mul(2)),
            active_dispatch: None,
        })
    }

    pub fn register_node(
        &mut self,
        config: NodeScheduleConfig,
        recovered_cursor: SequenceCursor,
        now: MonotonicMicros,
    ) -> Result<(), ChannelRuntimeError> {
        let config = config.validate()?;
        if self.nodes.len() >= self.max_nodes {
            return Err(ChannelRuntimeError::NodeCapacityReached);
        }
        if self
            .nodes
            .iter()
            .any(|node| node.config.node_address == config.node_address)
        {
            return Err(ChannelRuntimeError::DuplicateNode(config.node_address));
        }
        let work_id = self.allocate_work_id()?;
        self.nodes.push(NodeScheduleState {
            config,
            received: recovered_cursor,
            observed_produced: recovered_cursor,
            observed_at: now,
            service_deadline: now,
            ready_at: now,
            queued_at: now,
            consecutive_failures: 0,
            work_id,
            in_flight: false,
            recovery_required: false,
        });
        Ok(())
    }

    pub const fn observability(&self) -> ObservabilityProfile {
        self.observability
    }

    pub fn set_observability(&mut self, observability: ObservabilityProfile) {
        self.observability = observability;
    }

    pub fn configure_time_sync(
        &mut self,
        node_address: u8,
        config: TimeSyncScheduleConfig,
        first_deadline: MonotonicMicros,
    ) -> Result<(), ChannelRuntimeError> {
        let config = config.validate()?;
        let node = self
            .nodes
            .iter()
            .find(|node| node.config.node_address == node_address)
            .ok_or(ChannelRuntimeError::UnknownNode(node_address))?;
        if node.recovery_required {
            return Err(ChannelRuntimeError::RecoveryRequired(node_address));
        }
        if self
            .time_sync
            .iter()
            .any(|state| state.node_address == node_address)
        {
            return Err(ChannelRuntimeError::DuplicateTimeSync(node_address));
        }
        let work_id = self.allocate_work_id()?;
        let first_ready_at = MonotonicMicros::new(
            first_deadline
                .get()
                .saturating_sub(config.estimated_slot.get()),
        );
        self.time_sync.push(TimeSyncScheduleState {
            node_address,
            config,
            deadline: first_deadline,
            ready_at: first_ready_at,
            queued_at: first_ready_at,
            work_id,
            in_flight: false,
        });
        Ok(())
    }

    pub fn select_work(
        &mut self,
        now: MonotonicMicros,
    ) -> Result<Option<ChannelDispatch>, ChannelRuntimeError> {
        if self.active_dispatch.is_some() {
            return Ok(None);
        }
        self.candidates.clear();
        for node in self.nodes.iter().copied() {
            if let Some(candidate) = node.candidate(now)? {
                self.candidates.push(candidate);
            }
        }
        for state in self.time_sync.iter().copied() {
            let node_available = self.nodes.iter().any(|node| {
                node.config.node_address == state.node_address
                    && !node.recovery_required
                    && !node.in_flight
            });
            if node_available {
                if let Some(candidate) = state.candidate() {
                    self.candidates.push(candidate);
                }
            }
        }
        let Some(selected) = self.scheduler.select(
            ScheduleContext {
                now,
                observability: self.observability,
                diagnostic_budget_us: 0,
            },
            &self.candidates,
        )?
        else {
            return Ok(None);
        };
        let dispatch = match selected.class {
            TrafficClass::Measurement => {
                let node = self
                    .nodes
                    .iter_mut()
                    .find(|node| node.work_id == selected.work_id)
                    .ok_or(ChannelRuntimeError::InvariantViolation)?;
                node.in_flight = true;
                ChannelDispatch::Measurement(MeasurementDispatch {
                    work_id: node.work_id,
                    node_address: node.config.node_address,
                    max_samples: node.config.max_sample_chunk_samples,
                })
            }
            TrafficClass::TimeSync => {
                let state = self
                    .time_sync
                    .iter_mut()
                    .find(|state| state.work_id == selected.work_id)
                    .ok_or(ChannelRuntimeError::InvariantViolation)?;
                state.in_flight = true;
                ChannelDispatch::TimeSync(TimeSyncDispatch {
                    work_id: state.work_id,
                    node_address: state.node_address,
                })
            }
            _ => return Err(ChannelRuntimeError::InvariantViolation),
        };
        self.active_dispatch = Some(dispatch);
        Ok(Some(dispatch))
    }

    pub fn select_measurement(
        &mut self,
        now: MonotonicMicros,
    ) -> Result<Option<MeasurementDispatch>, ChannelRuntimeError> {
        if !self.time_sync.is_empty() {
            return Err(ChannelRuntimeError::UnifiedDispatchRequired);
        }
        if self.active_dispatch.is_some() {
            return Ok(None);
        }
        self.candidates.clear();
        for node in self.nodes.iter().copied() {
            if let Some(candidate) = node.candidate(now)? {
                self.candidates.push(candidate);
            }
        }
        let Some(selected) = self.scheduler.select(
            ScheduleContext {
                now,
                observability: self.observability,
                diagnostic_budget_us: 0,
            },
            &self.candidates,
        )?
        else {
            return Ok(None);
        };
        let node = self
            .nodes
            .iter_mut()
            .find(|node| node.work_id == selected.work_id)
            .ok_or(ChannelRuntimeError::InvariantViolation)?;
        if node.in_flight {
            return Err(ChannelRuntimeError::InvariantViolation);
        }
        node.in_flight = true;
        let dispatch = MeasurementDispatch {
            work_id: node.work_id,
            node_address: node.config.node_address,
            max_samples: node.config.max_sample_chunk_samples,
        };
        self.active_dispatch = Some(ChannelDispatch::Measurement(dispatch));
        Ok(Some(dispatch))
    }

    /// Selects one specific Measurement during controlled shutdown. Normal
    /// fill and service deadlines are bypassed because production is already
    /// frozen, while retry backoff after a failed attempt remains enforced.
    pub fn select_shutdown_measurement(
        &mut self,
        node_address: u8,
        now: MonotonicMicros,
    ) -> Result<Option<MeasurementDispatch>, ChannelRuntimeError> {
        if self.active_dispatch.is_some() {
            return Ok(None);
        }
        let node = self
            .nodes
            .iter_mut()
            .find(|node| node.config.node_address == node_address)
            .ok_or(ChannelRuntimeError::UnknownNode(node_address))?;
        if node.recovery_required {
            return Err(ChannelRuntimeError::RecoveryRequired(node_address));
        }
        if node.in_flight {
            return Ok(None);
        }
        if node.consecutive_failures > 0 && node.ready_at > now {
            return Ok(None);
        }
        node.in_flight = true;
        let dispatch = MeasurementDispatch {
            work_id: node.work_id,
            node_address,
            max_samples: node.config.max_sample_chunk_samples,
        };
        self.active_dispatch = Some(ChannelDispatch::Measurement(dispatch));
        Ok(Some(dispatch))
    }

    pub fn complete_measurement(
        &mut self,
        dispatch: MeasurementDispatch,
        progress: MeasurementProgress,
        now: MonotonicMicros,
    ) -> Result<(), ChannelRuntimeError> {
        let progress = progress.validate()?;
        let node_index = self.active_node_index(dispatch)?;
        let node = self.nodes[node_index];
        if progress.received < node.received || progress.produced < node.observed_produced {
            let node = &mut self.nodes[node_index];
            node.in_flight = false;
            node.recovery_required = true;
            self.active_dispatch = None;
            return Err(ChannelRuntimeError::StreamRegressed);
        }
        let next_deadline = now
            .checked_add(node.config.maximum_service_interval)
            .ok_or(ChannelRuntimeError::TimeOverflow)?;
        let next_work_id = self.allocate_work_id()?;
        let node = &mut self.nodes[node_index];
        node.received = progress.received;
        node.observed_produced = progress.produced;
        node.observed_at = now;
        node.service_deadline = next_deadline;
        node.ready_at = next_measurement_ready_at(node.config, progress, now)?;
        node.queued_at = now;
        node.consecutive_failures = 0;
        node.work_id = next_work_id;
        node.in_flight = false;
        self.active_dispatch = None;
        Ok(())
    }

    /// Releases the physical channel while keeping this node in flight until
    /// its storage completion arrives.
    pub fn park_measurement(
        &mut self,
        dispatch: MeasurementDispatch,
    ) -> Result<(), ChannelRuntimeError> {
        self.active_node_index(dispatch)?;
        self.active_dispatch = None;
        Ok(())
    }

    pub fn complete_parked_measurement(
        &mut self,
        dispatch: MeasurementDispatch,
        progress: MeasurementProgress,
        now: MonotonicMicros,
    ) -> Result<(), ChannelRuntimeError> {
        if self.active_dispatch == Some(ChannelDispatch::Measurement(dispatch)) {
            return Err(ChannelRuntimeError::InvariantViolation);
        }
        let progress = progress.validate()?;
        let node_index = self.in_flight_node_index(dispatch)?;
        let node = self.nodes[node_index];
        if progress.received < node.received || progress.produced < node.observed_produced {
            let node = &mut self.nodes[node_index];
            node.in_flight = false;
            node.recovery_required = true;
            return Err(ChannelRuntimeError::StreamRegressed);
        }
        let next_deadline = now
            .checked_add(node.config.maximum_service_interval)
            .ok_or(ChannelRuntimeError::TimeOverflow)?;
        let next_work_id = self.allocate_work_id()?;
        let node = &mut self.nodes[node_index];
        node.received = progress.received;
        node.observed_produced = progress.produced;
        node.observed_at = now;
        node.service_deadline = next_deadline;
        node.ready_at = next_measurement_ready_at(node.config, progress, now)?;
        node.queued_at = now;
        node.consecutive_failures = 0;
        node.work_id = next_work_id;
        node.in_flight = false;
        Ok(())
    }

    /// Removes a node from production rotation after `NodeSession` reports a
    /// stream change or another condition requiring authoritative recovery.
    pub fn require_recovery(
        &mut self,
        dispatch: MeasurementDispatch,
    ) -> Result<(), ChannelRuntimeError> {
        let node_index = self.active_node_index(dispatch)?;
        let node = &mut self.nodes[node_index];
        node.in_flight = false;
        node.recovery_required = true;
        self.active_dispatch = None;
        Ok(())
    }

    /// Re-enters rotation only after the caller rebuilt `NodeSession` from a
    /// validated boot epoch and durable cursor.
    pub fn recover_node(
        &mut self,
        node_address: u8,
        recovered_cursor: SequenceCursor,
        now: MonotonicMicros,
    ) -> Result<(), ChannelRuntimeError> {
        let node_index = self
            .nodes
            .iter()
            .position(|node| node.config.node_address == node_address)
            .ok_or(ChannelRuntimeError::UnknownNode(node_address))?;
        if !self.nodes[node_index].recovery_required {
            return Err(ChannelRuntimeError::RecoveryNotRequired(node_address));
        }
        let work_id = self.allocate_work_id()?;
        let node = &mut self.nodes[node_index];
        node.received = recovered_cursor;
        node.observed_produced = recovered_cursor;
        node.observed_at = now;
        node.service_deadline = now;
        node.ready_at = now;
        node.queued_at = now;
        node.consecutive_failures = 0;
        node.work_id = work_id;
        node.in_flight = false;
        node.recovery_required = false;
        Ok(())
    }

    pub fn fail_measurement(
        &mut self,
        dispatch: MeasurementDispatch,
        now: MonotonicMicros,
    ) -> Result<(), ChannelRuntimeError> {
        let node_index = self.active_node_index(dispatch)?;
        let node = &mut self.nodes[node_index];
        node.consecutive_failures = node.consecutive_failures.saturating_add(1);
        let delay = if node.consecutive_failures > node.config.retry_attempt_limit {
            node.config.offline_probe_interval
        } else {
            exponential_delay(
                node.config.retry_base_delay,
                node.config.retry_max_delay,
                node.consecutive_failures,
            )
        };
        node.ready_at = now
            .checked_add(delay)
            .ok_or(ChannelRuntimeError::TimeOverflow)?;
        node.in_flight = false;
        self.active_dispatch = None;
        Ok(())
    }

    /// Releases a dispatch which could not be handed to the transaction
    /// executor. No wire attempt occurred, so retry counters are unchanged.
    pub fn cancel_measurement(
        &mut self,
        dispatch: MeasurementDispatch,
    ) -> Result<(), ChannelRuntimeError> {
        let node_index = self.active_node_index(dispatch)?;
        self.nodes[node_index].in_flight = false;
        self.active_dispatch = None;
        Ok(())
    }

    pub fn complete_time_sync(
        &mut self,
        dispatch: TimeSyncDispatch,
        now: MonotonicMicros,
    ) -> Result<(), ChannelRuntimeError> {
        let index = self.active_time_sync_index(dispatch)?;
        let next_deadline = now
            .checked_add(self.time_sync[index].config.interval)
            .ok_or(ChannelRuntimeError::TimeOverflow)?;
        let next_work_id = self.allocate_work_id()?;
        let state = &mut self.time_sync[index];
        state.deadline = next_deadline;
        state.ready_at = now;
        state.queued_at = now;
        state.work_id = next_work_id;
        state.in_flight = false;
        self.active_dispatch = None;
        Ok(())
    }

    pub fn fail_time_sync(
        &mut self,
        dispatch: TimeSyncDispatch,
        now: MonotonicMicros,
    ) -> Result<(), ChannelRuntimeError> {
        let index = self.active_time_sync_index(dispatch)?;
        let ready_at = now
            .checked_add(self.time_sync[index].config.retry_delay)
            .ok_or(ChannelRuntimeError::TimeOverflow)?;
        let state = &mut self.time_sync[index];
        state.ready_at = ready_at;
        state.queued_at = now;
        state.in_flight = false;
        self.active_dispatch = None;
        Ok(())
    }

    pub fn cancel_time_sync(
        &mut self,
        dispatch: TimeSyncDispatch,
    ) -> Result<(), ChannelRuntimeError> {
        let index = self.active_time_sync_index(dispatch)?;
        self.time_sync[index].in_flight = false;
        self.active_dispatch = None;
        Ok(())
    }

    pub fn snapshot(
        &self,
        node_address: u8,
        now: MonotonicMicros,
    ) -> Result<NodeScheduleSnapshot, ChannelRuntimeError> {
        self.nodes
            .iter()
            .find(|node| node.config.node_address == node_address)
            .copied()
            .ok_or(ChannelRuntimeError::UnknownNode(node_address))?
            .snapshot(now)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub const fn node_capacity(&self) -> usize {
        self.max_nodes
    }

    fn active_node_index(
        &self,
        dispatch: MeasurementDispatch,
    ) -> Result<usize, ChannelRuntimeError> {
        if self.active_dispatch != Some(ChannelDispatch::Measurement(dispatch)) {
            return Err(ChannelRuntimeError::StaleDispatch);
        }
        self.in_flight_node_index(dispatch)
    }

    fn in_flight_node_index(
        &self,
        dispatch: MeasurementDispatch,
    ) -> Result<usize, ChannelRuntimeError> {
        let index = self
            .nodes
            .iter()
            .position(|node| node.config.node_address == dispatch.node_address)
            .ok_or(ChannelRuntimeError::UnknownNode(dispatch.node_address))?;
        let node = self.nodes[index];
        if !node.in_flight || node.work_id != dispatch.work_id {
            return Err(ChannelRuntimeError::StaleDispatch);
        }
        if node.config.max_sample_chunk_samples != dispatch.max_samples {
            return Err(ChannelRuntimeError::StaleDispatch);
        }
        Ok(index)
    }

    fn active_time_sync_index(
        &self,
        dispatch: TimeSyncDispatch,
    ) -> Result<usize, ChannelRuntimeError> {
        if self.active_dispatch != Some(ChannelDispatch::TimeSync(dispatch)) {
            return Err(ChannelRuntimeError::StaleDispatch);
        }
        let index = self
            .time_sync
            .iter()
            .position(|state| state.node_address == dispatch.node_address)
            .ok_or(ChannelRuntimeError::UnknownNode(dispatch.node_address))?;
        let state = self.time_sync[index];
        if !state.in_flight || state.work_id != dispatch.work_id {
            return Err(ChannelRuntimeError::StaleDispatch);
        }
        Ok(index)
    }

    fn allocate_work_id(&mut self) -> Result<u64, ChannelRuntimeError> {
        for _ in 0..=self.max_nodes {
            let candidate = self.next_work_id.max(1);
            self.next_work_id = candidate.wrapping_add(1).max(1);
            if !self.nodes.iter().any(|node| node.work_id == candidate)
                && !self
                    .time_sync
                    .iter()
                    .any(|state| state.work_id == candidate)
            {
                return Ok(candidate);
            }
        }
        Err(ChannelRuntimeError::WorkIdExhausted)
    }
}

fn exponential_delay(
    base: DurationMicros,
    maximum: DurationMicros,
    failure_count: u8,
) -> DurationMicros {
    let mut delay = base.get();
    for _ in 1..failure_count {
        delay = delay.saturating_mul(2).min(maximum.get());
    }
    DurationMicros::new(delay.min(maximum.get())).expect("validated delays are non-zero")
}

fn next_measurement_ready_at(
    config: NodeScheduleConfig,
    progress: MeasurementProgress,
    now: MonotonicMicros,
) -> Result<MonotonicMicros, ChannelRuntimeError> {
    let backlog = progress.produced.get() - progress.received.get();
    let missing_samples = u64::from(config.target_chunk_samples).saturating_sub(backlog);
    let fill_time_us = u128::from(missing_samples).saturating_mul(NANO_HERTZ_MICROS_PER_SAMPLE)
        / u128::from(config.output_rate.nanohertz());
    let latest_safe_start_us = config
        .maximum_service_interval
        .get()
        .saturating_sub(config.measurement_slot.get());
    let wait_us = u64::try_from(fill_time_us)
        .unwrap_or(u64::MAX)
        .min(latest_safe_start_us);
    now.get()
        .checked_add(wait_us)
        .map(MonotonicMicros::new)
        .ok_or(ChannelRuntimeError::TimeOverflow)
}

const fn div_ceil(numerator: u128, denominator: u128) -> u128 {
    numerator / denominator + if numerator % denominator == 0 { 0 } else { 1 }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelRuntimeError {
    ZeroNodeCapacity,
    NodeCapacityReached,
    InvalidNodeAddress(u8),
    DuplicateNode(u8),
    DuplicateTimeSync(u8),
    UnknownNode(u8),
    ZeroJournalCapacity,
    InvalidChunkSize,
    InvalidTargetChunkSize,
    SlotExceedsServiceInterval,
    InvalidRetryDelays,
    ZeroRetryLimit,
    TimeSyncSlotExceedsInterval,
    ReceivedAheadOfProduced,
    StreamRegressed,
    RecoveryNotRequired(u8),
    RecoveryRequired(u8),
    ClockRegressed,
    TimeOverflow,
    StaleDispatch,
    WorkIdExhausted,
    InvariantViolation,
    UnifiedDispatchRequired,
    Scheduler(SchedulerError),
}

impl From<SchedulerError> for ChannelRuntimeError {
    fn from(value: SchedulerError) -> Self {
        Self::Scheduler(value)
    }
}

impl fmt::Display for ChannelRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "channel runtime error: {self:?}")
    }
}

impl std::error::Error for ChannelRuntimeError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_scheduler::ChannelSchedulerPolicy;

    fn duration(value: u64) -> DurationMicros {
        DurationMicros::new(value).unwrap()
    }

    fn config(node_address: u8) -> NodeScheduleConfig {
        NodeScheduleConfig {
            node_address,
            output_rate: SampleRate::HZ_125,
            journal_capacity_samples: 2_048,
            max_sample_chunk_samples: 48,
            target_chunk_samples: 32,
            measurement_slot: duration(50_000),
            maximum_service_interval: duration(500_000),
            retry_base_delay: duration(10_000),
            retry_max_delay: duration(80_000),
            retry_attempt_limit: 3,
            offline_probe_interval: duration(2_000_000),
        }
    }

    fn time_config() -> TimeSyncScheduleConfig {
        TimeSyncScheduleConfig {
            interval: duration(100_000),
            estimated_slot: duration(10_000),
            retry_delay: duration(20_000),
        }
    }

    fn runtime(max_nodes: usize) -> ChannelRuntime {
        ChannelRuntime::new(
            ChannelScheduler::new(ChannelSchedulerPolicy {
                overflow_guard: duration(250_000),
                max_slot: duration(50_000),
            }),
            ObservabilityProfile::Production,
            max_nodes,
            10,
        )
        .unwrap()
    }

    #[test]
    fn rejects_chunk_target_and_slot_that_break_scheduler_contract() {
        let mut invalid_target = config(1);
        invalid_target.target_chunk_samples = invalid_target.max_sample_chunk_samples + 1;
        assert_eq!(
            invalid_target.validate(),
            Err(ChannelRuntimeError::InvalidTargetChunkSize)
        );

        let mut invalid_slot = config(1);
        invalid_slot.measurement_slot = duration(500_001);
        assert_eq!(
            invalid_slot.validate(),
            Err(ChannelRuntimeError::SlotExceedsServiceInterval)
        );
    }

    fn complete_empty(runtime: &mut ChannelRuntime, dispatch: MeasurementDispatch, now: u64) {
        runtime
            .complete_measurement(
                dispatch,
                MeasurementProgress {
                    received: SequenceCursor::ORIGIN,
                    produced: SequenceCursor::ORIGIN,
                },
                MonotonicMicros::new(now),
            )
            .unwrap();
    }

    #[test]
    fn rotates_equal_nodes_without_parallel_dispatch() {
        let mut runtime = runtime(2);
        runtime
            .register_node(config(1), SequenceCursor::ORIGIN, MonotonicMicros::new(0))
            .unwrap();
        runtime
            .register_node(config(2), SequenceCursor::ORIGIN, MonotonicMicros::new(0))
            .unwrap();

        let first = runtime
            .select_measurement(MonotonicMicros::new(0))
            .unwrap()
            .unwrap();
        assert_eq!(first.node_address, 1);
        assert!(runtime
            .select_measurement(MonotonicMicros::new(0))
            .unwrap()
            .is_none());

        complete_empty(&mut runtime, first, 1);
        let second = runtime
            .select_measurement(MonotonicMicros::new(1))
            .unwrap()
            .unwrap();
        assert_eq!(second.node_address, 2);
        complete_empty(&mut runtime, second, 2);
        assert_eq!(
            runtime
                .select_measurement(MonotonicMicros::new(500_000))
                .unwrap()
                .unwrap()
                .node_address,
            1
        );
    }

    #[test]
    fn backlog_aging_reduces_time_to_overflow_without_float_math() {
        let mut runtime = runtime(1);
        runtime
            .register_node(config(1), SequenceCursor::ORIGIN, MonotonicMicros::new(0))
            .unwrap();
        assert_eq!(
            runtime
                .snapshot(1, MonotonicMicros::new(0))
                .unwrap()
                .time_to_overflow_us,
            16_384_000
        );
        assert_eq!(
            runtime
                .snapshot(1, MonotonicMicros::new(1_000_000))
                .unwrap()
                .time_to_overflow_us,
            15_384_000
        );
    }

    #[test]
    fn empty_pull_waits_for_target_fill_without_missing_service_deadline() {
        let mut runtime = runtime(1);
        runtime
            .register_node(config(1), SequenceCursor::ORIGIN, MonotonicMicros::new(0))
            .unwrap();
        let dispatch = runtime
            .select_measurement(MonotonicMicros::new(0))
            .unwrap()
            .unwrap();
        complete_empty(&mut runtime, dispatch, 1);

        let snapshot = runtime.snapshot(1, MonotonicMicros::new(1)).unwrap();
        assert_eq!(snapshot.ready_at, MonotonicMicros::new(256_001));
        assert!(runtime
            .select_measurement(MonotonicMicros::new(256_000))
            .unwrap()
            .is_none());
        assert!(runtime
            .select_measurement(MonotonicMicros::new(256_001))
            .unwrap()
            .is_some());
    }

    #[test]
    fn retry_backoff_releases_scheduler_for_other_nodes() {
        let mut runtime = runtime(2);
        runtime
            .register_node(config(1), SequenceCursor::ORIGIN, MonotonicMicros::new(0))
            .unwrap();
        runtime
            .register_node(config(2), SequenceCursor::ORIGIN, MonotonicMicros::new(0))
            .unwrap();
        let first = runtime
            .select_measurement(MonotonicMicros::new(0))
            .unwrap()
            .unwrap();
        runtime
            .fail_measurement(first, MonotonicMicros::new(0))
            .unwrap();
        let next = runtime
            .select_measurement(MonotonicMicros::new(0))
            .unwrap()
            .unwrap();
        assert_eq!(next.node_address, 2);
        assert_eq!(
            runtime
                .snapshot(1, MonotonicMicros::new(0))
                .unwrap()
                .ready_at,
            MonotonicMicros::new(10_000)
        );
    }

    #[test]
    fn repeated_failures_enter_slow_offline_probe() {
        let mut runtime = runtime(1);
        runtime
            .register_node(config(1), SequenceCursor::ORIGIN, MonotonicMicros::new(0))
            .unwrap();
        let attempt_times = [0, 10_000, 30_000, 70_000];
        for now in attempt_times {
            let dispatch = runtime
                .select_measurement(MonotonicMicros::new(now))
                .unwrap()
                .unwrap();
            runtime
                .fail_measurement(dispatch, MonotonicMicros::new(now))
                .unwrap();
        }
        let snapshot = runtime.snapshot(1, MonotonicMicros::new(70_000)).unwrap();
        assert!(snapshot.offline);
        assert_eq!(snapshot.ready_at, MonotonicMicros::new(2_070_000));
    }

    #[test]
    fn stale_completion_cannot_modify_new_work() {
        let mut runtime = runtime(1);
        runtime
            .register_node(config(1), SequenceCursor::ORIGIN, MonotonicMicros::new(0))
            .unwrap();
        let first = runtime
            .select_measurement(MonotonicMicros::new(0))
            .unwrap()
            .unwrap();
        complete_empty(&mut runtime, first, 1);
        let second = runtime
            .select_measurement(MonotonicMicros::new(500_001))
            .unwrap()
            .unwrap();
        assert_ne!(first.work_id, second.work_id);
        assert_eq!(
            runtime.complete_measurement(
                first,
                MeasurementProgress {
                    received: SequenceCursor::ORIGIN,
                    produced: SequenceCursor::ORIGIN,
                },
                MonotonicMicros::new(500_001),
            ),
            Err(ChannelRuntimeError::StaleDispatch)
        );
    }

    #[test]
    fn regressed_stream_is_quarantined_until_explicit_recovery() {
        let mut runtime = runtime(1);
        runtime
            .register_node(config(1), SequenceCursor::new(100), MonotonicMicros::new(0))
            .unwrap();
        let dispatch = runtime
            .select_measurement(MonotonicMicros::new(0))
            .unwrap()
            .unwrap();
        assert_eq!(
            runtime.complete_measurement(
                dispatch,
                MeasurementProgress {
                    received: SequenceCursor::new(1),
                    produced: SequenceCursor::new(1),
                },
                MonotonicMicros::new(1),
            ),
            Err(ChannelRuntimeError::StreamRegressed)
        );
        assert!(
            runtime
                .snapshot(1, MonotonicMicros::new(1))
                .unwrap()
                .recovery_required
        );
        assert!(runtime
            .select_measurement(MonotonicMicros::new(1))
            .unwrap()
            .is_none());

        runtime
            .recover_node(1, SequenceCursor::ORIGIN, MonotonicMicros::new(2))
            .unwrap();
        assert!(runtime
            .select_measurement(MonotonicMicros::new(2))
            .unwrap()
            .is_some());
    }

    #[test]
    fn time_sync_uses_the_same_deadline_scheduler_as_measurement() {
        let mut runtime = runtime(1);
        runtime
            .register_node(config(1), SequenceCursor::ORIGIN, MonotonicMicros::new(0))
            .unwrap();
        runtime
            .configure_time_sync(1, time_config(), MonotonicMicros::new(0))
            .unwrap();

        let ChannelDispatch::Measurement(measurement) = runtime
            .select_work(MonotonicMicros::new(0))
            .unwrap()
            .unwrap()
        else {
            panic!("measurement wins equal initial deadlines");
        };
        complete_empty(&mut runtime, measurement, 1);

        let ChannelDispatch::TimeSync(sync) = runtime
            .select_work(MonotonicMicros::new(1))
            .unwrap()
            .unwrap()
        else {
            panic!("overdue time sync must run after measurement");
        };
        runtime
            .complete_time_sync(sync, MonotonicMicros::new(2))
            .unwrap();
        assert_eq!(
            runtime.complete_time_sync(sync, MonotonicMicros::new(3)),
            Err(ChannelRuntimeError::StaleDispatch)
        );

        assert!(runtime
            .select_work(MonotonicMicros::new(90_001))
            .unwrap()
            .is_none());
        assert!(matches!(
            runtime.select_work(MonotonicMicros::new(90_002)).unwrap(),
            Some(ChannelDispatch::TimeSync(_))
        ));
    }

    #[test]
    fn failed_time_sync_retries_without_blocking_measurement() {
        let mut runtime = runtime(2);
        runtime
            .register_node(config(1), SequenceCursor::ORIGIN, MonotonicMicros::new(0))
            .unwrap();
        runtime
            .register_node(config(2), SequenceCursor::ORIGIN, MonotonicMicros::new(0))
            .unwrap();
        runtime
            .configure_time_sync(1, time_config(), MonotonicMicros::new(0))
            .unwrap();

        let ChannelDispatch::Measurement(first) = runtime
            .select_work(MonotonicMicros::new(0))
            .unwrap()
            .unwrap()
        else {
            panic!("measurement must win");
        };
        complete_empty(&mut runtime, first, 1);
        let ChannelDispatch::Measurement(second) = runtime
            .select_work(MonotonicMicros::new(1))
            .unwrap()
            .unwrap()
        else {
            panic!("second overdue measurement must win");
        };
        complete_empty(&mut runtime, second, 2);

        let ChannelDispatch::TimeSync(sync) = runtime
            .select_work(MonotonicMicros::new(2))
            .unwrap()
            .unwrap()
        else {
            panic!("time sync must follow overdue measurements");
        };
        runtime
            .fail_time_sync(sync, MonotonicMicros::new(3))
            .unwrap();

        assert!(runtime
            .select_work(MonotonicMicros::new(20_002))
            .unwrap()
            .is_none());
        assert!(matches!(
            runtime.select_work(MonotonicMicros::new(20_003)).unwrap(),
            Some(ChannelDispatch::TimeSync(_))
        ));
    }

    #[test]
    fn time_sync_configuration_is_separate_and_fail_closed() {
        let mut runtime = runtime(1);
        runtime
            .register_node(config(1), SequenceCursor::ORIGIN, MonotonicMicros::new(0))
            .unwrap();
        let mut invalid = time_config();
        invalid.estimated_slot = duration(100_001);
        assert_eq!(
            runtime.configure_time_sync(1, invalid, MonotonicMicros::new(0)),
            Err(ChannelRuntimeError::TimeSyncSlotExceedsInterval)
        );
        runtime
            .configure_time_sync(1, time_config(), MonotonicMicros::new(0))
            .unwrap();
        assert_eq!(
            runtime.configure_time_sync(1, time_config(), MonotonicMicros::new(0)),
            Err(ChannelRuntimeError::DuplicateTimeSync(1))
        );
        assert_eq!(
            runtime.select_measurement(MonotonicMicros::new(0)),
            Err(ChannelRuntimeError::UnifiedDispatchRequired)
        );
    }

    #[test]
    fn first_deadlines_stagger_time_sync_across_nodes() {
        let mut runtime = runtime(2);
        for address in 1..=2 {
            runtime
                .register_node(
                    config(address),
                    SequenceCursor::ORIGIN,
                    MonotonicMicros::new(0),
                )
                .unwrap();
        }
        runtime
            .configure_time_sync(1, time_config(), MonotonicMicros::new(100_000))
            .unwrap();
        runtime
            .configure_time_sync(2, time_config(), MonotonicMicros::new(150_000))
            .unwrap();

        for now in 0..=1 {
            let ChannelDispatch::Measurement(dispatch) = runtime
                .select_work(MonotonicMicros::new(now))
                .unwrap()
                .unwrap()
            else {
                panic!("initial measurement must run first");
            };
            complete_empty(&mut runtime, dispatch, now + 1);
        }
        assert!(runtime
            .select_work(MonotonicMicros::new(89_999))
            .unwrap()
            .is_none());
        let Some(ChannelDispatch::TimeSync(first)) =
            runtime.select_work(MonotonicMicros::new(90_000)).unwrap()
        else {
            panic!("first phased sync must become ready");
        };
        assert_eq!(first.node_address, 1);
        runtime
            .complete_time_sync(first, MonotonicMicros::new(90_001))
            .unwrap();
        assert!(runtime
            .select_work(MonotonicMicros::new(139_999))
            .unwrap()
            .is_none());
        let Some(ChannelDispatch::TimeSync(second)) =
            runtime.select_work(MonotonicMicros::new(140_000)).unwrap()
        else {
            panic!("second phased sync must become ready later");
        };
        assert_eq!(second.node_address, 2);
    }
}
