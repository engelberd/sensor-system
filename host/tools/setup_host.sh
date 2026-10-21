#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PYTHON_BIN="${PYTHON_BIN:-python3}"
VENV_DIR="${ROOT_DIR}/host/.venv"
CONFIG_PATH="${ROOT_DIR}/host/system_config.json"
SYSTEM_PROFILE="${1:-rpi-sanok}"
PROFILE_PATH="${ROOT_DIR}/host/configs/systems/${SYSTEM_PROFILE}.json"

if ! command -v "${PYTHON_BIN}" >/dev/null 2>&1; then
    echo "[ERROR] Nie znaleziono ${PYTHON_BIN}. Zainstaluj Python 3 z pakietem venv." >&2
    exit 2
fi

if ! "${PYTHON_BIN}" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 11) else 1)'; then
    echo "[ERROR] Host v2 wymaga Python 3.11 lub nowszego." >&2
    exit 2
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "[ERROR] Nie znaleziono cargo. Zainstaluj toolchain z host_v2/rust-toolchain.toml." >&2
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

echo "[1/6] Przygotowanie środowiska Python dla dashboardu i aktualizacji firmware: ${VENV_DIR}"
"${PYTHON_BIN}" -m venv "${VENV_DIR}"

echo "[2/6] Instalacja lekkich zależności serwisowych"
"${VENV_DIR}/bin/python" -m pip install --disable-pip-version-check \
    -r "${ROOT_DIR}/host/requirements-v2.txt"

echo "[3/6] Przygotowanie lokalnej konfiguracji"
if [[ -e "${CONFIG_PATH}" ]]; then
    echo "[OK] Zachowano istniejącą konfigurację: ${CONFIG_PATH}"
else
    "${VENV_DIR}/bin/python" -c \
        'import json,sys; from pathlib import Path; Path(sys.argv[1]).write_text(json.dumps({"extends": f"configs/systems/{sys.argv[2]}.json", "system": {"name": sys.argv[2]}}, indent=2) + "\n", encoding="utf-8")' \
        "${CONFIG_PATH}" "${SYSTEM_PROFILE}"
    echo "[OK] Utworzono lokalną konfigurację dla profilu ${SYSTEM_PROFILE}: ${CONFIG_PATH}"
fi

echo "[4/6] Budowanie binariów host v2"
"${ROOT_DIR}/hostctl" build

echo "[5/6] Tworzenie katalogów danych z efektywnego profilu"
"${ROOT_DIR}/hostctl" paths --init

echo "[6/6] Walidacja konfiguracji bez otwierania RS485"
"${ROOT_DIR}/hostctl" validate

echo
echo "Bootstrap host v2 jest gotowy. Przed instalacją usług:"
echo "  1. zainstaluj sterownik konwertera i lokalne reguły udev;"
echo "  2. popraw tylko lokalne endpointy/storage/enable w host/system_config.json;"
echo "  3. uruchom ./hostctl discover, gdy RS485 jest dostępne;"
echo "  4. zainstaluj usługi, podając adres interfejsu administracyjnego:"
echo "     SENSOR_HOST_DASHBOARD_HOST=<TAILSCALE_IP> ./hostctl install-services"
echo "  5. uruchom ./hostctl start && ./hostctl doctor"
