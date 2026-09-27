# Shared runtime toolchains

`runtime-pins.json` is the SDK-owned immutable Alpine/APK/Node/npm/pnpm/native-TypeScript contract. APK hashes, source hashes, builder image digests, complete dependency closures and toolchain licenses are preserved from the imported implementation. `import-provenance.json` records that source revision and file hashes.

Build a rootfs with `scripts/mobile-linux/build-rootfs.sh --arch aarch64 --output-dir /external/rootfs --cache-dir /external/node-cache`. Both mutable directories are mandatory and must be outside the source checkout. Existing source-build caches remain usable when their recorded source/configuration identity matches.

Optional JavaScript dependencies are caller-owned. Use `build-node-modules.sh --arch aarch64 --rootfs /external/rootfs/aarch64/rootfs.tar.gz --bundle-dir /caller/dependency-bundle --lock-sha256 <SHA256> --output-dir /external/modules --cache-dir /external/dependency-cache`. The bundle contains package.json, pnpm-lock.yaml and any workspace inputs; it must not contain symlinks. This generic producer does not choose a template, renderer, plugin or Local App profile. The caller retains those checks.

Installed guest paths and provenance schemas remain compatible with existing rootfs images; extraction does not rename `/opt/lingxi` paths or change rootfs bytes.
