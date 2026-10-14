use core::fmt;
use std::collections::HashSet;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::channel_activation::{
    RecordingActivationError, RecordingActivationPolicy, RecordingShutdownPolicy,
};
use crate::channel_admission::AdmissionPolicy;
use crate::channel_agent::ChannelPolicy;
use crate::channel_scheduler::ChannelSchedulerPolicy;
use crate::channel_startup::{ChannelStartupError, ChannelStartupPolicy, RecordingAssemblyPolicy};
use crate::channel_worker::{ControlledChannelShutdownError, ControlledChannelShutdownPolicy};
use crate::installation_bootstrap::{
    SerialChannelBootstrapError, SerialInstallationBootstrapPolicy,
};
use crate::operations::ChannelOperationsPolicy;
use crate::time_profile::TimeSyncInstallationPolicy;
use crate::transaction::DurationMicros;

const RUNTIME_V2_KEYS: [&str; 30] = [
    "supervision_interval_ms",
    "metrics_interval_ms",
    "transaction_timeout_ms",
    "retry_delay_ms",
    "measurement_attempts_per_slot",
    "max_pending_work_items",
    "quarantine_us",
    "bootstrap_node_deadline_ms",
    "serial_io_timeout_ms",
    "maximum_utilization_ppm",
    "recovery_reserve_ppm",
    "target_chunk_samples",
    "measurement_transaction_time_us",
    "measurement_slot_us",
    "maximum_service_interval_ms",
    "retry_base_delay_ms",
    "retry_max_delay_ms",
    "retry_attempt_limit",
    "offline_probe_interval_ms",
    "time_sync_transaction_time_ms",
    "overflow_guard_ms",
    "max_slot_ms",
    "first_time_sync_delay_ms",
    "time_sync_phase_step_ms",
    "activation_node_deadline_ms",
    "shutdown_barrier_deadline_ms",
    "shutdown_node_deadline_ms",
    "shutdown_drain_deadline_ms",
    "operations_mailbox_capacity",
    "operations_ledger_capacity",
];

/// Tunable process policy for host v2.
///
/// This profile owns timing and bounded-capacity choices only. Physical
/// identity remains in topology, while storage paths and persistence policy
/// remain in `InstallationStorageProfile`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostRuntimeInstallationProfile {
    supervision_interval: Duration,
    metrics_interval: Duration,
    serial_bootstrap: SerialInstallationBootstrapPolicy,
    startup_base: ChannelStartupBasePolicy,
    recording_activation: RecordingActivationPolicy,
    recording_assembly: RecordingAssemblyPolicy,
    controlled_shutdown: ControlledChannelShutdownPolicy,
    operations: ChannelOperationsPolicy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChannelStartupBasePolicy {
    admission: AdmissionPolicy,
    target_chunk_samples: u16,
    measurement_transaction_time: DurationMicros,
    measurement_slot: DurationMicros,
    maximum_service_interval: DurationMicros,
    retry_base_delay: DurationMicros,
    retry_max_delay: DurationMicros,
    retry_attempt_limit: u8,
    offline_probe_interval: DurationMicros,
    time_sync_transaction_time: DurationMicros,
}

