#![forbid(unsafe_code)]

pub mod archive_coverage;
pub mod archive_retention;
pub mod bounded_writer;
pub mod capture_coverage_checkpoint;
pub mod capture_export;
pub mod capture_profile;
pub mod capture_repository;
pub mod capture_runtime;
pub mod capture_segments;
pub mod capture_wal;
pub mod capture_window;
pub mod channel_activation;
pub mod channel_admission;
pub mod channel_agent;
pub mod channel_bootstrap;
pub mod channel_endpoint;
pub mod channel_metrics;
pub mod channel_port;
pub mod channel_recording_loop;
pub mod channel_recovery;
pub mod channel_runtime;
pub mod channel_scheduler;
pub mod channel_startup;
pub mod channel_supervisor;
pub mod channel_worker;
pub mod commissioning;
pub mod configuration;
pub mod discovery;
pub mod discovery_executor;
pub mod handshake;
pub mod host_coordinator;
pub mod host_doctor;
pub mod host_health;
pub mod host_process;
pub mod installation_bootstrap;
pub mod installation_config;
pub mod installation_topology;
pub mod lifecycle;
pub mod measurement_coordinator;
pub mod measurement_persistence;
pub mod measurement_transfer;
pub mod node_metrics;
pub mod node_session;
pub mod operations;
pub mod persistence_executor;
pub mod retention_checkpoint;
pub mod retention_cleanup;
pub mod runtime_profile;
pub mod runtime_status;
pub mod simulation;
pub mod storage_health;
pub mod stream_recovery;
pub mod termination_signal;
pub mod time_health;
pub mod time_persistence;
pub mod time_profile;
pub mod time_repository;
pub mod time_runtime;
pub mod time_wal;
pub mod transaction;

pub const HOST_SOFTWARE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn version_line() -> String {
    let protocol = sensor_protocol::LOCAL_PROTOCOL_VERSION;
    format!(
        "sensor-host {} (framing {}, application {})",
        HOST_SOFTWARE_VERSION, protocol.framing, protocol.application
    )
}

#[cfg(test)]
mod tests {
    use super::version_line;

    #[test]
    fn version_output_identifies_software_and_wire_contract() {
        assert_eq!(
            version_line(),
            "sensor-host 0.5.0-dev (framing 3, application 5)"
        );
    }
}
