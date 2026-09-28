#!/usr/bin/env bash
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1
exec "$(dirname "$0")/build-local-app-rootfs.sh" "$@"
