// swift-tools-version: 5.9
import PackageDescription

// Stage actual artifacts with scripts/release/package-sdk.py before consuming locally.
let package = Package(
    name: "MobileLinuxRuntime",
    platforms: [.iOS("18.0")],
    products: [
        .library(name: "MobileLinuxRuntime", targets: ["MobileLinuxRuntime", "MobileLinuxRuntimeBindings"]),
        .library(name: "MobileLinuxNativeSupport", targets: ["MobileLinuxNativeSupportLink"]),
    ],
    targets: [
        .binaryTarget(name: "MobileLinuxRuntimeFFI", path: "Artifacts/MobileLinuxRuntimeFFI.xcframework"),
        .binaryTarget(name: "MobileLinuxNativeSupport", path: "Artifacts/MobileLinuxNativeSupport.xcframework"),
        .target(name: "MobileLinuxNativeSupportLink", dependencies: ["MobileLinuxNativeSupport"], path: "NativeSupportLink", linkerSettings: [.linkedLibrary("sqlite3"), .linkedLibrary("resolv"), .linkedLibrary("z"), .linkedLibrary("c++")]),
        .target(name: "MobileLinuxRuntimeBindings", dependencies: ["MobileLinuxRuntimeFFI"], path: "Bindings"),
        .target(name: "MobileLinuxRuntime", dependencies: ["MobileLinuxRuntimeBindings", "MobileLinuxNativeSupportLink"], path: "SDK"),
    ]
)
