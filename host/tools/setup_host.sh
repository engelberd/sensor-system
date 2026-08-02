#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PYTHON_BIN="${PYTHON_BIN:-python3}"
VENV_DIR="${ROOT_DIR}/host/.venv"
CONFIG_PATH="${ROOT_DIR}/host/system_config.json"
CONFIG_TEMPLATE="${ROOT_DIR}/host/configs/host_system.local.example.json"
LOCAL_STATE_DIR="${ROOT_DIR}/var"

if ! command -v "${PYTHON_BIN}" >/dev/null 2>&1; then
    echo "[ERROR] Nie znaleziono ${PYTHON_BIN}. Zainstaluj Python 3 z pakietem venv." >&2
    exit 2
fi

echo "[1/3] Przygotowanie środowiska Python: ${VENV_DIR}"
"${PYTHON_BIN}" -m venv "${VENV_DIR}"

echo "[2/3] Instalowanie zależności hosta"
"${VENV_DIR}/bin/python" -m pip install --upgrade pip
"${VENV_DIR}/bin/python" -m pip install -r "${ROOT_DIR}/host/requirements-recorder.txt"

echo "[3/3] Przygotowanie konfiguracji i katalogów danych"
mkdir -p \
    "${LOCAL_STATE_DIR}/recordings" \
    "${LOCAL_STATE_DIR}/archive" \
    "${LOCAL_STATE_DIR}/diagnostics" \
    "${LOCAL_STATE_DIR}/log" \
    "${LOCAL_STATE_DIR}/tmp"
if [[ -e "${CONFIG_PATH}" ]]; then
    echo "[OK] Zachowano istniejącą konfigurację: ${CONFIG_PATH}"
else
    cp "${CONFIG_TEMPLATE}" "${CONFIG_PATH}"
    echo "[OK] Utworzono konfigurację z szablonu: ${CONFIG_PATH}"
fi

echo
echo "Gotowe. Teraz ustaw porty i czujniki w host/system_config.json, a następnie uruchom:"
echo "  ./hostctl supervisor --config host/system_config.json"
echo "  ./hostctl operator --config host/system_config.json --host 127.0.0.1 --port 8090"
echo "  ./hostctl paths --config host/system_config.json"
echo "  ./hostctl doctor --config host/system_config.json"
