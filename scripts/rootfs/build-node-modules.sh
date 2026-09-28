#!/usr/bin/env bash
# Build caller-supplied, digest-bound dependencies inside an explicit Alpine rootfs.
# Requires --arch --rootfs --bundle-dir --lock-sha256 --output-dir --cache-dir.
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
BUNDLE=""
EXPECTED_LOCK_SHA=""
CACHE=""

ARCH=""
ROOTFS=""
OUTPUT=""
CONTAINER_RUNTIME="${CONTAINER_RUNTIME:-}"

usage() { sed -n '2,24p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --arch) ARCH="${2:-}"; shift 2 ;;
    --bundle-dir) BUNDLE="${2:-}"; shift 2 ;;
    --lock-sha256) EXPECTED_LOCK_SHA="${2:-}"; shift 2 ;;
    --cache-dir) CACHE="${2:-}"; shift 2 ;;
    --rootfs) ROOTFS="${2:-}"; shift 2 ;;
    --output|--output-dir) OUTPUT="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

case "${ARCH}" in
  aarch64|x86_64) ;;
  *) echo "--arch must be aarch64 or x86_64" >&2; exit 2 ;;
esac
[[ -n "${ROOTFS}" && -n "${OUTPUT}" && -n "${CACHE}" && -n "${BUNDLE}" && "${EXPECTED_LOCK_SHA}" =~ ^[0-9a-f]{64}$ ]] || {
  echo "--rootfs --bundle-dir --lock-sha256 --output-dir --cache-dir are required" >&2; exit 2; }
OUTPUT="$(python3 "${SCRIPT_DIR}/source_contract.py" --input "${ROOTFS}" "${OUTPUT}")"
CACHE="$(python3 "${SCRIPT_DIR}/source_contract.py" --input "${ROOTFS}" "${CACHE}")"
python3 "${SCRIPT_DIR}/verify-bundle.py" --bundle-dir "${BUNDLE}" --lock-sha256 "${EXPECTED_LOCK_SHA}" --output-dir "${OUTPUT}" --cache-dir "${CACHE}"
[[ -f "${ROOTFS}" ]] || { echo "missing rootfs: ${ROOTFS}" >&2; exit 1; }
if [[ -z "${CONTAINER_RUNTIME}" ]]; then
  if command -v podman >/dev/null 2>&1; then CONTAINER_RUNTIME=podman
  elif command -v docker >/dev/null 2>&1; then CONTAINER_RUNTIME=docker
  else echo "neither podman nor docker is available" >&2; exit 1; fi
fi

TEMPLATE="${BUNDLE}"
ACTUAL_LOCK_SHA="$(shasum -a 256 "${TEMPLATE}/pnpm-lock.yaml" | awk '{print $1}')"
if [[ "${ACTUAL_LOCK_SHA}" != "${EXPECTED_LOCK_SHA}" ]]; then
  echo "bundle pnpm-lock.yaml does not match explicit --lock-sha256" >&2
  echo "  pins:     ${EXPECTED_LOCK_SHA}" >&2
  echo "  template: ${ACTUAL_LOCK_SHA}" >&2
  exit 1
fi

IMAGE="mobile-linux-runtime-node:${ARCH}"
case "${ARCH}" in
  aarch64) IMAGE_ARCH="arm64" ;;
  x86_64) IMAGE_ARCH="amd64" ;;
esac

echo "[node_modules:${ARCH}] importing ${ROOTFS} as ${IMAGE}"
# Re-import every run: a stale image from an older rootfs would resolve against
# an older Node/pnpm while reporting success for the current one.
"${CONTAINER_RUNTIME}" rmi -f "${IMAGE}" >/dev/null 2>&1 || true
"${CONTAINER_RUNTIME}" import --arch "${IMAGE_ARCH}" --os linux "${ROOTFS}" "${IMAGE}" >/dev/null

mkdir -p "${CACHE}"
WORK="$(mktemp -d "${CACHE}/dependency-work.XXXXXX")"
cleanup() { chmod -R u+w "${WORK}" 2>/dev/null || true; rm -rf "${WORK}"; }
trap cleanup EXIT
mkdir -p "${WORK}/project" "${WORK}/store" "${WORK}/state"
# The caller owns this explicitly digest-bound bundle. No SDK template lookup.
cp -a "${BUNDLE}/." "${WORK}/project/"
echo "[node_modules:${ARCH}] resolving the pinned lockfile inside the rootfs"
# The flags are the engine's own (local_apps_host.rs run_dependency_install), so
# the shipped tree is byte-identical in shape to one a device would build for
# itself -- including `--ignore-scripts`, which is why no postinstall runs here.
"${CONTAINER_RUNTIME}" run --rm \
  -v "${WORK}/project:/project" \
  -v "${WORK}/store:/var/lingxi/local-app-dependency-store" \
  -v "${WORK}/state:/state" \
  -e CI=1 \
  -e HOME=/state/home \
  -e TMPDIR=/state/tmp -e TMP=/state/tmp -e TEMP=/state/tmp \
  -e XDG_CACHE_HOME=/state/xdg-cache \
  -e XDG_CONFIG_HOME=/state/xdg-config \
  -e XDG_DATA_HOME=/state/xdg-data \
  -e PNPM_HOME=/state/pnpm-home \
  -e COREPACK_HOME=/state/corepack \
  -w /project \
  "${IMAGE}" \
  sh -c 'test "$(uname -m)" = "'"${ARCH}"'" || {
           echo "builder is $(uname -m), expected '"${ARCH}"'" >&2; exit 1; }
         exec /usr/bin/pnpm install --frozen-lockfile --ignore-scripts --no-runtime \
           --prefer-offline --store-dir /var/lingxi/local-app-dependency-store \
           --reporter=append-only'

mkdir -p "$(dirname "${OUTPUT}")"
chmod -R u+w "${OUTPUT}" 2>/dev/null || true
rm -rf "${OUTPUT}"
mkdir -p "${OUTPUT}"
# -a keeps the `.bin` shims as shims. Dereferencing them would double every
# binary they point at and break `vite` resolution on the device.
cp -a "${WORK}/project/node_modules" "${OUTPUT}/node_modules"
printf '%s\n' "${ACTUAL_LOCK_SHA}" > "${OUTPUT}/pnpm-lock.sha256"

echo "[node_modules:${ARCH}] lockfile sha256 ${ACTUAL_LOCK_SHA}"
du -sh "${OUTPUT}/node_modules" | awk '{print "[node_modules] tree size "$1}'
find "${OUTPUT}/node_modules" -type f | wc -l | awk '{print "[node_modules] files "$1}'
find "${OUTPUT}/node_modules" -type l | wc -l | awk '{print "[node_modules] shims "$1}'
echo "[node_modules:${ARCH}] wrote ${OUTPUT}/node_modules"