impl HostRuntimeInstallationProfile {
    pub fn from_document(document: &Value) -> Result<Self, RuntimeProfileError> {
        let root = document
            .as_object()
            .ok_or(RuntimeProfileError::RootNotObject)?;
        let runtime = object_field(root, "runtime_v2")?;
        reject_unknown_keys(runtime)?;

        let serial_bootstrap = SerialInstallationBootstrapPolicy {
            agent: ChannelPolicy {
                transaction_timeout: duration_micros_from_ms(runtime, "transaction_timeout_ms")?,
                retry_delay: duration_micros_from_ms(runtime, "retry_delay_ms")?,
                measurement_attempts_per_slot: u8_field(runtime, "measurement_attempts_per_slot")?,
                max_pending_work_items: usize_field(runtime, "max_pending_work_items")?,
            },
            quarantine_duration: duration_micros(runtime, "quarantine_us")?,
            node_deadline: duration_from_ms(runtime, "bootstrap_node_deadline_ms")?,
            serial_io_timeout: duration_from_ms(runtime, "serial_io_timeout_ms")?,
        };
        serial_bootstrap
            .validate()
            .map_err(RuntimeProfileError::SerialBootstrap)?;

        let startup_base = ChannelStartupBasePolicy {
            admission: AdmissionPolicy {
                maximum_utilization_ppm: u32_field(runtime, "maximum_utilization_ppm")?,
                recovery_reserve_ppm: u32_field(runtime, "recovery_reserve_ppm")?,
            },
            target_chunk_samples: u16_field(runtime, "target_chunk_samples")?,
            measurement_transaction_time: duration_micros(
                runtime,
                "measurement_transaction_time_us",
            )?,
            measurement_slot: duration_micros(runtime, "measurement_slot_us")?,
            maximum_service_interval: duration_micros_from_ms(
                runtime,
                "maximum_service_interval_ms",
            )?,
            retry_base_delay: duration_micros_from_ms(runtime, "retry_base_delay_ms")?,
            retry_max_delay: duration_micros_from_ms(runtime, "retry_max_delay_ms")?,
            retry_attempt_limit: u8_field(runtime, "retry_attempt_limit")?,
            offline_probe_interval: duration_micros_from_ms(runtime, "offline_probe_interval_ms")?,
            time_sync_transaction_time: duration_micros_from_ms(
                runtime,
                "time_sync_transaction_time_ms",
            )?,
        };

        if startup_base.retry_base_delay.get() > startup_base.retry_max_delay.get() {
            return Err(RuntimeProfileError::InvalidRelation(
                "retry_base_delay_ms must not exceed retry_max_delay_ms",
            ));
        }

        let recording_activation = RecordingActivationPolicy {
            node_deadline: duration_from_ms(runtime, "activation_node_deadline_ms")?,
        };
        recording_activation
            .validate()
            .map_err(RuntimeProfileError::Activation)?;

        let recording_assembly = RecordingAssemblyPolicy {
            scheduler: ChannelSchedulerPolicy {
                overflow_guard: duration_micros_from_ms(runtime, "overflow_guard_ms")?,
                max_slot: duration_micros_from_ms(runtime, "max_slot_ms")?,
            },
            first_time_sync_delay: duration_micros_from_ms(runtime, "first_time_sync_delay_ms")?,
            time_sync_phase_step: duration_micros_from_ms(runtime, "time_sync_phase_step_ms")?,
        };

        let controlled_shutdown = ControlledChannelShutdownPolicy {
            barrier_deadline: duration_from_ms(runtime, "shutdown_barrier_deadline_ms")?,
            node: RecordingShutdownPolicy {
                node_deadline: duration_from_ms(runtime, "shutdown_node_deadline_ms")?,
            },
            drain_deadline: duration_from_ms(runtime, "shutdown_drain_deadline_ms")?,
        };
        controlled_shutdown
            .validate()
            .map_err(RuntimeProfileError::ControlledShutdown)?;

        let operations = ChannelOperationsPolicy {
            mailbox_capacity: usize_field(runtime, "operations_mailbox_capacity")?,
            ledger_capacity: usize_field(runtime, "operations_ledger_capacity")?,
        };
        operations
            .validate()
            .map_err(RuntimeProfileError::Operations)?;

        Ok(Self {
            supervision_interval: duration_from_ms(runtime, "supervision_interval_ms")?,
            metrics_interval: duration_from_ms(runtime, "metrics_interval_ms")?,
            serial_bootstrap,
            startup_base,
            recording_activation,
            recording_assembly,
            controlled_shutdown,
            operations,
        })
    }

    pub const fn supervision_interval(&self) -> Duration {
        self.supervision_interval
    }

