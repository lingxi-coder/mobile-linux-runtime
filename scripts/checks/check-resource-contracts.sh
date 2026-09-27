#!/usr/bin/env bash
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1
cd "$(dirname "$0")/../.."
python3 scripts/rootfs/verify-toolchain-pins.py
# This checked-in sample deliberately has placeholder hashes/size zero. Prove
# the real manifest validator rejects it; real artifact manifests are checked
# positively by resource tests and obligatorily by release-input verification.
if result="$(bash scripts/rootfs/check-rootfs-manifest.sh 2>&1)"; then
  echo "invalid documentation sample was accepted as a release manifest" >&2
  exit 1
fi
case "$result" in
  *"archive.size_bytes must be a positive integer"*) ;;
  *) printf 'manifest negative fixture failed for the wrong reason: %s\n' "$result" >&2; exit 1 ;;
esac
echo "rootfs manifest validator rejects the incomplete documentation sample"
# Actual arm64 candidate evidence is mandatory. Archive bytes are additionally
# required by rootfs-build.yml and release packaging; source gates verify the
# committed real inventory rather than substituting the documentation sample.
evidence=docs/mobile-linux/releases/3.24.2/arm64-v8a
bash scripts/rootfs/check-rootfs-manifest.sh "$evidence/rootfs-manifest.json"
python3 scripts/rootfs/verify-evidence.py --evidence-dir "$evidence"
