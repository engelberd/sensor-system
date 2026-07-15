#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

VERSION="$(tr -d '[:space:]' < VERSION)"
HOST_VERSION="$(host/.venv/bin/python -c 'from host.common.version import PROJECT_VERSION; print(PROJECT_VERSION)')"
FW_VERSION="$(host/.venv/bin/python - <<'PY'
import re
from pathlib import Path

text = Path("node/common/protocol_ids.h").read_text(encoding="utf-8")
parts = []
for name in ("MAJOR", "MINOR", "PATCH"):
    match = re.search(rf"FW_VERSION_{name}\s*=\s*(\d+)", text)
    if match is None:
        raise SystemExit(f"missing FW_VERSION_{name}")
    parts.append(match.group(1))
print(".".join(parts))
PY
)"

if [[ "${VERSION}" != "${HOST_VERSION}" || "${VERSION}" != "${FW_VERSION}" ]]; then
    echo "[ERROR] Niespójne wersje: VERSION=${VERSION}, host=${HOST_VERSION}, firmware=${FW_VERSION}" >&2
    exit 2
fi

for local_file in host/host_config.json host/system_config.json; do
    if ./sgit ls-files --error-unmatch "${local_file}" >/dev/null 2>&1; then
        echo "[ERROR] Lokalna konfiguracja nadal jest śledzona: ${local_file}" >&2
        exit 2
    fi
done

if ./sgit ls-files 'host/systemd/*.service' | grep -q .; then
    echo "[ERROR] Lokalne unity systemd bez końcówki .example są śledzone." >&2
    exit 2
fi

echo "[1/5] Testy hosta"
host/.venv/bin/python -m unittest discover -s host/tests

echo "[2/5] Testy logiki firmware'u"
bash node/tests/run_host_tests.sh

echo "[3/5] Kontrola składni i diffu"
host/.venv/bin/python -m compileall -q host
./sgit diff --check

echo "[4/5] Budowanie firmware'u"
cmake --build node/build

echo "[5/5] Pakowanie release'u ${VERSION}"
cmake --build node/build --target sensor_system_node_release

(
    cd "node/build/releases"
    sha256sum -c "sensor-system-node-v${VERSION}.zip.sha256"
)

echo "[OK] Release v${VERSION} przeszedł pełną kontrolę."
