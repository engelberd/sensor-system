#!/usr/bin/env bash
set -euo pipefail

CXX="${CXX:-g++}"
BUILD_DIR="${TMPDIR:-/tmp}/sensor_system_host_tests"
NODE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "${NODE_DIR}"

mkdir -p "${BUILD_DIR}"

compile_and_run() {
    local source="$1"
    local name
    name="$(basename "${source}" .cpp)"

    "${CXX}" -std=c++17 -Wall -Wextra -pedantic \
        -Itests/stubs \
        -I. \
        "${source}" "${@:2}" \
        -o "${BUILD_DIR}/${name}"

    "${BUILD_DIR}/${name}"
    echo "PASS ${name}"
}

compile_and_run tests/decimating_filter_test.cpp
compile_and_run tests/packet_queue_test.cpp
compile_and_run tests/data_plane_test.cpp
compile_and_run tests/fifo_service_policy_test.cpp
compile_and_run tests/acquisition_engine_test.cpp
compile_and_run tests/boot_update_protocol_test.cpp boot/boot_update_protocol.cpp
