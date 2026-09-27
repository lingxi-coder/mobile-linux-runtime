// swift-tools-version: 5.9
import PackageDescription

// Binary checksums refer to v0.1.0-rc.1 assets built from source 9315dc4b87680d165b966c057e6e51e7d2f5d767.
let package = Package(
    name: "MobileLinuxRuntime",
    platforms: [.iOS("18.0")],
    products: [
        .library(name: "MobileLinuxRuntime", targets: ["MobileLinuxRuntime", "MobileLinuxRuntimeBindings"]),
        .library(name: "MobileLinuxNativeSupport", targets: ["MobileLinuxNativeSupportLink"]),
    ],
    targets: [
        .binaryTarget(name: "MobileLinuxRuntimeFFI", url: "https://github.com/lingxi-coder/mobile-linux-runtime/releases/download/v0.1.0-rc.1/MobileLinuxRuntimeFFI-0.1.0.xcframework.zip", checksum: "fd665d43112f466df7d93673f632b8b64f8ce80907a966c6523d317930609679"),
        .binaryTarget(name: "MobileLinuxNativeSupport", url: "https://github.com/lingxi-coder/mobile-linux-runtime/releases/download/v0.1.0-rc.1/MobileLinuxNativeSupport-0.1.0.xcframework.zip", checksum: "c9abb8499f3e014ce8c3d90df86a0864794d00cb97b537150304b90e561e3256"),
        .target(name: "MobileLinuxNativeSupportLink", dependencies: ["MobileLinuxNativeSupport"], path: "ios/NativeSupportLink", linkerSettings: [.linkedLibrary("sqlite3"), .linkedLibrary("resolv"), .linkedLibrary("z"), .linkedLibrary("c++")]),
        .target(name: "MobileLinuxRuntimeBindings", dependencies: ["MobileLinuxRuntimeFFI"], path: "ios/Bindings"),
        .target(name: "MobileLinuxRuntime", dependencies: ["MobileLinuxRuntimeBindings", "MobileLinuxNativeSupportLink"], path: "ios/SDK"),
    ]
)
