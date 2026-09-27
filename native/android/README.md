# Android native support

This directory contains the Android-specific network policy launcher and PRoot
network overlay, plus the exact pinned OpenMinis PTY bridge and talloc sources.
`docs/android/native-pins.json` records their upstream revisions, archive hashes,
licenses and individual source hashes. The original application checkouts are
never patched by the builder.

The PRoot input is archived from revision
`8cf13e997cdc9472997aae19df8050c073c9a86c`. PTY/talloc are extracted from OpenMinis
`9cf3a855fecd27bb5735b84cacbd56852a3ab8dd`. PRoot receives the SDK network overlay
and the pinned `patches/proot-loader-16k.patch` only in a temporary build directory.
The builder forces 16 KiB ELF page alignment for all ARM64 helpers and verifies
every PT_LOAD segment before publication; this also supports 16 KiB Android devices
when the caller builds with an older NDK 27 revision. `PtyBridge.kt` retains its JNI class name
`com.openminis.app.sandbox.PtyBridge`, matching the pinned C symbol exports.
It belongs to the optional legacy host-shell API; the PRoot guest backend uses
the independent Rust `platform-pty` implementation.

Run `scripts/build/build-android-native.sh --ndk <ndk> --output-dir <output>
--cache-dir <cache>`. An optional `--proot-source <checkout>` must name an
independent checkout at the pinned commit. Otherwise the builder fetches that
commit into its external cache. Both `arm64-v8a` and `x86_64` are built by default;
`--abi` can select one. `--without-legacy-shell` omits mksh/toybox and the legacy
PTY bridge. PRoot/launcher/PTy target API 26 by default; the optional legacy shell
and minijail/libcap retain their existing API 29 floor. The manifest declares both.

Output is `jniLibs/<abi>/*.so` plus `native-manifest.json`. These are native
support artifacts only: **no Rust runtime core or FFI cdylib is included**.
A full SDK AAR adds its one SDK cdylib; Rust hosts embed the SDK crates into their
existing library instead and consume only this support bundle. Verify with
`scripts/checks/verify-android-native.py --artifact-dir <output>`. Source verification
uses `--source-only` and does not require an OpenMinis checkout.

The Rust backend takes an explicit `native_library_dir`; it does not infer an
application/library name. Optional `IsolatedBuildProfile` configuration supplies
all application-specific build paths/channels. Without a profile, isolated
application builds fail closed; normal shell/PTY/runtime operations remain
available. Harness/LingXi adapters supply their historical profile and remain
outside this SDK.
