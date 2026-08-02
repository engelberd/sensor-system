#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PYTHON_BIN="${PYTHON_BIN:-python3}"
VENV_DIR="${ROOT_DIR}/host/.venv"
CONFIG_PATH="${ROOT_DIR}/host/system_config.json"
LOCAL_STATE_DIR="${ROOT_DIR}/var"
SYSTEM_PROFILE="${1:-rpi-sanok}"
PROFILE_PATH="${ROOT_DIR}/host/configs/systems/${SYSTEM_PROFILE}.json"

if ! command -v "${PYTHON_BIN}" >/dev/null 2>&1; then
    echo "[ERROR] Nie znaleziono ${PYTHON_BIN}. Zainstaluj Python 3 z pakietem venv." >&2
    exit 2
fi

if [[ ! -f "${PROFILE_PATH}" ]]; then
    echo "[ERROR] Nie ma profilu systemu: ${PROFILE_PATH}" >&2
    echo "Dostępne profile:" >&2
    for profile_path in "${ROOT_DIR}"/host/configs/systems/*.json; do
        echo "  $(basename "${profile_path}")" >&2
    done
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
    "${VENV_DIR}/bin/python" -c \
        'import json,sys; from pathlib import Path; Path(sys.argv[1]).write_text(json.dumps({"extends": f"configs/systems/{sys.argv[2]}.json", "system": {"name": sys.argv[2]}}, indent=2) + "\n", encoding="utf-8")' \
        "${CONFIG_PATH}" "${SYSTEM_PROFILE}"
    echo "[OK] Utworzono lokalną konfigurację dla profilu ${SYSTEM_PROFILE}: ${CONFIG_PATH}"
fi

echo
echo "Gotowe. Teraz ustaw porty i czujniki w host/system_config.json, a następnie uruchom:"
echo "  ./hostctl supervisor --config host/system_config.json"
echo "  ./hostctl operator --config host/system_config.json --host 127.0.0.1 --port 8090"
echo "  ./hostctl paths --config host/system_config.json"
echo "  ./hostctl doctor --config host/system_config.json"
