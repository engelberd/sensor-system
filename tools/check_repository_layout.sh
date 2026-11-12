#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

required=(
  host/runtime/Cargo.toml
  host/runtime/crates/sensor-host/Cargo.toml
  host/configs/common.json
  node/protocol/framing.h
  node/CMakeLists.txt
)

for path in "${required[@]}"; do
  [[ -e $path ]] || { echo "missing canonical repository path: $path" >&2; exit 1; }
done

for retired in host_v2 node/protocol_v3; do
  [[ ! -e $retired ]] || { echo "retired repository path returned: $retired" >&2; exit 1; }
done

if [[ -x ./sgit ]]; then
  vcs=(./sgit)
else
  vcs=(git)
fi
tracked_generated=$("${vcs[@]}" ls-files | grep -E '(^|/)(\.DS_Store|__pycache__|\.ipynb_checkpoints)(/|$)|\.pyc$' || true)
if [[ -n $tracked_generated ]]; then
  printf 'generated files are tracked:\n%s\n' "$tracked_generated" >&2
  exit 1
fi

echo "repository layout: OK"
