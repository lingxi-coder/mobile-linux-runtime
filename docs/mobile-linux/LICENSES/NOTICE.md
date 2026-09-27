# Mobile Linux SDK component attribution

The root [MIT license](../../../LICENSE) applies to original SDK source carrying
that grant. It does not relicense imported source or the contents of a rootfs.
The full Android and iOS distributions are not uniformly MIT licensed. This
inventory reports the terms and provenance recorded by the source tree;
individual component license files and source notices remain authoritative.

| Component | Recorded source / license evidence |
| --- | --- |
| SDK API, core, FFI and original wrappers | Root MIT license; crate metadata records exceptions |
| `platform-pty` | [NOTICE](../../../crates/platform-pty/NOTICE): OpenAI Codex at `b8c2d29cc23b41fa7c7f5f5483e92fc71099635a`, Apache-2.0; Windows code originating from WezTerm, MIT |
| Android OpenMinis PTY bridge | OpenMinis `9cf3a855fecd27bb5735b84cacbd56852a3ab8dd`, GPL-3.0-only; [exact license](../../../native/android/OPENMINIS-LICENSE) |
| Android PRoot fork | OpenMinis/proot `8cf13e997cdc9472997aae19df8050c073c9a86c`, GPL-2.0-or-later; the pinned source's `COPYING` and source headers |
| talloc 2.4.2 | LGPL-3.0-or-later; [pinned source identity](../../android/native-pins.json) and retained source headers |
| Optional Android mksh | [Original NOTICE](../../../third_party/mksh/NOTICE), including its component-specific terms |
| Optional Android toybox | [Original LICENSE](../../../third_party/toybox/LICENSE), 0BSD text; per-file notices remain applicable |
| Android minijail | [Original LICENSE](../../../third_party/minijail/LICENSE) and [NOTICE](../../../third_party/minijail/NOTICE), BSD terms |
| Android libcap | [Original License](../../../third_party/libcap/License), BSD-3-Clause OR GPL-2.0-only, with explicit source exceptions |
| iOS Rust backend | [Crate metadata](../../../crates/mobile-linux-ios/Cargo.toml): GPL-3.0-only |
| iOS OpenMinis glue | Same pinned OpenMinis revision above; [exact GPL-3.0 license](../../../native/ios/upstream/openminis/LICENSE) |
| iSH, libapps and libarchive | Immutable revisions in [iOS sources.json](../../../native/ios/sources.json); native builds retain `LICENSE.md`, `LICENSE.IOS`, libapps `LICENSE` and libarchive `COPYING` from those revisions |
| Alpine/APK and additional rootfs tools | [Active toolchain pins](../../toolchains/runtime-pins.json) plus each release's `rootfs.spdx.json` and APK closure; multiple package licenses |

Android source hashes, PRoot archive identity and upstream license hashes are in
[Android native pins](../../android/native-pins.json). iOS source hashes and all
applied patch identities are in [iOS source pins](../../../native/ios/sources.json).
The Android native builder and iOS native builder fetch pinned sources into
caller-owned caches and build from them; an unrelated application checkout is
not a release input.

Distribute the applicable original license texts and copyright notices with the
corresponding native artifacts. Preserve immutable source references and the
actual patches used to build them; a top-level MIT file or generic Maven license
label is not a replacement. The iOS native bundle includes `licenses` and
`source-provenance` sidecars. Android native license inventory is recorded in its
`native-manifest.json`. Rootfs SBOM, manifest, source pins and archive digest must
refer to the actual distributed payload and ABI.

Historical imported Android pin/SBOM records are preserved byte-for-byte under
[docs/migration/legacy](../../migration/legacy/README.md). They do not describe
the active Alpine 3.24.2 toolchain-equipped release archive. The active evidence
is under [docs/mobile-linux/releases](../releases/README.md).

This notice was revised after extraction to describe the standalone SDK and its
current ownership. The imported original notice hash remains recorded in
`docs/toolchains/import-provenance.json`; the documentation update does not change
any third-party license text or source grant.