    pub const fn metrics_interval(&self) -> Duration {
        self.metrics_interval
    }

    pub const fn serial_bootstrap(&self) -> SerialInstallationBootstrapPolicy {
        self.serial_bootstrap
    }

    pub fn channel_startup(
        &self,
        time_sync: TimeSyncInstallationPolicy,
    ) -> Result<ChannelStartupPolicy, RuntimeProfileError> {
        let policy = ChannelStartupPolicy {
            admission: self.startup_base.admission,
            target_chunk_samples: self.startup_base.target_chunk_samples,
            measurement_transaction_time: self.startup_base.measurement_transaction_time,
            measurement_slot: self.startup_base.measurement_slot,
            maximum_service_interval: self.startup_base.maximum_service_interval,
            retry_base_delay: self.startup_base.retry_base_delay,
            retry_max_delay: self.startup_base.retry_max_delay,
            retry_attempt_limit: self.startup_base.retry_attempt_limit,
            offline_probe_interval: self.startup_base.offline_probe_interval,
            time_sync_schedule: time_sync.schedule(),
            time_sync_transaction_time: self.startup_base.time_sync_transaction_time,
        };
        policy.validate().map_err(RuntimeProfileError::Startup)
    }

    pub const fn recording_activation(&self) -> RecordingActivationPolicy {
        self.recording_activation
    }

    pub const fn recording_assembly(&self) -> RecordingAssemblyPolicy {
        self.recording_assembly
    }

    pub const fn controlled_shutdown(&self) -> ControlledChannelShutdownPolicy {
        self.controlled_shutdown
    }

    pub const fn operations(&self) -> ChannelOperationsPolicy {
        self.operations
    }
}

fn object_field<'a>(
    object: &'a Map<String, Value>,
    key: &'static str,
) -> Result<&'a Map<String, Value>, RuntimeProfileError> {
    object
        .get(key)
        .ok_or(RuntimeProfileError::MissingField(key))?
        .as_object()
        .ok_or(RuntimeProfileError::InvalidFieldType(key))
}

fn unsigned_field(
    object: &Map<String, Value>,
    key: &'static str,
) -> Result<u64, RuntimeProfileError> {
    object
        .get(key)
        .ok_or(RuntimeProfileError::MissingField(key))?
        .as_u64()
        .ok_or(RuntimeProfileError::InvalidFieldType(key))
}

fn nonzero_unsigned_field(
    object: &Map<String, Value>,
    key: &'static str,
) -> Result<u64, RuntimeProfileError> {
    let value = unsigned_field(object, key)?;
    if value == 0 {
        Err(RuntimeProfileError::ZeroField(key))
    } else {
        Ok(value)
    }
}

