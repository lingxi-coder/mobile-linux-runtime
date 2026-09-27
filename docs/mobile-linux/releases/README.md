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

The x86_64 candidate is being built on native Ubuntu with every pinned APK hash,
offline installation and actual Node/toolchain probes enforced. Its historical
blocked closure is accepted only by explicit verification build mode; it cannot
pass default release acceptance until actual successful evidence is recorded.
Missing archive payloads or incomplete ABI evidence are release failures.
Source gates verify committed manifests and inventories; release jobs additionally
require and verify archive bytes against every immutable manifest entry.
