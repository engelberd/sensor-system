#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/.." && pwd)

cd "$workspace"
cargo test -p sensor-host --lib host_coordinator::tests::one_four_and_eight_channels_share_time_and_shutdown_cleanly -- --exact
cargo test -p sensor-host --lib channel_supervisor::tests::one_faulted_channel_does_not_stop_the_other_workers -- --exact
cargo test -p sensor-host --lib channel_recording_loop::tests::multi_node_recording_survives_one_lost_response_and_storage_retry -- --exact
