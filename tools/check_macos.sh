#!/usr/bin/env bash
set -u

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXPECTED_SDK_VERSION="$(tr -d '[:space:]' < "${ROOT_DIR}/node/PICO_SDK_VERSION")"
ERRORS=0
WARNINGS=0

ok() {
    echo "[OK] $*"
}

warn() {
    echo "[WARN] $*"
    WARNINGS=$((WARNINGS + 1))
}

error() {
    echo "[ERROR] $*"
    ERRORS=$((ERRORS + 1))
}

check_command() {
    if command -v "$1" >/dev/null 2>&1; then
        ok "$1: $(command -v "$1")"
    else
        error "Brak polecenia '$1'. Uruchom: brew bundle --file Brewfile"
    fi
}

if [[ "$(uname -s)" != "Darwin" ]]; then
    error "Ten checker jest przeznaczony dla macOS."
    echo "Summary: ${ERRORS} errors, ${WARNINGS} warnings"
    exit 2
fi

echo "macOS workstation check"
check_command brew
check_command git
check_command cmake
check_command ninja
check_command arm-none-eabi-gcc
check_command picotool
check_command python3

KICAD_CLI="/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"
if [[ -x "${KICAD_CLI}" ]]; then
    ok "KiCad CLI: ${KICAD_CLI}"
else
    error "Nie znaleziono KiCad CLI. Uruchom: brew install --cask kicad"
fi

if [[ -z "${PICO_SDK_PATH:-}" ]]; then
    error "PICO_SDK_PATH nie jest ustawione (oczekiwana wersja SDK: ${EXPECTED_SDK_VERSION})."
elif [[ ! -f "${PICO_SDK_PATH}/external/pico_sdk_import.cmake" ]]; then
    error "PICO_SDK_PATH nie wskazuje na kompletne Pico SDK: ${PICO_SDK_PATH}"
else
    ok "Pico SDK: ${PICO_SDK_PATH} (projekt zweryfikowany z ${EXPECTED_SDK_VERSION})"
fi

HOST_PYTHON="${ROOT_DIR}/host/.venv/bin/python"
if [[ -x "${HOST_PYTHON}" ]]; then
    if "${HOST_PYTHON}" -c 'import serial,numpy,h5py' >/dev/null 2>&1; then
        ok "Host Python venv i zależności"
    else
        error "Niekompletne host/.venv; uruchom ./host/tools/setup_host.sh <profil>."
    fi
else
    error "Brak host/.venv; uruchom ./host/tools/setup_host.sh <profil>."
fi

echo "Serial ports:"
if [[ -x "${HOST_PYTHON}" ]] && "${HOST_PYTHON}" -c 'import serial' >/dev/null 2>&1; then
    "${HOST_PYTHON}" -m serial.tools.list_ports -v
else
    ls /dev/cu.* 2>/dev/null || warn "Nie można wylistować portów /dev/cu.*."
fi

if system_profiler SPUSBDataType 2>/dev/null | grep -qiE 'CH9344|1A86'; then
    warn "Wykryto urządzenie WCH. CH9344 nie ma potwierdzonego oficjalnego sterownika macOS; sprawdź, czy powstały wszystkie porty /dev/cu.*."
fi

if ! ls /dev/cu.* >/dev/null 2>&1; then
    warn "Brak portów /dev/cu.*. Podłącz konwerter lub zainstaluj oficjalny sterownik właściwy dla jego chipsetu."
fi

echo "Summary: ${ERRORS} errors, ${WARNINGS} warnings"
if (( ERRORS > 0 )); then
    exit 2
fi
