// swift-tools-version: 5.9
import PackageDescription

// Binary checksums refer to v0.1.0-rc.2 assets built from source 9e8e19a473728722183fcbfa0814398c1dd8a8ff.
let package = Package(
    name: "MobileLinuxRuntime",
    platforms: [.iOS("18.0")],
    products: [
        .library(name: "MobileLinuxRuntime", targets: ["MobileLinuxRuntime", "MobileLinuxRuntimeBindings"]),
        .library(name: "MobileLinuxNativeSupport", targets: ["MobileLinuxNativeSupportLink"]),
    ],
    targets: [
        .binaryTarget(name: "MobileLinuxRuntimeFFI", url: "https://github.com/lingxi-coder/mobile-linux-runtime/releases/download/v0.1.0-rc.2/MobileLinuxRuntimeFFI-0.1.0-rc.2.xcframework.zip", checksum: "35ed764afe495a0e7ad65e6b085554b9f1e70524534ef5dafd5972e0d1c61885"),
        .binaryTarget(name: "MobileLinuxNativeSupport", url: "https://github.com/lingxi-coder/mobile-linux-runtime/releases/download/v0.1.0-rc.2/MobileLinuxNativeSupport-0.1.0-rc.2.xcframework.zip", checksum: "180289e732b9a16ff2cb4e382a86cafe9ec0304a28419022f73d6d50eb67ec78"),
        .target(name: "MobileLinuxNativeSupportLink", dependencies: ["MobileLinuxNativeSupport"], path: "ios/NativeSupportLink", linkerSettings: [.linkedLibrary("sqlite3"), .linkedLibrary("resolv"), .linkedLibrary("z"), .linkedLibrary("c++")]),
        .target(name: "MobileLinuxRuntimeBindings", dependencies: ["MobileLinuxRuntimeFFI"], path: "ios/Bindings"),
        .target(name: "MobileLinuxRuntime", dependencies: ["MobileLinuxRuntimeBindings", "MobileLinuxNativeSupportLink"], path: "ios/SDK"),
    ]
)
