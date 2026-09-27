import SwiftUI
import MobileLinuxRuntime
import MobileLinuxRuntimeBindings

@main
struct MobileLinuxSampleApp: App {
    var body: some Scene { WindowGroup { RuntimeView() } }
}
struct RuntimeView: View {
    @State private var status = "Checking standalone SDK…"
    @State private var runtime: MobileLinuxRuntime?
    var body: some View {
        Text(status).padding().task {
            if ProcessInfo.processInfo.arguments.contains("--sdk-device-smoke") {
                UIApplication.shared.isIdleTimerDisabled = true
                do {
                    try await SDKDeviceSmoke().run(resources: Bundle.main.resourceURL!.appendingPathComponent("DeviceResources"))
                    print("SDK_SMOKE_RESULT PASS")
                    exit(0)
                } catch {
                    print("SDK_SMOKE_RESULT FAIL \(error)")
                    exit(1)
                }
            }
            if NSClassFromString("XCTestCase") != nil { return }
            do {
                let files = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
                let configuration = RuntimeConfig(
                    platform: .ios, managedRoot: files.appendingPathComponent("linux").path,
                    appSandboxRoot: files.path, abi: "aarch64", rootfsVersion: "caller-provided-rootfs",
                    archiveSha256: nil, nativeLibraryDir: nil,
                    workspaceHostPath: files.appendingPathComponent("workspace").path,
                    stableWorkspaceId: "sample", authorizationFile: nil,
                    rootfsArchivePath: nil, rootfsPatchPath: nil, defaultMountPath: nil,
                    protectedHostRoots: [], allowedMountRoots: [], allowedGuestRoots: [])
                let sdk = try MobileLinuxRuntime(configuration: configuration)
                runtime = sdk
                let capability = await sdk.capability()
                status = capability.available ? "Runtime available. Supply an explicit verified rootfs archive before booting." : (capability.reason ?? "Runtime unavailable on this target")
            } catch { status = String(describing: error) }
        }
    }
}
