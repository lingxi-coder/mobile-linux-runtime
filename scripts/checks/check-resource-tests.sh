#!/usr/bin/env bash
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1
cd "$(dirname "$0")/../.."
if ! result="$(python3 scripts/tests/test_resource_contracts.py 2>&1 && bash scripts/rootfs/test-rootfs-tooling.sh 2>&1)"; then
  printf '%s\n' "$result" >&2
  exit 1
fi
echo "SDK resource tests passed: bundle/pins/dependency rejection probes and complete rootfs tooling suite"
