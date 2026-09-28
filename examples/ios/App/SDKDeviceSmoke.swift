import Foundation

import MobileLinuxRuntime
import MobileLinuxRuntimeBindings

struct SDKDeviceSmoke {
    struct Failure: Error { let message: String }
    private func require(_ condition: Bool, _ message: String) throws { if !condition { throw Failure(message: message) } }
    private func archiveDigest(in manifest: [String: Any]) throws -> String {
        guard let digest = manifest["rootfs_zip_sha256"] as? String,
              digest.count == 64, digest.allSatisfy({ "0123456789abcdef".contains($0) }) else {
            throw Failure(message: "missing or invalid rootfs digest")
        }
        return digest
    }
    private func command(_ script: String, network: NetworkPolicyFfi = .disabled, memory: UInt32? = nil, timeout: UInt64? = 10_000) -> MobileLinuxCommandRequestFfi {
        MobileLinuxCommandRequestFfi(command: "/bin/sh", args: ["-c", script], cwd: nil, env: [], stdin: nil,
            timeoutMs: timeout, network: network,
            resourceLimits: ResourceLimitsFfi(maxCpuSeconds: nil, maxMemoryMb: memory, maxProcesses: nil, maxOpenFiles: nil), mounts: [])
    }

    func run(resources: URL) async throws {
        #if targetEnvironment(simulator)
        throw Failure(message: "Real embedded iSH execution requires an arm64 physical device")
        #else
        let manifest = try JSONSerialization.jsonObject(with: Data(contentsOf: resources.appendingPathComponent("manifest.json"))) as! [String: Any]
        let archiveHash = try archiveDigest(in: manifest)
        let files = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("sdk-device-validation-" + String(archiveHash.prefix(16)))
        let workspace = files.appendingPathComponent("workspace-one")
        try FileManager.default.createDirectory(at: workspace, withIntermediateDirectories: true)
        var config = RuntimeConfig(platform: .ios, managedRoot: files.appendingPathComponent("runtime").path,
            appSandboxRoot: files.path, abi: "aarch64", rootfsVersion: "alpine-3.24.2-sdk",
            archiveSha256: manifest["rootfs_zip_sha256"] as? String, nativeLibraryDir: nil,
            workspaceHostPath: workspace.path, stableWorkspaceId: "one", authorizationFile: nil,
            rootfsArchivePath: resources.appendingPathComponent("alpine-rootfs.zip").path,
            rootfsPatchPath: resources.appendingPathComponent("RootfsPatch.bundle").path,
            defaultMountPath: resources.appendingPathComponent("default_mount").path,
            protectedHostRoots: [], allowedMountRoots: [], allowedGuestRoots: [])
        let runtime = try MobileLinuxRuntime(configuration: config)
        let capability = await runtime.capability()
        try require(capability.available, "device kernel unavailable: \(capability.reason ?? "unknown")")
        print("SDK_SMOKE_PHASE install-boot")
        _ = try await runtime.boot()
        print("SDK_SMOKE_PHASE foreground")
        let first = try await runtime.handle.runCommand(request: command("printf sdk-ready"))
        try require(first.stdout == "sdk-ready", "foreground stdout mismatch")
        try require(first.exitCode == 0, "foreground exit code")
        try require(first.enforcement.networkPolicyEnforced, "network receipt missing")
        let nonzero = try await runtime.handle.runCommand(request: command("exit 7"))
        try require(nonzero.exitCode == 7, "guest wait status was not decoded")
        let defaults = try await runtime.handle.runCommand(request: command("printf %s \"${BROWSER-unset}\""))
        try require(defaults.stdout == "unset", "SDK injected a product browser command")
        var customEnvironment = command("printf '%s|%s' \"$HOME\" \"$BROWSER\"")
        customEnvironment.env = [MobileLinuxEnvEntryFfi(key: "HOME", value: "/tmp/sdk-home"), MobileLinuxEnvEntryFfi(key: "BROWSER", value: "/tmp/caller-browser")]
        let custom = try await runtime.handle.runCommand(request: customEnvironment)
        try require(custom.stdout == "/tmp/sdk-home|/tmp/caller-browser", "caller environment did not override native defaults")
        // Read tiny combined chunks after a process emits non-UTF8 bytes on both pipes.
        print("SDK_SMOKE_PHASE raw-binary")
        let raw = try await runtime.handle.openRawStdio(request: command(#"printf '\000\377\001\200\002\003\004'; printf '\376\000\372\012\013' >&2"#, timeout: nil))
        var stdout = Data(), stderr = Data(), closed = false
        for _ in 0..<1000 {
            let chunk = try await runtime.handle.readRawStdio(id: raw.id, maxBytes: 3)
            try require(chunk.stdout.count + chunk.stderr.count <= 3, "combined read limit violated")
            stdout.append(chunk.stdout); stderr.append(chunk.stderr)
            if chunk.closed { closed = true; break }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        try require(closed, "raw process did not reach EOF")
        try require(stdout == Data([0,255,1,128,2,3,4]), "binary stdout tail mismatch")
        try require(stderr == Data([254,0,250,10,11]), "binary stderr tail mismatch")
        try await runtime.handle.closeRawStdio(id: raw.id)
        try await runtime.handle.closeRawStdio(id: raw.id)
        print("SDK_SMOKE_PHASE raw-stdin-roundtrip")
        let echo = try await runtime.handle.openRawStdio(request: command("python3 -c 'import sys; data=sys.stdin.buffer.read(7); sys.stdout.buffer.write(data); sys.stderr.buffer.write(data)'", timeout: nil))
        let input = Data([0, 255, 128, 10, 13, 0, 42])
        try await runtime.handle.writeRawStdio(id: echo.id, data: input)
        var echoedOutput = Data(), echoedError = Data(), echoClosed = false
        for _ in 0..<1000 {
            let chunk = try await runtime.handle.readRawStdio(id: echo.id, maxBytes: 3)
            echoedOutput.append(chunk.stdout); echoedError.append(chunk.stderr)
            if chunk.closed { echoClosed = true; break }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        try require(echoClosed && echoedOutput == input && echoedError == input, "raw stdin/stdout/stderr binary roundtrip failed")
        try await runtime.handle.closeRawStdio(id: echo.id)
        // A guest that does not read stdin must still be cancellable while write waits.
        print("SDK_SMOKE_PHASE raw-cancel-write")
        let sleeper = try await runtime.handle.openRawStdio(request: command("sleep 60", timeout: nil))
        let writer = Task { try await runtime.handle.writeRawStdio(id: sleeper.id, data: Data(repeating: 0xAA, count: 1_048_576)) }
        try await Task.sleep(nanoseconds: 100_000_000)
        let before = Date()
        try await runtime.handle.closeRawStdio(id: sleeper.id)
        try require(Date().timeIntervalSince(before) < 4, "raw cancellation blocked by write")
        _ = await writer.result
        print("SDK_SMOKE_PHASE raw-resource-cleanup")
        let overflow = try await runtime.handle.openRawStdio(request: command("head -c 5242880 /dev/zero; sleep 60", timeout: nil))
        try await Task.sleep(nanoseconds: 500_000_000)
        var resourceFailure = false
        for _ in 0..<1000 {
            do {
                let chunk = try await runtime.handle.readRawStdio(id: overflow.id, maxBytes: 65_536)
                if chunk.closed { break }
            } catch {
                try require(String(describing: error).lowercased().contains("resource"), "untyped raw resource error: \(error)")
                resourceFailure = true
                break
            }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        try require(resourceFailure, "raw overflow did not report resource failure")
        do { try await runtime.handle.closeRawStdio(id: overflow.id) }
        catch { try require(String(describing: error).lowercased().contains("resource"), "raw cleanup error: \(error)") }
        try await runtime.handle.closeRawStdio(id: overflow.id)
        print("SDK_SMOKE_PHASE foreground-timeout")
        let timeout = try await runtime.handle.runCommand(request: command("sleep 60", timeout: 100))
        try require(timeout.timedOut, "foreground timeout missing")
        // Explicit network policy blocks internet sockets while local command still runs.
        print("SDK_SMOKE_PHASE network")
        let network = try await runtime.handle.runCommand(request: command("python3 -c 'import socket; socket.create_connection((\"1.1.1.1\",443),1)'"))
        try require(network.exitCode != 0 && (network.stderr.contains("Operation not permitted") || network.stderr.contains("Permission denied")), "disabled network request did not report permission denial: \(network.stderr)")
        print("SDK_SMOKE_PHASE memory")
        do {
            _ = try await runtime.handle.runCommand(request: command("python3 -c 'x=bytearray(128*1024*1024); import time; time.sleep(2)'", memory: 16))
            throw Failure(message: "memory overrun must return a typed resource error")
        } catch let failure as Failure { throw failure }
        catch { try require(String(describing: error).lowercased().contains("resource"), "untyped resource error: \(error)") }
        print("SDK_SMOKE_PHASE multiworkspace")
        let secondWorkspace = files.appendingPathComponent("workspace-two")
        try FileManager.default.createDirectory(at: secondWorkspace, withIntermediateDirectories: true)
        config.workspaceHostPath = secondWorkspace.path; config.stableWorkspaceId = "two"
        let second = try MobileLinuxRuntime(configuration: config)
        _ = try await second.boot()
        let secondResult = try await second.handle.runCommand(request: command("printf workspace-two"))
        try require(secondResult.stdout == "workspace-two", "second workspace failed")
        var conflict = config; conflict.managedRoot += "-conflict"
        do { _ = try MobileLinuxRuntime(configuration: conflict); throw Failure(message: "conflicting kernel root accepted") }
        catch is Failure { throw Failure(message: "conflicting kernel root accepted") }
        catch { }
        try await second.shutdown()
        print("SDK_SMOKE_PHASE shutdown")
        let foreground = Task { try await runtime.handle.runCommand(request: command("sleep 60", timeout: nil)) }
        try await Task.sleep(nanoseconds: 200_000_000)
        try await runtime.shutdown()
        _ = await foreground.result
        do { _ = try await runtime.handle.repairRootfs(); throw Failure(message: "booted kernel repair was permitted") }
        catch { try require(String(describing: error).lowercased().contains("restart"), "missing restart_required: \(error)") }
        #endif
    }
}
