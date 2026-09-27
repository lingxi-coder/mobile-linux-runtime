# Standalone SDK integration

The Rust API, backend and FFI crates have no Harness or application protocol
dependency. Kotlin and Swift bindings use the `mobile_linux_runtime` UniFFI
namespace. The application supplies paths, verified rootfs assets, mount policy,
authorization and lifecycle. The `io.lingxi.mobilelinux` Kotlin package is a
stable SDK namespace; it does not require the LingXi application.

## Choose one integration mode

| Application | Android artifacts | iOS products |
| --- | --- | --- |
| Kotlin or Swift application | `mobile-linux-runtime`, with transitive installer and native-support AARs | `MobileLinuxRuntime`, linking the FFI and native-support XCFrameworks |
| Application already embedding Rust | SDK Rust crates plus `mobile-linux-installer` and `mobile-linux-native-support` | SDK Rust crates plus `MobileLinuxNativeSupport` |

Keep exactly one Rust runtime in the process. Native-support-only distributions
contain platform glue/helpers, not another Rust library. In a Rust application,
select `mobile-linux-android` or `mobile-linux-ios` and the shared
`mobile-linux-api` / `mobile-linux-core` crates from the same Git revision. The
crate name `platform-pty` denotes the SDK-owned shared PTY implementation.

## Prerequisites and source checks

Use Rust **1.94.0** from `rust-toolchain.toml` and Python **3.12 or newer**. Android
builds use the Gradle wrapper, JDK 21, Android SDK platform 37, NDK
`27.2.12479018`, and `cargo-ndk` 4.1.2. Their application minimum is **API 26**.
iOS builds require macOS, Xcode with the iOS SDK, Meson and Ninja; the deployment
minimum is **iOS 18.0**. SwiftPM source wrappers use Swift tools 5.9.

Run commands from the repository root. Set `SDK_BUILD_DIR` to an absolute,
caller-owned directory outside this checkout; outputs and caches are explicit.
No sibling Harness or application checkout is needed.

```sh
: "${SDK_BUILD_DIR:?Set an absolute directory outside the source checkout}"
export CARGO_TARGET_DIR="$SDK_BUILD_DIR/rust"
cargo test --locked -p mobile-linux-api -p mobile-linux-core -p platform-pty
bash scripts/check-dependencies.sh
bash scripts/check-resource-contracts.sh
bash scripts/check-resource-tests.sh
```

Default tests use checked-in manifests and temporary fixtures. The optional
full rootfs installation regression needs real archive bytes supplied by the
caller; it has no fixed machine-local archive path:

```sh
: "${MOBILE_LINUX_TEST_ARCHIVE:?Set the verified release rootfs archive path}"
export MOBILE_LINUX_TEST_ARCHIVE
cargo test --locked -p mobile-linux-core --test producer_manifest \
  published_archive_stages_verifies_and_activates_real_tree -- --ignored
```

## Android: build and consume AARs

Set `ANDROID_NDK_HOME` to the pinned NDK installation, and install the Rust
Android targets and `cargo-ndk`. Build both supported ABIs:

```sh
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk --locked --version '=4.1.2'
bash scripts/build-android-native.sh \
  --ndk "$ANDROID_NDK_HOME" --android-api 26 \
  --output-dir "$SDK_BUILD_DIR/android-native" \
  --cache-dir "$SDK_BUILD_DIR/android-native-cache"
python3 scripts/build-ffi.py --platform android --release \
  --output-dir "$SDK_BUILD_DIR/android-ffi" \
  --target-dir "$SDK_BUILD_DIR/android-rust"
python3 scripts/publish-android.py \
  --native-artifacts "$SDK_BUILD_DIR/android-native" \
  --ffi-artifacts "$SDK_BUILD_DIR/android-ffi" \
  --maven-dir "$SDK_BUILD_DIR/maven" --build-dir "$SDK_BUILD_DIR/gradle" \
  --version 0.1.0
```

Publication requires a clean committed checkout. For local development only,
`--allow-dirty-validation` records an explicitly non-release publication. The
version above is the local build coordinate, not a claim that Maven Central has
a published release. Rust embeddings omit the FFI build and pass `--native-only`
to publication.

Add your Maven directory or published Maven URL to both `pluginManagement` and
`dependencyResolutionManagement` repositories in the consumer's
`settings.gradle.kts`. Keep Google, Maven Central and the Gradle plugin portal
where needed. In the application module:

```kotlin
plugins {
    id("io.github.lingxi-coder.mobile-linux") version "0.1.0"
}
dependencies {
    implementation("io.github.lingxi-coder:mobile-linux-runtime:0.1.0")
}
```

Use `minSdk = 26`, include `arm64-v8a` and `x86_64`, and set
`android:extractNativeLibs="true"` on the application manifest. The Gradle plugin
enables legacy JNI packaging and checks helper presence and ELF ABI in the final
APK. The supplied AAR build includes legacy host-shell helpers whose own floor
is API 29; those optional APIs do not lower that requirement.

For an existing Rust embedding, depend on the installer and native-support
coordinates instead and set `mobileLinuxPackaging=native-support` in
`gradle.properties`. Keep only the SDK's stable
`com.openminis.app.sandbox.PtyBridge` JNI class if the old host defined its own.
See [the complete Gradle consumer](../examples/android).

Before booting:

1. Obtain the archive, full manifest and SBOM from a trusted matching release.
   Keep the archive SHA-256 pinned by the application; the manifest is not a
   substitute for that trust decision.
2. Call `RootfsInstaller.stage` on an I/O worker with an app-private
   `managedRoot`, expected version/hash, manifest ABI (`arm64` or `x86_64`),
   archive filename, the unchanged manifest/SBOM JSON, and a `copyArchive`
   callback. It verifies the archive, immutable inventory, symlinks and SBOM,
   then publishes the staged directory and complete manifest atomically.
