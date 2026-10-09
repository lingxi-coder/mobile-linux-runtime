import CryptoKit
import Foundation
import XCTest


@_silgen_name("lx_ish_execution_context_next")
private func testLXISHExecutionContextNext() -> UInt64
@_silgen_name("lx_ish_execution_policy_register")
private func testLXISHExecutionPolicyRegister(_ context: UInt64, _ networkPolicy: Int32) -> Int32
@_silgen_name("lx_ish_execution_policy_unregister")
private func testLXISHExecutionPolicyUnregister(_ context: UInt64)
@_silgen_name("lx_ish_network_policy_for_context")
private func testLXISHNetworkPolicyForContext(_ context: UInt64) -> Int32

final class MobileLinuxNativeRegressionTests: XCTestCase {
    func testRawOutputIngressBoundsPendingCallbacksAndReportsOverflowOnce() {
        let ingress = LXISHRawOutputIngress(limit: 4)
        XCTAssertEqual(ingress.reserve(4), .accepted)
        ingress.release(2)
        XCTAssertEqual(ingress.reserve(2), .accepted)
        XCTAssertEqual(ingress.reserve(1), .overflow)
        ingress.release(4)
        XCTAssertEqual(ingress.reserve(1), .rejected)
    }

    func testRawReadDrainsBothBinaryTailsBeforeEOFWithinCombinedLimit() {
        let expectedOutput = Data([0, 255, 1, 128, 2, 3, 4])
        let expectedError = Data([254, 0, 250, 10, 11])
        var output = expectedOutput
        var error = expectedError
        var receivedOutput = Data()
        var receivedError = Data()
        for index in 0..<4 {
            let chunk = LXISHRawOutputChunk.drain(stdout: &output, stderr: &error, terminal: true, maxBytes: 3)
            XCTAssertLessThanOrEqual(chunk.stdout.count + chunk.stderr.count, 3)
            XCTAssertEqual(chunk.closed, index == 3)
            receivedOutput.append(chunk.stdout)
            receivedError.append(chunk.stderr)
        }
        XCTAssertEqual(receivedOutput, expectedOutput)
        XCTAssertEqual(receivedError, expectedError)
        XCTAssertFalse(LXISHRawOutputChunk.drain(stdout: &output, stderr: &error, terminal: false, maxBytes: 3).closed)
    }

