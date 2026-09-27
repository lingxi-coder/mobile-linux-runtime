# Alpine 3.24.2 rootfs pipeline

The active source contract is `docs/toolchains/runtime-pins.json`: exact Alpine
builder image digests, the complete per-ABI APK closure, source-built Node and
integrity-pinned npm, pnpm and native TypeScript. Historical Alpine 3.21.3
minirootfs download records are preserved only in `docs/migration/legacy`.
They are not release archive identities.

Build with `scripts/mobile-linux/build-rootfs.sh --arch aarch64|x86_64
--output-dir /external/rootfs --cache-dir /external/cache`. Source is mounted
read-only; all mutable staging and compilation happen in explicit external paths.
Every APK is hash-checked before offline installation. Node is built from its
pinned source; matching cache provenance may reuse compiled output. The build
runs actual Node, TypeScript CLI and LSP probes before producing release evidence.
The x86 historical blocked closure can be tested only with the explicit
`--verify-blocked-closure` candidate mode. Default release acceptance stays closed
until successful native build evidence is recorded; candidate mode never changes
source pins automatically.

Archives include interactive package tools, Git, OpenSSH, Python, CA certificates
and the pinned Node/npm/pnpm/TypeScript toolchains. Optional dependency bundles
are supplied explicitly with a lock digest to `build-node-modules.sh`. Product
profiles, templates, skills, user permissions and application policies belong to
the caller and are not selected by this SDK.

Required evidence per ABI:

- `rootfs-manifest.json`: schema v2, exact archive hash and size, package licenses,
  complete immutable regular-file/symlink inventory and executable allowlist.
- `rootfs-build.lock.json`, `rootfs.spdx.json`, `executable-allowlist.json`.
- `apk-closure.json`, `producer-inputs.json` and the bounded interpreter alias
  transformation receipt.

`publish-rootfs-evidence.py` produces these records from an actual tree, archive
and exact APK closure. `verify-evidence.py --evidence-dir DIR --archive PATH`
requires archive bytes and matches every immutable payload entry to the manifest,
including hardlink aliases and symlink target bytes, alongside SPDX/license and
build-lock consistency. `--root DIR` additionally compares an extracted tree.
Missing payloads or mismatched evidence fail release acceptance.

Only `/bin/sh` and `/usr/bin/python3` may be normalized in new staging from
relative symlinks to same-byte ELF hardlinks. Their targets must stay inside the
immutable binary roots; source caches and archives remain unchanged. The receipt
records each target and before/after byte hash. Other symlinks retain their exact
inventory representation. Schema v1 is rejected because it lacks this inventory.

Real per-ABI candidate records live under `docs/mobile-linux/releases`.
`scripts/check-resource-contracts.sh` validates actual committed evidence and
proves the intentionally incomplete documentation sample is rejected.
`scripts/mobile-linux/test-rootfs-tooling.sh` exercises packaging and rejection
cases. The native `rootfs-build.yml` workflow verifies actual archives before
uploading candidate artifacts and separately asserts source checkout immutability.
