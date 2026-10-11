#!/usr/bin/env bash
set -euo pipefail

CXX="${CXX:-g++}"
BUILD_DIR="${TMPDIR:-/tmp}/sensor_system_host_tests"
NODE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CXX_PLATFORM_FLAGS=()

# Some standalone Apple Command Line Tools releases keep libc++ headers only
# inside the active SDK while clang's default C++ search path points at the
# empty toolchain directory. Keep Linux unchanged and make the host tests use
# the SDK selected by xcrun on macOS.
if [[ $(uname -s) == "Darwin" ]] && command -v xcrun >/dev/null 2>&1; then
    SDK_PATH="$(xcrun --show-sdk-path)"
    if [[ -d "${SDK_PATH}/usr/include/c++/v1" ]]; then
        CXX_PLATFORM_FLAGS+=("-isystem" "${SDK_PATH}/usr/include/c++/v1")
    fi
fi

cd "${NODE_DIR}"

mkdir -p "${BUILD_DIR}"

compile_and_run() {
    local source="$1"
    local name
    name="$(basename "${source}" .cpp)"

    "${CXX}" -std=c++17 -Wall -Wextra -pedantic \
        "${CXX_PLATFORM_FLAGS[@]}" \
        -Itests/stubs \
        -I. \
        "${source}" "${@:2}" \
        -o "${BUILD_DIR}/${name}"

    "${BUILD_DIR}/${name}"
    echo "PASS ${name}"
}

compile_and_run tests/decimating_filter_test.cpp
compile_and_run tests/drdy_timestamp_ring_test.cpp
compile_and_run tests/packet_queue_test.cpp
compile_and_run tests/data_plane_test.cpp
compile_and_run tests/fifo_service_policy_test.cpp
compile_and_run tests/acquisition_engine_test.cpp
compile_and_run tests/observer_snapshot_sink_test.cpp
compile_and_run tests/usb_observer_endpoint_test.cpp \
    transport/frame_encoder.cpp \
    transport/frame_decoder.cpp \
    transport/crc16.cpp
compile_and_run tests/transport_test.cpp \
    transport/frame_encoder.cpp \
    transport/frame_decoder.cpp \
    transport/crc16.cpp
compile_and_run tests/boot_update_protocol_test.cpp boot/boot_update_protocol.cpp
compile_and_run tests/boot_runtime_logic_test.cpp \
    boot/boot_metadata.cpp \
    boot_shared/boot_runtime_logic.cpp
compile_and_run tests/protocol_v3_framing_test.cpp protocol_v3/framing.cpp
compile_and_run tests/protocol_v3_identity_test.cpp protocol_v3/identity.cpp
compile_and_run tests/protocol_v3_time_test.cpp protocol_v3/time.cpp
compile_and_run tests/node_v2_capabilities_test.cpp protocol_v3/identity.cpp
compile_and_run tests/protocol_v3_lifecycle_test.cpp protocol_v3/lifecycle.cpp
compile_and_run tests/protocol_v3_dispatcher_test.cpp \
    protocol_v3/dispatcher.cpp \
    protocol_v3/time.cpp \
    protocol_v3/framing.cpp \
    protocol_v3/identity.cpp \
    protocol_v3/lifecycle.cpp \
    protocol_v3/measurement.cpp
compile_and_run tests/protocol_v3_measurement_test.cpp protocol_v3/measurement.cpp
compile_and_run tests/measurement_stream_test.cpp
compile_and_run tests/measurement_journal_test.cpp
compile_and_run tests/sample_ingress_test.cpp -pthread
compile_and_run tests/measurement_ingress_sink_test.cpp
compile_and_run tests/operation_mailbox_test.cpp
compile_and_run tests/sensor_runtime_test.cpp
compile_and_run tests/system_operation_runtime_test.cpp
compile_and_run tests/core1_protocol_runtime_test.cpp
compile_and_run tests/node_v2_runtime_types_test.cpp
compile_and_run tests/node_v2_composition_types_test.cpp \
    protocol_v3/dispatcher.cpp \
    protocol_v3/time.cpp \
    protocol_v3/framing.cpp \
    protocol_v3/identity.cpp \
    protocol_v3/lifecycle.cpp \
    protocol_v3/measurement.cpp
compile_and_run tests/node_v2_runtime_composition_test.cpp \
    protocol_v3/dispatcher.cpp \
    protocol_v3/time.cpp \
    protocol_v3/identity.cpp \
    protocol_v3/lifecycle.cpp \
    protocol_v3/measurement.cpp
compile_and_run tests/node_v2_core1_composition_test.cpp \
    protocol_v3/dispatcher.cpp \
    protocol_v3/time.cpp \
    protocol_v3/framing.cpp \
    protocol_v3/identity.cpp \
    protocol_v3/lifecycle.cpp \
    protocol_v3/measurement.cpp
compile_and_run tests/delivery_session_test.cpp
compile_and_run tests/protocol_v3_measurement_handler_test.cpp protocol_v3/measurement.cpp
compile_and_run tests/protocol_v3_response_outbox_test.cpp \
    protocol_v3/dispatcher.cpp \
    protocol_v3/time.cpp \
    protocol_v3/framing.cpp \
    protocol_v3/identity.cpp \
    protocol_v3/lifecycle.cpp \
    protocol_v3/measurement.cpp
compile_and_run tests/protocol_v3_transport_endpoint_test.cpp \
    protocol_v3/dispatcher.cpp \
    protocol_v3/time.cpp \
    protocol_v3/framing.cpp \
    protocol_v3/identity.cpp \
    protocol_v3/lifecycle.cpp \
    protocol_v3/measurement.cpp
