# Mobile Linux Runtime

A standalone Linux runtime SDK for Android and iOS applications. It provides
command execution, streaming, background tasks, PTY and raw stdio, explicit
mount/network policy, and verified rootfs installation. Its Rust contracts and
platform backends have no dependency on Harness, an Agent, an LLM, or an
application's preferences or protocol.

| Target | Runtime | Supported application minimum | Distribution |
| --- | --- | --- | --- |
| Android arm64-v8a / x86_64 | PRoot | Android API 26 | Maven AARs or Rust source with native support |
| iOS arm64 device | iSH arm64 | iOS 18.0 | Swift package / XCFrameworks or Rust source with native support |
| iOS arm64 / x86_64 simulator | Unavailable backend | iOS 18.0 | Link and API integration checks; no guest execution |

Rust source builds use **Rust 1.94.0**. Optional Android legacy host-shell helpers
retain their API 29 requirement; the PRoot SDK runtime minimum is API 26.

Start with [SDK integration](docs/SDK-INTEGRATION.md) for prerequisites, build
commands, Gradle/SwiftPM setup, rootfs inputs, and runtime lifecycle. Runnable
consumer projects live in [examples/android](examples/android) and
[examples/ios](examples/ios). Applications already embedding Rust use the
native-support distribution to keep one copy of the runtime.

This repository supplies source and artifact builders. Binary availability and
validation belong to a specific release's checksums and `release-provenance.json`;
a successful source build is not a device acceptance result. The checked-in
[rootfs evidence](docs/mobile-linux/releases/README.md) also describes which
rootfs ABIs have actual payload evidence. Rootfs archives are separate caller
inputs and are not silently downloaded or bundled into the SDK.

Original SDK code carries the [MIT license](LICENSE), with explicit exceptions
in crate metadata and source notices. The full distributions include third-party
code under other terms, including GPL/LGPL components. See the
[component attribution](docs/mobile-linux/LICENSES/NOTICE.md),
[Android native pins](docs/android/native-pins.json),
[iOS source pins](native/ios/sources.json), and
[PTY notice](crates/platform-pty/NOTICE). The root license does not relicense those
components.
