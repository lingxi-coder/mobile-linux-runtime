// swift-tools-version: 5.9
import PackageDescription

// RC3 Release binaries use clean SDK source 6b033d41a2b6b736fda1e8f996f55a4b20bd885d.
let package = Package(
    name: "MobileLinuxRuntime",
    platforms: [.iOS("18.0")],
    products: [
        .library(name: "MobileLinuxRuntime", targets: ["MobileLinuxRuntime", "MobileLinuxRuntimeBindings"]),
        .library(name: "MobileLinuxNativeSupport", targets: ["MobileLinuxNativeSupportLink"]),
    ],
    targets: [
        .binaryTarget(name: "MobileLinuxRuntimeFFI", url: "https://github.com/lingxi-coder/mobile-linux-runtime/releases/download/v0.1.0-rc.3/MobileLinuxRuntimeFFI-0.1.0-rc.3.xcframework.zip", checksum: "7a9b7d7819bdc69536057699e1953b8cf4fd17d1e16df5feec8c974679285af2"),
        .binaryTarget(name: "MobileLinuxNativeSupport", url: "https://github.com/lingxi-coder/mobile-linux-runtime/releases/download/v0.1.0-rc.3/MobileLinuxNativeSupport-0.1.0-rc.3.xcframework.zip", checksum: "ad75d649565de57209827c9e8bf4f8bc7b8cc9371030f0b8b42d2ce634203264"),
        .target(name: "MobileLinuxNativeSupportLink", dependencies: ["MobileLinuxNativeSupport"], path: "ios/NativeSupportLink", linkerSettings: [.linkedLibrary("sqlite3"), .linkedLibrary("resolv"), .linkedLibrary("z"), .linkedLibrary("c++")]),
        .target(name: "MobileLinuxRuntimeBindings", dependencies: ["MobileLinuxRuntimeFFI"], path: "ios/Bindings"),
        .target(name: "MobileLinuxRuntime", dependencies: ["MobileLinuxRuntimeBindings", "MobileLinuxNativeSupportLink"], path: "ios/SDK"),
    ]
)