    func testRawTerminalResourceFailureUsesStructuredBridgeError() throws {
        let payload = LXISHRawOutputChunk.failurePayload("buffer limit") as? [String: String]
        XCTAssertEqual(payload?["code"], "resource_limit_exceeded")
        XCTAssertEqual(payload?["message"], "buffer limit")
        XCTAssertTrue(LXISHRawOutputChunk.failurePayload(nil) is NSNull)
        let envelope = encodeEnvelope(ok: true, payload: ["closed": true, "error": LXISHRawOutputChunk.failurePayload("buffer limit")])
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(envelope.utf8)) as? [String: Any])
        let encodedError = try XCTUnwrap(object["error"] as? [String: String])
        XCTAssertEqual(encodedError["code"], "resource_limit_exceeded")
        XCTAssertEqual(encodedError["message"], "buffer limit")
    }

    func testExplicitCallerHomeAndBrowserSurviveDefaultEnvironment() {
        let environment = LXISHGuestEnvironment.merged(requestEnvironment: ["HOME": "/tmp/sdk-home", "BROWSER": "/tmp/caller-browser"], cwd: nil, stableWorkspaceId: "one")
        XCTAssertEqual(environment["HOME"], "/tmp/sdk-home")
        XCTAssertEqual(environment["BROWSER"], "/tmp/caller-browser")
        XCTAssertNil(LXISHGuestEnvironment.merged(requestEnvironment: [:], cwd: nil, stableWorkspaceId: "one")["BROWSER"])
    }

    private var temporaryRoot: URL!
    private var rootfsArchivePath: String?
    private var defaultMountPath: String?

    private enum StubFailure: LocalizedError {
        case boom

        var errorDescription: String? {
            switch self {
            case .boom:
                return "boom"
            }
        }
    }

    override func setUpWithError() throws {
        temporaryRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("ios-ish-runtime-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: temporaryRoot, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        rootfsArchivePath = nil
        defaultMountPath = nil
        if temporaryRoot != nil {
            try? FileManager.default.removeItem(at: temporaryRoot)
        }
    }







    func testExecutionPolicyRegistryFailsClosedWhenOwnershipIsLost() {
        let context = testLXISHExecutionContextNext()
        XCTAssertNotEqual(context, 0)
        XCTAssertEqual(testLXISHNetworkPolicyForContext(0), 0)
        XCTAssertEqual(testLXISHExecutionPolicyRegister(context, 2), 0)
        XCTAssertEqual(testLXISHNetworkPolicyForContext(context), 2)

        testLXISHExecutionPolicyUnregister(context)

        XCTAssertEqual(
            testLXISHNetworkPolicyForContext(context),
            1,
            "an orphaned non-zero execution context must fall back to Disabled"
        )
    }

    func testMemoryLimitObservationTracksPeakAndFirstExceededSample() {
        var observation = LXISHMemoryLimitObservation(limitBytes: 800)

        XCTAssertFalse(observation.recordWatchdogSample(residentBytes: 600))
        XCTAssertTrue(observation.recordWatchdogSample(residentBytes: 900))
        XCTAssertFalse(observation.recordWatchdogSample(residentBytes: 1_200))

        XCTAssertEqual(observation.observedResidentBytesAtExceed, 900)
        XCTAssertEqual(observation.peakResidentBytes, 1_200)
        XCTAssertEqual(
            observation.resourceLimitMessage,
            "resource_limit_exceeded: reason=memory_limit observed=900 peak=1200 limit=800 bytes"
        )
    }

    func testMemoryWarningProducesDistinctResourceLimitDiagnostic() {
        var observation = LXISHMemoryLimitObservation(limitBytes: 800)
        XCTAssertFalse(observation.recordWatchdogSample(residentBytes: 500))

        XCTAssertTrue(observation.recordMemoryWarning(residentBytes: 700))
        XCTAssertFalse(observation.recordMemoryWarning(residentBytes: 750))

        XCTAssertEqual(observation.observedResidentBytesAtExceed, 700)
        XCTAssertEqual(observation.peakResidentBytes, 750)
        XCTAssertEqual(
            observation.resourceLimitMessage,
            "resource_limit_exceeded: reason=memory_warning observed=700 peak=750 limit=800 bytes"
        )
    }

    func testMemoryWarningObserverCanBeCancelledAfterSynchronousRun() {
        let center = NotificationCenter()
        let name = Notification.Name("LXISHRuntimeBundleManifestTests.memory-warning")
        var notifications = 0
        let observer = LXISHMemoryWarningObserver(center: center, name: name) {
            notifications += 1
        }

        center.post(name: name, object: nil)
        XCTAssertEqual(notifications, 1)

        observer.cancel()
        center.post(name: name, object: nil)
        XCTAssertEqual(notifications, 1, "completed synchronous runs must release their observer")
    }

    /// Byte-for-byte the JSON serde writes for Rust's `NativeConfigPayload`
    /// (`lingxi-code/platforms/ios-ish-runtime/src/lib.rs`), which has no
    /// `rename_all`, so the wire keys are the Rust field names. Every
    /// config-bearing C-ABI call carries exactly this; if it stops decoding,
    /// the whole native runtime stops with a Foundation decoder message rather
    /// than anything that names the runtime. Decoded through THE bridge
    /// decoder (`LXISHBridgeJSON`) — the snake_case convention now lives at
    /// that chokepoint, not in per-struct `CodingKeys`.
    func testNativeConfigDecodesTheKeysRustActuallySends() throws {
        let wire = Data("""
        {
          "managed_root": "/managed",
          "workspace_host_path": "/workspace-host",
          "stable_workspace_id": "12345678-1234-4abc-8def-1234567890ab",
          "abi": "arm64",
          "rootfs_version": "3.24.1",
          "archive_sha256": null,
          "authorization_file": null
        }
        """.utf8)

        let config = try LXISHBridgeJSON.decoder().decode(LXISHNativeConfig.self, from: wire)

        XCTAssertEqual(config.managedRoot, "/managed")
        XCTAssertEqual(config.workspaceHostPath, "/workspace-host")
        XCTAssertEqual(config.stableWorkspaceId, "12345678-1234-4abc-8def-1234567890ab")
        XCTAssertEqual(config.abi, "arm64")
        XCTAssertEqual(config.rootfsVersion, "3.24.1")
        XCTAssertNil(config.archiveSha256)
        XCTAssertNil(config.authorizationFile)
    }

    /// camelCase is what the struct used to expect and what Rust never sends.
    /// Without this the fix could be silently undone by a decoder that accepts
    /// both, and the bug would come back with the same unreadable message.
    func testNativeConfigRejectsCamelCaseKeys() {
        let wire = Data("""
        {
          "managedRoot": "/managed",
          "workspaceHostPath": "/workspace-host",
          "stableWorkspaceId": "12345678-1234-4abc-8def-1234567890ab",
          "abi": "arm64",
          "rootfsVersion": "3.24.1"
        }
        """.utf8)

        XCTAssertThrowsError(
            try LXISHBridgeJSON.decoder().decode(LXISHNativeConfig.self, from: wire)
        )
    }

    /// WHY the bridge coders carry NO key strategy, pinned as a test: a
    /// chokepoint `convertFromSnakeCase` looks like the class fix for this
    /// bridge's repeated missing-CodingKeys outages, but Foundation's key
    /// strategies transform DICTIONARY keys too, and the requests embed
    /// environment maps. Real env vars — `GIT_CONFIG_COUNT`, `no_proxy`,
    /// `npm_config_userAgent` — must cross the bridge byte-verbatim. If a
    /// future pass re-adds a strategy, this fails before the terminal does.
    func testBridgeCodersPreserveEnvMapKeys() throws {
        let wire = Data("""
        {
          "command": "/bin/sh",
          "args": ["-c", "env"],
          "cwd": null,
          "env": {
            "GIT_CONFIG_COUNT": "1",
            "no_proxy": "localhost",
            "npm_config_userAgent": "npm/10",
            "TERM": "xterm-256color"
          },
          "stdin": null,
          "timeout_ms": 1000,
          "network": "allowed",
          "resource_limits": {
            "max_cpu_seconds": null,
            "max_memory_mb": 800,
            "max_processes": null,
            "max_open_files": null
          },
          "mounts": null
        }
        """.utf8)
        let request = try LXISHBridgeJSON.decoder().decode(LXISHRunRequest.self, from: wire)
        XCTAssertEqual(request.env["GIT_CONFIG_COUNT"], "1")
        XCTAssertEqual(request.env["no_proxy"], "localhost")
        XCTAssertEqual(request.env["npm_config_userAgent"], "npm/10")
        XCTAssertEqual(request.env["TERM"], "xterm-256color")
        XCTAssertEqual(request.timeoutMs, 1000, "struct fields still decode their snake keys")
        XCTAssertEqual(request.resourceLimits?.maxMemoryMb, 800)

        let encoded = try LXISHBridgeJSON.encoder().encode(request)
        let json = String(decoding: encoded, as: UTF8.self)
        for key in ["GIT_CONFIG_COUNT", "no_proxy", "npm_config_userAgent", "TERM"] {
            XCTAssertTrue(json.contains("\"\(key)\""), "env key must survive encode verbatim: \(key)")
        }
        XCTAssertTrue(json.contains("\"timeout_ms\""), "struct keys stay snake_case: \(json)")
        XCTAssertTrue(json.contains("\"resource_limits\""), "resource limits stay snake_case: \(json)")
        XCTAssertTrue(json.contains("\"max_memory_mb\":800"), "memory limit stays snake_case: \(json)")
    }

    /// iSH's `exit_hook` receives the kernel's wait-status encoding
    /// (`do_exit(status << 8)` on a normal exit; the low 7 bits carry a fatal
    /// signal). The bridge's ISHProcessExited observer — the piece that turns
    /// a user typing `exit` into a `pty_closed` event instead of a dead
    /// caret — must decode both shapes.
    func testGuestWaitStatusDecodesNormalExitsAndSignals() {
        // exit 0 / exit 3 → status << 8.
        XCTAssertEqual(LXISHGuestWaitStatus.decode(0).code, 0)
        XCTAssertNil(LXISHGuestWaitStatus.decode(0).detail)
        XCTAssertEqual(LXISHGuestWaitStatus.decode(3 << 8).code, 3)
        // SIGKILL(9) → shell convention 128 + signal, with a diagnosis.
        let killed = LXISHGuestWaitStatus.decode(9)
        XCTAssertEqual(killed.code, 137)
        XCTAssertEqual(killed.detail, "terminated by signal 9")
    }







    func testGuestEnvironmentPinsOnlyTheCurrentWorkspaceAsSafeDirectory() {
        let environment = LXISHGuestEnvironment.merged(
            requestEnvironment: [:],
            cwd: nil,
            stableWorkspaceId: "12345678-1234-4abc-8def-1234567890ab"
        )

        XCTAssertEqual(environment["HOME"], "/root")
        XCTAssertEqual(environment["PWD"], "/workspace/12345678-1234-4abc-8def-1234567890ab")
        XCTAssertEqual(environment["TMPDIR"], "/tmp")
        XCTAssertEqual(environment["PATH"], "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
        XCTAssertEqual(environment["XDG_CACHE_HOME"], "/root/.cache")
        XCTAssertEqual(environment["NPM_CONFIG_CACHE"], "/root/.npm")
        XCTAssertEqual(environment["PIP_CACHE_DIR"], "/root/.cache/pip")
        XCTAssertEqual(environment["SSL_CERT_FILE"], "/etc/ssl/cert.pem")
        XCTAssertEqual(environment["GIT_CONFIG_COUNT"], "1")
        XCTAssertEqual(environment["GIT_CONFIG_KEY_0"], "safe.directory")
        XCTAssertEqual(environment["GIT_CONFIG_VALUE_0"], "/workspace/12345678-1234-4abc-8def-1234567890ab")
        XCTAssertNil(environment["GIT_CONFIG_KEY_1"])
    }

    func testGuestEnvironmentOverridesCallerProvidedSafeDirectoryEscape() {
        let environment = LXISHGuestEnvironment.merged(
            requestEnvironment: [
                "GIT_CONFIG_COUNT": "2",
                "GIT_CONFIG_KEY_0": "safe.directory",
                "GIT_CONFIG_VALUE_0": "*"
            ],
            cwd: nil,
            stableWorkspaceId: "12345678-1234-4abc-8def-1234567890ab"
        )

        XCTAssertEqual(environment["GIT_CONFIG_COUNT"], "1")
        XCTAssertEqual(environment["GIT_CONFIG_KEY_0"], "safe.directory")
        XCTAssertEqual(environment["GIT_CONFIG_VALUE_0"], "/workspace/12345678-1234-4abc-8def-1234567890ab")
    }

    func testRuntimeMountPlannerPreservesPersistentHomeAndWorkspaceLayers() {
        let config = LXISHNativeConfig(
            managedRoot: temporaryRoot.appendingPathComponent("managed-root", isDirectory: true).path,
            workspaceHostPath: temporaryRoot.appendingPathComponent("workspace-host", isDirectory: true).path,
            stableWorkspaceId: "12345678-1234-4abc-8def-1234567890ab",
            abi: "arm64",
            rootfsVersion: "3.24.1",
            archiveSha256: String(repeating: "a", count: 64),
            authorizationFile: nil,
            rootfsArchivePath: rootfsArchivePath,
            defaultMountPath: defaultMountPath
        )

        let mounts = LXISHRuntimeMountPlanner.effectiveMounts(
            requestedMounts: [
                LXISHMountSpec(
                    hostPath: temporaryRoot.appendingPathComponent("ignored-root", isDirectory: true).path,
                    guestPath: "/root",
                    readOnly: false,
                    purpose: "external"
                ),
                LXISHMountSpec(
                    hostPath: temporaryRoot.appendingPathComponent("workspace-subdir", isDirectory: true).path,
                    guestPath: "/workspace/12345678-1234-4abc-8def-1234567890ab/src",
                    readOnly: false,
                    purpose: "external"
                ),
            ],
            config: config
        )

        XCTAssertEqual(mounts.map(\.guestPath), [
            "/root",
            "/workspace/12345678-1234-4abc-8def-1234567890ab",
            "/workspace/12345678-1234-4abc-8def-1234567890ab/src",
        ])
        XCTAssertEqual(mounts[0].hostPath, config.persistentHomeURL.path)
        XCTAssertEqual(
            mounts[1].hostPath,
            URL(fileURLWithPath: config.workspaceHostPath, isDirectory: true).standardizedFileURL.path
        )
    }

    func testRuntimeMountPlannerCanExcludeManagedHomeAndWorkspaceForIsolatedRequests() {
        let config = LXISHNativeConfig(
            managedRoot: temporaryRoot.appendingPathComponent("managed-root-isolated", isDirectory: true).path,
            workspaceHostPath: temporaryRoot.appendingPathComponent("workspace-host-isolated", isDirectory: true).path,
            stableWorkspaceId: "12345678-1234-4abc-8def-1234567890ab",
            abi: "arm64",
            rootfsVersion: "3.24.1",
            archiveSha256: String(repeating: "a", count: 64),
            authorizationFile: nil,
            rootfsArchivePath: rootfsArchivePath,
            defaultMountPath: defaultMountPath
        )

        let mounts = LXISHRuntimeMountPlanner.effectiveMounts(
            requestedMounts: [
                LXISHMountSpec(
                    hostPath: temporaryRoot.appendingPathComponent("build-root/../build-root", isDirectory: true).path,
                    guestPath: "/var/lingxi/project-mounts/abcd1234/store/project",
                    readOnly: false,
                    purpose: "external"
                )
            ],
            config: config,
            includeDefaultMounts: false
        )

        XCTAssertEqual(mounts.count, 1)
        XCTAssertEqual(mounts[0].guestPath, "/var/lingxi/project-mounts/abcd1234/store/project")
        XCTAssertEqual(
            mounts[0].hostPath,
            temporaryRoot.appendingPathComponent("build-root", isDirectory: true).path
        )
    }

    func testExecutionMountScopeRestoresPersistentMountsAfterIsolatedSuccess() throws {
        var runtimeMounts = [
            LXISHMountSpec(hostPath: "/host/home", guestPath: "/root", readOnly: false, purpose: "home"),
            LXISHMountSpec(
                hostPath: "/host/workspace",
                guestPath: "/workspace/12345678-1234-4abc-8def-1234567890ab",
                readOnly: false,
                purpose: "workspace"
            ),
        ]
        let requestedMounts = [
            LXISHMountSpec(
                hostPath: "/host/build",
                guestPath: "/var/lingxi/project-mounts/abcd1234/store/project",
                readOnly: false,
                purpose: "external"
            )
        ]
        var applied: [([String], Bool)] = []

        let result = try LXISHExecutionMountScope.withMounts(
            runtimeMounts: &runtimeMounts,
            requestedMounts: requestedMounts,
            includeDefaultMounts: false,
            apply: { mounts, includeDefaultMounts in
                applied.append((mounts.map(\.guestPath), includeDefaultMounts))
            },
            body: { "ok" }
        )

        XCTAssertEqual(result, "ok")
        XCTAssertEqual(runtimeMounts.map(\.guestPath), [
            "/root",
            "/workspace/12345678-1234-4abc-8def-1234567890ab",
        ])
        XCTAssertEqual(applied.count, 2)
        XCTAssertEqual(applied[0].0, ["/var/lingxi/project-mounts/abcd1234/store/project"])
        XCTAssertFalse(applied[0].1)
        XCTAssertEqual(applied[1].0, [
            "/root",
            "/workspace/12345678-1234-4abc-8def-1234567890ab",
        ])
        XCTAssertTrue(applied[1].1)
    }

    func testExecutionMountScopeRestoresPersistentMountsAfterIsolatedFailure() {
        var runtimeMounts = [
            LXISHMountSpec(hostPath: "/host/home", guestPath: "/root", readOnly: false, purpose: "home"),
            LXISHMountSpec(
                hostPath: "/host/workspace",
                guestPath: "/workspace/12345678-1234-4abc-8def-1234567890ab",
                readOnly: false,
                purpose: "workspace"
            ),
        ]
        let requestedMounts = [
            LXISHMountSpec(
                hostPath: "/host/build",
                guestPath: "/var/lingxi/project-mounts/abcd1234/store/project",
                readOnly: false,
                purpose: "external"
            )
        ]
        var applied: [([String], Bool)] = []

        XCTAssertThrowsError(
            try LXISHExecutionMountScope.withMounts(
                runtimeMounts: &runtimeMounts,
                requestedMounts: requestedMounts,
                includeDefaultMounts: false,
                apply: { mounts, includeDefaultMounts in
                    applied.append((mounts.map(\.guestPath), includeDefaultMounts))
                },
                body: { throw StubFailure.boom }
            )
        ) { error in
            XCTAssertEqual(error.localizedDescription, "boom")
        }

        XCTAssertEqual(runtimeMounts.map(\.guestPath), [
            "/root",
            "/workspace/12345678-1234-4abc-8def-1234567890ab",
        ])
        XCTAssertEqual(applied.count, 2)
        XCTAssertEqual(applied[0].0, ["/var/lingxi/project-mounts/abcd1234/store/project"])
        XCTAssertFalse(applied[0].1)
        XCTAssertEqual(applied[1].0, [
            "/root",
            "/workspace/12345678-1234-4abc-8def-1234567890ab",
        ])
        XCTAssertTrue(applied[1].1)
    }

    func testBootDiscardsLegacyPersistedRequestMounts() throws {
        let managedRoot = temporaryRoot.appendingPathComponent("managed-root-stale-mounts", isDirectory: true)
        try FileManager.default.createDirectory(at: managedRoot, withIntermediateDirectories: true)
        let config = LXISHNativeConfig(
            managedRoot: managedRoot.path,
            workspaceHostPath: temporaryRoot.appendingPathComponent("workspace-current", isDirectory: true).path,
            stableWorkspaceId: "12345678-1234-4abc-8def-1234567890ab",
            abi: "arm64",
            rootfsVersion: "3.24.1",
            archiveSha256: String(repeating: "a", count: 64),
            authorizationFile: nil,
            rootfsArchivePath: rootfsArchivePath,
            defaultMountPath: defaultMountPath
        )
        let staleMounts = [
            LXISHMountSpec(
                hostPath: "/private/var/containers/Bundle/Application/OLD/LingxiCode.app/app-runtime/node_modules",
                guestPath: "/opt/lingxi/app-runtime/node_modules",
                readOnly: true,
                purpose: "shared"
            ),
            LXISHMountSpec(
                hostPath: "/private/var/mobile/Containers/Data/Application/CURRENT/Library/Application Support/LingxiCode/apps/old-app/build/store",
                guestPath: "/var/lingxi/project-mounts/old-app/store/project",
                readOnly: false,
                purpose: "external"
            ),
        ]
        try LXISHBridgeJSON.encoder().encode(staleMounts).write(to: config.mountsCacheURL, options: .atomic)

        let restored = try LXISHNativeRootfsManager().mountsForBoot(for: config)

        XCTAssertTrue(restored.isEmpty, "boot must rebuild stable mounts from config and wait for the current request")
        XCTAssertFalse(
            FileManager.default.fileExists(atPath: config.mountsCacheURL.path),
            "the one-time legacy cache must not poison later boots"
        )
    }

    func testInstallVerifiesPinnedArchiveAndMigratesLegacyHomeIntoPersistentRoot() throws {
        let archive = try makeRootfsArchive(version: "3.24.1")
        let managedRoot = temporaryRoot.appendingPathComponent("managed-root", isDirectory: true)
        let defaultMount = temporaryRoot.appendingPathComponent("default_mount", isDirectory: true)
        try FileManager.default.createDirectory(at: defaultMount, withIntermediateDirectories: true)
        rootfsArchivePath = archive.url.path
        defaultMountPath = defaultMount.path

        let stableId = "12345678-1234-4abc-8def-1234567890ab"
        try seedExistingRootfs(at: managedRoot, stableWorkspaceId: stableId)

        let config = LXISHNativeConfig(
            managedRoot: managedRoot.path,
            workspaceHostPath: "/tmp/project",
            stableWorkspaceId: stableId,
            abi: "arm64",
            rootfsVersion: "3.24.1",
            archiveSha256: archive.sha256,
            authorizationFile: nil,
            rootfsArchivePath: rootfsArchivePath,
            defaultMountPath: defaultMountPath
        )
        let manager = LXISHNativeRootfsManager()

        let status = try manager.installIfNeeded(for: config)

        XCTAssertEqual(status.state, "ready")
        XCTAssertEqual(
            try String(contentsOf: config.rootfsURL.appendingPathComponent("etc/alpine-release"), encoding: .utf8),
            "3.24.1"
        )
        XCTAssertEqual(
            try String(contentsOf: config.persistentHomeURL.appendingPathComponent("notes.txt"), encoding: .utf8),
            "keep-me"
        )
        XCTAssertFalse(
            FileManager.default.fileExists(
                atPath: config.rootfsDataURL.appendingPathComponent("workspace/\(stableId)/hello.txt").path
            )
        )

        let drifted = LXISHNativeConfig(
            managedRoot: managedRoot.path,
            workspaceHostPath: "/tmp/project",
            stableWorkspaceId: stableId,
            abi: "arm64",
            rootfsVersion: "3.24.1",
            archiveSha256: String(repeating: "0", count: 64),
            authorizationFile: nil,
            rootfsArchivePath: rootfsArchivePath,
            defaultMountPath: defaultMountPath
        )
        XCTAssertEqual(manager.status(for: drifted).state, "corrupt")
    }

    func testInstallRejectsArchiveHashMismatch() throws {
        let archive = try makeRootfsArchive(version: "3.24.1")
        let managedRoot = temporaryRoot.appendingPathComponent("managed-root-mismatch", isDirectory: true)
        let defaultMount = temporaryRoot.appendingPathComponent("default_mount_mismatch", isDirectory: true)
        try FileManager.default.createDirectory(at: defaultMount, withIntermediateDirectories: true)
        rootfsArchivePath = archive.url.path
        defaultMountPath = defaultMount.path

        let config = LXISHNativeConfig(
            managedRoot: managedRoot.path,
            workspaceHostPath: "/tmp/project",
            stableWorkspaceId: "12345678-1234-4abc-8def-1234567890ab",
            abi: "arm64",
            rootfsVersion: "3.24.1",
            archiveSha256: String(repeating: "f", count: 64),
            authorizationFile: nil,
            rootfsArchivePath: rootfsArchivePath,
            defaultMountPath: defaultMountPath
        )

        XCTAssertThrowsError(try LXISHNativeRootfsManager().installIfNeeded(for: config))
    }

    func testInstallRejectsMissingArchiveHash() throws {
        let archive = try makeRootfsArchive(version: "3.24.1")
        let managedRoot = temporaryRoot.appendingPathComponent("managed-root-missing-hash", isDirectory: true)
        let defaultMount = temporaryRoot.appendingPathComponent("default_mount_missing_hash", isDirectory: true)
        try FileManager.default.createDirectory(at: defaultMount, withIntermediateDirectories: true)
        rootfsArchivePath = archive.url.path
        defaultMountPath = defaultMount.path

        let config = LXISHNativeConfig(
            managedRoot: managedRoot.path,
            workspaceHostPath: "/tmp/project",
            stableWorkspaceId: "12345678-1234-4abc-8def-1234567890ab",
            abi: "arm64",
            rootfsVersion: "3.24.1",
            archiveSha256: nil,
            authorizationFile: nil,
            rootfsArchivePath: rootfsArchivePath,
            defaultMountPath: defaultMountPath
        )

        XCTAssertThrowsError(try LXISHNativeRootfsManager().installIfNeeded(for: config))
    }

    private func seedExistingRootfs(at managedRoot: URL, stableWorkspaceId: String) throws {
        let rootfsURL = managedRoot.appendingPathComponent("alpine-rootfs", isDirectory: true)
        let dataURL = rootfsURL.appendingPathComponent("data", isDirectory: true)
        try FileManager.default.createDirectory(at: dataURL, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(
            at: rootfsURL.appendingPathComponent("etc", isDirectory: true),
            withIntermediateDirectories: true
        )
        try FileManager.default.createDirectory(at: dataURL.appendingPathComponent("root", isDirectory: true), withIntermediateDirectories: true)
        try FileManager.default.createDirectory(
            at: dataURL.appendingPathComponent("workspace/\(stableWorkspaceId)", isDirectory: true),
            withIntermediateDirectories: true
        )
        try "old".write(to: rootfsURL.appendingPathComponent("meta.db"), atomically: true, encoding: .utf8)
        try "aarch64".write(to: rootfsURL.appendingPathComponent(".arch"), atomically: true, encoding: .utf8)
        try "3.21.0".write(to: rootfsURL.appendingPathComponent("etc/alpine-release"), atomically: true, encoding: .utf8)
        try "keep-me".write(to: dataURL.appendingPathComponent("root/notes.txt"), atomically: true, encoding: .utf8)
        try "workspace-data".write(
            to: dataURL.appendingPathComponent("workspace/\(stableWorkspaceId)/hello.txt"),
            atomically: true,
            encoding: .utf8
        )
        try """
        {
          "abi": "arm64",
          "rootfs_version": "3.21.0",
          "archive_sha256": "old",
          "arch": "aarch64",
          "updated_at": "2026-08-04T00:00:00Z"
        }
        """.write(to: rootfsURL.appendingPathComponent("bridge-state.json"), atomically: true, encoding: .utf8)
    }

    private func makeRootfsArchive(version: String) throws -> (url: URL, sha256: String) {
        let archiveURL = temporaryRoot.appendingPathComponent("alpine-rootfs-\(version).zip")
        let entries = [
            ("alpine-rootfs/meta.db", Data("meta".utf8)),
            ("alpine-rootfs/etc/alpine-release", Data(version.utf8)),
            ("alpine-rootfs/usr/bin/node", Data("#!/bin/sh\nexit 0\n".utf8)),
        ]
        let data = try TestZipArchive.make(entries: entries)
        try data.write(to: archiveURL, options: .atomic)
        let sha256 = SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
        return (archiveURL, sha256)
    }

    // MARK: - Guest mount points

    func testGuestMountPointMapsAbsolutePathsIntoTheFakefsDataTree() throws {
        let dataRoot = URL(fileURLWithPath: "/managed/alpine-rootfs/data", isDirectory: true)

        // A bind mount needs a directory on BOTH sides. Only the host side was
        // created, so every project mount attached to nothing.
        let build = LXISHRuntimeMountPlanner.guestMountPointURL(
            for: "/var/lingxi/project-mounts/abcd1234/store/project",
            under: dataRoot
        )
        XCTAssertEqual(
            build?.standardizedFileURL.path,
            "/managed/alpine-rootfs/data/var/lingxi/project-mounts/abcd1234/store/project"
        )

        XCTAssertEqual(
            LXISHRuntimeMountPlanner.guestMountPointURL(for: "/root", under: dataRoot)?
                .standardizedFileURL.path,
            "/managed/alpine-rootfs/data/root"
        )
    }

    /// `fakefs_bind_mount` registers the path it binds and none of its
    /// parents, so the parents are the runtime's job — and getting the set
    /// wrong is invisible until a guest lookup walks through one of them.
    ///
    /// Keep the fixtures neutral here: the helper has to handle multiple
    /// unrelated bind roots deterministically.
    func testGuestMountParentsCoverEveryAncestorButNotTheMountPoint() {
        let mounts = [
            LXISHMountSpec(
                hostPath: "/host/cache/assets",
                guestPath: "/srv/assets/cache",
                readOnly: true,
                purpose: "cache"
            ),
            LXISHMountSpec(
                hostPath: "/host/data/apps/abcd1234/build/store",
                guestPath: "/var/lingxi/project-mounts/abcd1234/store/project",
                readOnly: false,
                purpose: "external"
            )
        ]

        XCTAssertEqual(
            LXISHRuntimeMountPlanner.guestMountParents(of: mounts),
            [
                "/srv",
                "/var",
                "/srv/assets",
                "/var/lingxi",
                "/var/lingxi/project-mounts",
                "/var/lingxi/project-mounts/abcd1234",
                "/var/lingxi/project-mounts/abcd1234/store"
            ],
            "shallowest first, deduplicated, and never the mount point itself"
        )

        // A single-component mount point has no parent to create: `/` is not
        // something to mkdir, and emitting it would make the guest helper
        // fail on every run.
        XCTAssertEqual(
            LXISHRuntimeMountPlanner.guestMountParents(of: [
                LXISHMountSpec(hostPath: "/h", guestPath: "/workspace", readOnly: false, purpose: "workspace")
            ]),
            []
        )
    }

    /// The Swift half of the cross-language pin on the guest-path atlas.
    /// `LXISHGuestPaths` twins Rust's `traits::mobile_linux::guest_paths`;
    /// the Rust side pins the SAME literals in
    /// `guest_paths::tests::atlas_atoms_are_pinned`. If either twin drifts,
    /// exactly one of the two pins goes red and names the divergence.


    func testGuestMountPointRefusesPathsThatWouldEscapeTheDataTree() throws {
        let dataRoot = URL(fileURLWithPath: "/managed/alpine-rootfs/data", isDirectory: true)
        // Creating these would mkdir outside the rootfs on the HOST, so they are
        // refused rather than normalised.
        for guestPath in ["../escape", "/var/../../escape", "/a/./b", "relative/path", "/", ""] {
            XCTAssertNil(
                LXISHRuntimeMountPlanner.guestMountPointURL(for: guestPath, under: dataRoot),
                "guest path \(guestPath) must not map to a directory"
            )
        }
}

private enum TestZipArchive {
    struct Entry {
        var path: String
        var data: Data
    }

    static func make(entries: [(String, Data)]) throws -> Data {
        try make(entries: entries.map { Entry(path: $0.0, data: $0.1) })
    }

    static func make(entries: [Entry]) throws -> Data {
        var archive = Data()
        var centralDirectory = Data()
        var offset: UInt32 = 0

        for entry in entries {
            let name = Data(entry.path.utf8)
            let crc = CRC32.checksum(for: entry.data)
            archive.appendLE(UInt32(0x04034b50))
            archive.appendLE(UInt16(20))
            archive.appendLE(UInt16(0))
            archive.appendLE(UInt16(0))
            archive.appendLE(UInt16(0))
            archive.appendLE(UInt16(0))
            archive.appendLE(crc)
            archive.appendLE(UInt32(entry.data.count))
            archive.appendLE(UInt32(entry.data.count))
            archive.appendLE(UInt16(name.count))
            archive.appendLE(UInt16(0))
            archive.append(name)
            archive.append(entry.data)

            centralDirectory.appendLE(UInt32(0x02014b50))
            centralDirectory.appendLE(UInt16(20))
            centralDirectory.appendLE(UInt16(20))
            centralDirectory.appendLE(UInt16(0))
            centralDirectory.appendLE(UInt16(0))
            centralDirectory.appendLE(UInt16(0))
            centralDirectory.appendLE(UInt16(0))
            centralDirectory.appendLE(crc)
            centralDirectory.appendLE(UInt32(entry.data.count))
            centralDirectory.appendLE(UInt32(entry.data.count))
            centralDirectory.appendLE(UInt16(name.count))
            centralDirectory.appendLE(UInt16(0))
            centralDirectory.appendLE(UInt16(0))
            centralDirectory.appendLE(UInt16(0))
            centralDirectory.appendLE(UInt16(0))
            centralDirectory.appendLE(UInt32(0))
            centralDirectory.appendLE(offset)
            centralDirectory.append(name)

            offset = UInt32(archive.count)
        }

        let centralDirectoryOffset = UInt32(archive.count)
        archive.append(centralDirectory)
        archive.appendLE(UInt32(0x06054b50))
        archive.appendLE(UInt16(0))
        archive.appendLE(UInt16(0))
        archive.appendLE(UInt16(entries.count))
        archive.appendLE(UInt16(entries.count))
        archive.appendLE(UInt32(centralDirectory.count))
        archive.appendLE(centralDirectoryOffset)
        archive.appendLE(UInt16(0))
        return archive
    }
}

private enum CRC32 {
    static func checksum(for data: Data) -> UInt32 {
        var crc: UInt32 = 0xffff_ffff
        for byte in data {
            crc ^= UInt32(byte)
            for _ in 0..<8 {
                let mask = (crc & 1) == 1 ? UInt32(0xedb8_8320) : 0
                crc = (crc >> 1) ^ mask
            }
        }
        return crc ^ 0xffff_ffff
    }
    }
}

private extension Data {
    mutating func appendLE(_ value: UInt16) {
        var value = value.littleEndian
        append(Data(bytes: &value, count: MemoryLayout<UInt16>.size))
    }

    mutating func appendLE(_ value: UInt32) {
        var value = value.littleEndian
        append(Data(bytes: &value, count: MemoryLayout<UInt32>.size))
    }

}
