# Real SDK rootfs release inputs

The active builder uses Alpine 3.24.2 and the complete APK/source identities in
`docs/toolchains/runtime-pins.json`. The separate `mobile-linux-pins.json`
3.21.3 minirootfs downloads are a historical source contract, not these payloads.
The original contract had no published release archive digest; source archive
hashes must never be substituted for a toolchain-equipped release archive.

`3.24.2/arm64-v8a` contains evidence generated from the actual cached, verified
rootfs and repackaged in new staging. The original cache is unchanged. Only
`/bin/sh` and `/usr/bin/python3` interpreter aliases were converted from relative
symlinks to same-byte ELF hardlinks so immutable executable validation applies.
The transformation receipt records both source aliases and byte hashes.
The exact producer revision is verified against every recorded script and pin
hash. The release archive SHA256 is
`7be76008c419ad48a129f043bd7df44a5dafc872b65cf919b2670e6b03efe609`.

`3.24.2/x86_64` contains evidence from native Ubuntu rootfs-build run
`36329480255` (producer revision `6b72d2a610dbb44f15a88a8dc2d875ca4c8f9ff7`).
Its archive SHA256 is
`9948c666f8280a04d259f1e6c2dec3676685e23922ecabd563c6148eb50c2717`.
The run verified every pinned APK hash, offline installation and actual
Node/toolchain probes; `producer-run.json` fixes the run and archive identity.
The x86 closure was promoted only after those checks passed. The producer
evidence records the pre-promotion pins hash, while current pins differ only in
the x86 closure status, removed historical blocker and derived release-ready flag.
The actual x86 Android guest still requires the separate emulator workflow.
Missing archive payloads or incomplete ABI evidence are release failures.
Source gates verify committed manifests and inventories; release jobs additionally
require and verify archive bytes against every immutable manifest entry.