fn narrow<T>(object: &Map<String, Value>, key: &'static str) -> Result<T, RuntimeProfileError>
where
    T: TryFrom<u64>,
{
    T::try_from(nonzero_unsigned_field(object, key)?)
        .map_err(|_| RuntimeProfileError::FieldOverflow(key))
}

fn u8_field(object: &Map<String, Value>, key: &'static str) -> Result<u8, RuntimeProfileError> {
    narrow(object, key)
}

fn u16_field(object: &Map<String, Value>, key: &'static str) -> Result<u16, RuntimeProfileError> {
    narrow(object, key)
}

fn u32_field(object: &Map<String, Value>, key: &'static str) -> Result<u32, RuntimeProfileError> {
    narrow(object, key)
}

fn usize_field(
    object: &Map<String, Value>,
    key: &'static str,
) -> Result<usize, RuntimeProfileError> {
    narrow(object, key)
}

fn duration_micros(
    object: &Map<String, Value>,
    key: &'static str,
) -> Result<DurationMicros, RuntimeProfileError> {
    DurationMicros::new(nonzero_unsigned_field(object, key)?)
        .map_err(|_| RuntimeProfileError::ZeroField(key))
}

fn duration_micros_from_ms(
    object: &Map<String, Value>,
    key: &'static str,
) -> Result<DurationMicros, RuntimeProfileError> {
    let micros = nonzero_unsigned_field(object, key)?
        .checked_mul(1_000)
        .ok_or(RuntimeProfileError::FieldOverflow(key))?;
    DurationMicros::new(micros).map_err(|_| RuntimeProfileError::ZeroField(key))
}

fn duration_from_ms(
    object: &Map<String, Value>,
    key: &'static str,
) -> Result<Duration, RuntimeProfileError> {
    Ok(Duration::from_millis(nonzero_unsigned_field(object, key)?))
}

fn reject_unknown_keys(object: &Map<String, Value>) -> Result<(), RuntimeProfileError> {
    let allowed: HashSet<&str> = RUNTIME_V2_KEYS.iter().copied().collect();
    if let Some(key) = object.keys().find(|key| !allowed.contains(key.as_str())) {
        Err(RuntimeProfileError::UnknownField(key.clone()))
    } else {
        Ok(())
    }
}

#[derive(Debug)]
pub enum RuntimeProfileError {
    RootNotObject,
    MissingField(&'static str),
    InvalidFieldType(&'static str),
    ZeroField(&'static str),
    FieldOverflow(&'static str),
    UnknownField(String),
    InvalidRelation(&'static str),
    SerialBootstrap(SerialChannelBootstrapError),
    Startup(ChannelStartupError),
    Activation(RecordingActivationError),
    ControlledShutdown(ControlledChannelShutdownError),
    Operations(crate::operations::OperationsError),
}

impl fmt::Display for RuntimeProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "host runtime profile error: {self:?}")
    }
}

impl std::error::Error for RuntimeProfileError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time_profile::TimeSyncInstallationPolicy;

    fn valid_document() -> Value {
        serde_json::from_str(include_str!("../../../../host/configs/common.json")).unwrap()
    }

    #[test]
    fn tracked_runtime_profile_builds_all_runtime_policies() {
        let profile = HostRuntimeInstallationProfile::from_document(&valid_document()).unwrap();
        let time_sync = TimeSyncInstallationPolicy::new(
            DurationMicros::new(10_000_000).unwrap(),
            DurationMicros::new(20_000).unwrap(),
            DurationMicros::new(250_000).unwrap(),
        )
        .unwrap();
        let startup = profile.channel_startup(time_sync).unwrap();

        assert_eq!(profile.supervision_interval(), Duration::from_millis(100));
        assert_eq!(profile.metrics_interval(), Duration::from_secs(10));
        assert_eq!(profile.serial_bootstrap().agent.max_pending_work_items, 8);
        assert_eq!(startup.target_chunk_samples, 32);
        assert_eq!(startup.measurement_slot.get(), 40_000);
        assert_eq!(
            profile.controlled_shutdown().drain_deadline,
            Duration::from_secs(5)
        );
        assert_eq!(profile.operations().mailbox_capacity, 8);
        assert_eq!(profile.operations().ledger_capacity, 64);
    }

    #[test]
    fn rejects_unknown_and_invalid_runtime_values_before_io() {
        let mut document = valid_document();
        document["runtime_v2"]["retry_delai_ms"] = Value::from(1);
        assert!(matches!(
            HostRuntimeInstallationProfile::from_document(&document),
            Err(RuntimeProfileError::UnknownField(_))
        ));

        let mut document = valid_document();
        document["runtime_v2"]["measurement_slot_us"] = Value::from(30_000);
        let profile = HostRuntimeInstallationProfile::from_document(&document).unwrap();
        let time_sync = TimeSyncInstallationPolicy::new(
            DurationMicros::new(10_000_000).unwrap(),
            DurationMicros::new(20_000).unwrap(),
            DurationMicros::new(250_000).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            profile.channel_startup(time_sync),
            Err(RuntimeProfileError::Startup(
                ChannelStartupError::TransactionExceedsSlot
            ))
        ));
    }
}
