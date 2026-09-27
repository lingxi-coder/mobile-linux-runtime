#!/usr/bin/env bash
set -euo pipefail
SDK_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 "${SDK_ROOT}/native/android/build_native.py" "$@"
