# Rust source license preservation

The extraction preserves the existing `MIT OR Apache-2.0` choice for LingXi and
Harness Rust source. The SDK workspace retains that declaration, and
`mobile-linux-ios` states it explicitly. The original root MIT `LICENSE` bytes
are unchanged (SHA-256
`4de8883488f651e3889ebca1b4e3b77e579cf33cbffae1a8b8fe673f06af261a`).
`LICENSE-APACHE` supplies the full Apache 2.0 text, including its appendix; it is
identical to the existing `crates/platform-pty/LICENSE-APACHE` (SHA-256
`a9040321c3712d8fd0b09cf52b17445de04a23a10165049ae187cd39e5c86be5`).
Both SDK license texts are included in the FFI source/artifact hash inventories
and binary/Swift package notices.

| Extracted source | Original revision and declaration |
| --- | --- |
| `mobile-linux-api`, `mobile-linux-core`, `mobile-linux-android` | Harness `fe876d368a6bd06988f936064f9a6e00edbe55a3`: `crates/platform-api`, `crates/platforms/common`, `crates/platforms/android` use `license.workspace = true`; workspace declares `MIT OR Apache-2.0` |
| Android libcap/minijail Rust build wrappers | Same Harness revision: `crates/platforms/android-libcap` and `crates/platforms/android-minijail` inherit the same workspace declaration; their native sources retain separate licenses |
| Android shellbin Rust build wrapper | LingXi `7f5619342236077e3aba4edbcb01f4fef042abb5`: `lingxi-code/platforms/android-shellbin` inherits the same workspace declaration |
| iOS Rust adapter | LingXi `7f5619342236077e3aba4edbcb01f4fef042abb5`: `lingxi-code/platforms/ios-ish-runtime` inherits `MIT OR Apache-2.0`; this is also the declaration at its introduction in `930f735e9d2c8fc4d0fa665db40234d85b63e34c` |
| `platform-pty` | Its original NOTICE identifies different files derived from Apache-2.0 OpenAI Codex and MIT WezTerm sources; the crate therefore records `Apache-2.0 AND MIT`, not a new dual-license choice |

The original iOS Rust source at the migration baseline has SHA-256
`c079cce2ca02e2007a29c70cc5490aad25e02217ee3897d14331bdbc5c70c6ed`.
It implements Rust runtime state and calls a separate C bridge; it is not an
import of the GPL-licensed native implementation. The extraction's initial
`GPL-3.0-only` Rust crate metadata was not supported by that source declaration
and was corrected to preserve the original choice.

OpenMinis, PRoot, talloc, iSH and other native components keep their original
license texts and notices. Their revisions and patches are identified by
`docs/android/native-pins.json` and `native/ios/sources.json`. Native distribution
attribution is separate from Rust crate metadata; neither replaces the other.
No broad GPL exception in a Rust dependency allowlist is needed to compensate
for the corrected iOS Rust metadata.