3. Create `MobileLinuxRuntime` with `RuntimeConfig`: platform `ANDROID`, the
   same managed root, absolute app sandbox root, Android ABI (`arm64-v8a` or
   `x86_64`), expected version/hash, and
   `applicationInfo.nativeLibraryDir`. Product preferences are not inferred.
4. Call `runtime.handle.repairRootfs()` to activate staging, then `runtime.boot()`.
   Use `runtime.execute(request)` or `runtime.handle` for streaming, PTY,
   background tasks and raw stdio; retain the runtime for those handles' lifetime.

[RealRuntimeSmokeTest](../examples/android/app/src/androidTest/java/io/lingxi/mobilelinux/sample/RealRuntimeSmokeTest.kt)
shows the complete installer, configuration and execution sequence with real
caller-staged assets. `MainActivity` demonstrates object creation only; it does
not install an image or prove guest execution.

## iOS: build and consume SwiftPM / XCFrameworks

Build native support and the FFI independently from the same source revision:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
bash scripts/build-ios-xcframework.sh \
  --output "$SDK_BUILD_DIR/ios-native" --cache "$SDK_BUILD_DIR/ios-cache" \
  --kind native-support --configuration Release
python3 scripts/build-ffi.py --platform ios --release \
  --output-dir "$SDK_BUILD_DIR/ios-ffi" --target-dir "$SDK_BUILD_DIR/ios-rust"
```

`MobileLinuxRuntimeFFI.xcframework` contains Rust.
`MobileLinuxNativeSupport.xcframework` contains the iSH/native bridge. The
simulator slices allow linking and API checks but report the guest backend as
unavailable; guest execution requires an arm64 iOS device.

For local SwiftPM use, stage these two actual frameworks under `ios/Artifacts`
and `MobileLinuxRuntimeBindings.swift` from the FFI output's `swift` directory
under `ios/Bindings`. Add the local `ios` package to the app and select
`MobileLinuxRuntime`. The `ios/Package.swift` manifest references binary targets;
a clean checkout needs those build outputs before SwiftPM can resolve it.
An existing Rust host selects only `MobileLinuxNativeSupport`. Its product adds
the required native system linker libraries.

For a binary release, use the generated root Swift package or released package
ZIP, whose binary URLs and checksums are generated from the actual artifacts.
Do not replace checksum values with examples. The package builder described
below creates that entry point after binaries are available.

The iOS rootfs input is an iSH fakefs ZIP, not the Android/Linux tarball. Convert
an attested aarch64 rootfs with the pinned SDK tool:

```sh
: "${ROOTFS_ARCHIVE:?Set the aarch64 archive path}"
: "${ROOTFS_ARCHIVE_SHA256:?Set its pinned SHA-256}"
bash scripts/prepare-ios-rootfs.sh \
  --archive "$ROOTFS_ARCHIVE" --expected-archive-sha256 "$ROOTFS_ARCHIVE_SHA256" \
  --profile toolchain --native-output "$SDK_BUILD_DIR/ios-native/native" \
  --output "$SDK_BUILD_DIR/ios-rootfs" --cache "$SDK_BUILD_DIR/ios-cache"
```

The output contains the fakefs ZIP, its manifest, `RootfsPatch.bundle`,
`libvdso.so.elf`, and the default mount resources. The application distributes
these resources and passes their final absolute paths explicitly. Use the
converted ZIP's SHA-256 for the iOS runtime configuration, not the source
tarball's SHA-256.

Import `MobileLinuxRuntime` and `MobileLinuxRuntimeBindings`, construct
`RuntimeConfig`, then call `MobileLinuxRuntime(configuration:)`. Supply platform
`ios`, app-private absolute `managedRoot` / `appSandboxRoot`, ABI `arm64`, rootfs
version/hash, `workspaceHostPath`, a persistent `stableWorkspaceId`,
`rootfsArchivePath`, `rootfsPatchPath`, and `defaultMountPath`. The caller also
chooses `protectedHostRoots`, `allowedMountRoots`, `allowedGuestRoots`, and any
authorization file. See [the Swift consumer](../examples/ios) and its device
smoke for complete configuration. After installation/boot, use `runtime.handle`
for command, PTY and raw-stdio operations. Pass byte buffers without text
conversion when exact stdio content matters.

The iSH kernel lives for the app process lifetime. Logical shutdown closes
runtime tasks. Changing an initialized kernel's configuration or repairing or
resetting it can return `RestartRequired`; the host owns the app-restart UX.

## Release identity and attribution

`package-sdk.py` accepts a Maven directory, the two real XCFrameworks and matching
Swift bindings, plus version, source revision, output directory and release base
URL. It verifies source identity, writes ZIPs, checksums and
`release-provenance.json`, and emits a Swift package containing the exact binary
checksums. Release inputs must come from a clean committed checkout and release
build profiles. `--allow-dirty-validation` creates validation-only outputs which
cannot be promoted by changing their label.

After source commit A and binary publication, `--write-root-package` writes the
repository-root SwiftPM manifest and matching binding source for wrapper commit
B. Record the binary source revision and later package revision separately.
Archive presence, simulator linking and host tests do not establish real-device
acceptance; attach the corresponding device evidence to each release.

The root MIT license covers original SDK source only. Full Android distributions
include OpenMinis, PRoot and talloc under their separate GPL/LGPL terms; iOS
includes the GPL-3.0-only Rust backend and the pinned native sources. Preserve
the applicable license/notice files, source revisions and patch provenance.
[Component attribution](mobile-linux/LICENSES/NOTICE.md) identifies these inputs;
rootfs SBOMs carry their own package licenses. The SDK's independence from an
application does not change those source licenses.
