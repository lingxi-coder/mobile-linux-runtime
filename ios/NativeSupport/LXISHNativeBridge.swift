//
//  LXISHNativeBridge.swift
//  LingxiCode
//
//  Swift implementation for the Rust-facing C ABI declared in
//  `LXISHNativeBridge.h`.
//

import Foundation
import Network
import ObjectiveC.runtime
import UIKit

@_silgen_name("mlr_ish_classes_linked")
private func mlrISHClassesLinked() -> Bool

@_silgen_name("lx_ish_execution_policy_version")
private func lxISHExecutionPolicyVersion() -> UInt32
@_silgen_name("lx_ish_execution_context_next")
private func lxISHExecutionContextNext() -> UInt64
@_silgen_name("lx_ish_execution_policy_register")
private func lxISHExecutionPolicyRegister(_ context: UInt64, _ networkPolicy: Int32) -> Int32
@_silgen_name("lx_ish_execution_policy_unregister")
private func lxISHExecutionPolicyUnregister(_ context: UInt64)
@_silgen_name("lx_ish_execution_resident_bytes")
private func lxISHExecutionResidentBytes(_ context: UInt64) -> UInt64
@_silgen_name("lx_ish_execution_context_active")
private func lxISHExecutionContextActive(_ context: UInt64) -> Int32

/// The ONE pair of coders for this bridge. Rust (serde) is uniformly
/// snake_case on both directions of the C ABI; the convention is enforced by
/// per-struct `CodingKeys` plus the round-trip tests that pin them.
///
/// DELIBERATELY NO `keyDecodingStrategy`/`keyEncodingStrategy` here, although
/// a chokepoint strategy looks like the obvious class fix for the
/// missing-CodingKeys outages this bridge has shipped: Foundation's key
/// strategies transform DICTIONARY keys too, and these payloads embed
/// environment maps (`env: [String: String]`). Real env vars like
/// `GIT_CONFIG_COUNT`, `no_proxy`, and `npm_config_userAgent` would be
/// silently rewritten in flight — a worse outage than the one being
/// prevented, and no case-based heuristic survives `npm_config_userAgent`.
/// `LXISHRuntimeBundleManifestTests.testBridgeCodersPreserveEnvMapKeys` pins
/// this decision; if a future pass re-adds a strategy, that test fails first.
enum LXISHBridgeJSON {
    static func decoder() -> JSONDecoder {
        JSONDecoder()
    }

    static func encoder() -> JSONEncoder {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return encoder
    }
}

struct LXISHMountSpec: Codable, Hashable {
    var hostPath: String
    var guestPath: String
    var readOnly: Bool
    var purpose: String


    enum CodingKeys: String, CodingKey {
        case hostPath = "host_path"
        case guestPath = "guest_path"
        case readOnly = "read_only"
        case purpose
    }
}

struct LXISHResourceLimits: Codable {
    var maxCpuSeconds: UInt32?
    var maxMemoryMb: UInt32?
    var maxProcesses: UInt32?
    var maxOpenFiles: UInt32?

    enum CodingKeys: String, CodingKey {
        case maxCpuSeconds = "max_cpu_seconds"
        case maxMemoryMb = "max_memory_mb"
        case maxProcesses = "max_processes"
        case maxOpenFiles = "max_open_files"
    }
}

struct LXISHRunRequest: Codable {
    var command: String
    var args: [String]
    var cwd: String?
    var env: [String: String]
    var stdin: String?
    var timeoutMs: UInt64?
    var network: String
    var resourceLimits: LXISHResourceLimits?
    var mounts: [LXISHMountSpec]?
    var includeDefaultMounts: Bool?

    enum CodingKeys: String, CodingKey {
        case command
        case args
        case cwd
        case env
        case stdin
        case timeoutMs = "timeout_ms"
        case network
        case resourceLimits = "resource_limits"
        case mounts
        case includeDefaultMounts = "include_default_mounts"
    }
}

struct LXISHPtyOpenRequest: Codable {
    var command: String
    var args: [String]
    var cwd: String?
    var env: [String: String]
    var cols: UInt16
    var rows: UInt16
    var mounts: [LXISHMountSpec]?
}

struct LXISHPtyWriteRequest: Codable {
    var sessionId: String
    var dataBase64: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case dataBase64 = "data_base64"
    }
}

struct LXISHPtyResizeRequest: Codable {
    var sessionId: String
    var cols: UInt16
    var rows: UInt16

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case cols
        case rows
    }
}

struct LXISHPtyCloseRequest: Codable {
    var sessionId: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
    }
}

struct LXISHPollRequest: Codable {
    var afterSequence: UInt64?
    var limit: UInt32?


    enum CodingKeys: String, CodingKey {
        case afterSequence = "after_sequence"
        case limit
    }
}

struct LXISHBackgroundProcessRequest: Codable {
    var processId: String


    enum CodingKeys: String, CodingKey {
        case processId = "process_id"
    }
}

struct LXISHBackgroundPollRequest: Codable {
    var processId: String
    var afterSequence: UInt64?
    var limit: UInt32?


    enum CodingKeys: String, CodingKey {
        case processId = "process_id"
        case afterSequence = "after_sequence"
        case limit
    }
}

struct LXISHLoopbackProbeRequest: Codable {
    var port: UInt16
    var timeoutMs: UInt32


    enum CodingKeys: String, CodingKey {
        case port
        case timeoutMs = "timeout_ms"
    }
}

private struct LXISHErrorPayload: Codable {
    var code: String
    var message: String
}

private struct LXISHRunResultPayload: Codable {
    var stdout: String
    var stderr: String
    var exitCode: Int
    var timedOut: Bool
    var cancelled: Bool
    var durationSeconds: Double
    var networkPolicyEnforced: Bool
    var memoryLimitEnforced: Bool


    enum CodingKeys: String, CodingKey {
        case stdout
        case stderr
        case exitCode = "exit_code"
        case timedOut = "timed_out"
        case cancelled
        case durationSeconds = "duration_seconds"
        case networkPolicyEnforced = "network_policy_enforced"
        case memoryLimitEnforced = "memory_limit_enforced"
    }
}

private struct LXISHPtyEventPayload: Codable {
    var sequence: UInt64
    var sessionId: String
    var kind: String
    var dataBase64: String?
    var detail: String?
    /// Real guest exit code, present only on a shell SELF-exit `pty_closed`
    /// (produced by the ISHProcessExited observer). Bridge-initiated closes
    /// omit it; the Rust reader then reports code 0 as before.
    var exitCode: Int32?


    enum CodingKeys: String, CodingKey {
        case sequence
        case sessionId = "session_id"
        case kind
        case dataBase64 = "data_base64"
        case detail
        case exitCode = "exit_code"
    }
}

private struct LXISHBackgroundEventPayload: Codable {
    var sequence: UInt64
    var processId: String
    var kind: String
    var line: String?
    var dataBase64: String?
    var exitCode: Int?
    var cancelled: Bool?
    var detail: String?
    var result: LXISHRunResultPayload? = nil
    var error: LXISHErrorPayload? = nil


    enum CodingKeys: String, CodingKey {
        case sequence
        case processId = "process_id"
        case kind
        case line
        case dataBase64 = "data_base64"
        case exitCode = "exit_code"
        case cancelled
        case detail
        case result
        case error
    }
}

private struct LXISHRawStdioOpenRequest: Codable {
    var command: String
    var args: [String]
    var cwd: String?
    var env: [String: String]
    var network: String
    var resourceLimits: LXISHResourceLimits?
    var mounts: [LXISHMountSpec]?

    enum CodingKeys: String, CodingKey {
        case command, args, cwd, env, network, mounts
        case resourceLimits = "resource_limits"
    }
}

private struct LXISHRawStdioRequest: Codable {
    var sessionId: String
    var dataBase64: String?
    var maxBytes: UInt32?

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case dataBase64 = "data_base64"
        case maxBytes = "max_bytes"
    }
}

private struct LXISHShellExecutionResultBox {
    var exitCode: Int
    var errorCode: Int
    var stdoutText: String
    var stderrText: String
    var durationSeconds: Double
}

struct LXISHGuestEnvironment {
    static func merged(
        requestEnvironment: [String: String],
        cwd: String?,
        stableWorkspaceId: String
    ) -> [String: String] {
        var environment = requestEnvironment
        let workspaceGuestPath = LXISHGuestPaths.workspace(stableWorkspaceId)
        let defaultHome = LXISHGuestPaths.home
        let defaultPath = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
        let resolvedCwd = cwd?.isEmpty == false ? cwd! : workspaceGuestPath
        let defaults: [String: String] = [
            "HOME": defaultHome,
            "PWD": resolvedCwd,
            "PATH": defaultPath,
            "TMPDIR": "/tmp",
            "TMP": "/tmp",
            "TEMP": "/tmp",
            "XDG_CACHE_HOME": "\(defaultHome)/.cache",
            "NPM_CONFIG_CACHE": "\(defaultHome)/.npm",
            "npm_config_cache": "\(defaultHome)/.npm",
            "PIP_CACHE_DIR": "\(defaultHome)/.cache/pip",
            "SSL_CERT_FILE": "/etc/ssl/cert.pem",
            "SSL_CERT_DIR": "/etc/ssl/certs",
            "GIT_SSL_CAINFO": "/etc/ssl/cert.pem"
        ]
        for (key, value) in defaults where environment[key] == nil {
            environment[key] = value
        }
        environment["GIT_CONFIG_COUNT"] = "1"
        environment["GIT_CONFIG_KEY_0"] = "safe.directory"
        environment["GIT_CONFIG_VALUE_0"] = workspaceGuestPath
        return environment
    }
}

struct LXISHRuntimeMountPlanner {
    /// Where an absolute guest path lands inside the fakefs data tree, or nil if
    /// it is not an in-tree absolute path. Traversal components are rejected
    /// rather than normalised: the cost of being wrong here is creating a
    /// directory outside the rootfs on the host.
    static func guestMountPointURL(for guestPath: String, under dataRoot: URL) -> URL? {
        guard guestPath.hasPrefix("/") else { return nil }
        let components = guestPath.split(separator: "/").map(String.init)
        guard !components.isEmpty,
              !components.contains(".."),
              !components.contains(".")
        else {
            return nil
        }
        let candidate = components.reduce(dataRoot) {
            $0.appendingPathComponent($1, isDirectory: true)
        }
        guard candidate.standardizedFileURL.path.hasPrefix(dataRoot.path + "/") else {
            return nil
        }
        return candidate
    }

    /// Every ancestor directory a mount point hangs from, shallowest first and
    /// deduplicated across mounts.
    ///
    /// The mount point itself is excluded: `fakefs_bind_mount` creates and
    /// registers that one. Only what is above it is nobody's job.
    static func guestMountParents(of mounts: [LXISHMountSpec]) -> [String] {
        var seen = Set<String>()
        var ordered: [String] = []
        for mount in mounts {
            let components = mount.guestPath.split(separator: "/").map(String.init)
            guard components.count > 1,
                  !components.contains(".."),
                  !components.contains(".")
            else {
                continue
            }
            // dropLast() leaves the parents only.
            var path = ""
            for component in components.dropLast() {
                path += "/" + component
                if seen.insert(path).inserted {
                    ordered.append(path)
                }
            }
        }
        return ordered.sorted(by: shallowestFirst)
    }

    /// Parents before children, and total: `sorted(by:)` is not stable, so
    /// depth alone would leave siblings in an order that varies run to run.
    static func shallowestFirst(_ lhs: String, _ rhs: String) -> Bool {
        let lhsDepth = lhs.split(separator: "/").count
        let rhsDepth = rhs.split(separator: "/").count
        return lhsDepth == rhsDepth ? lhs < rhs : lhsDepth < rhsDepth
    }

    static func effectiveMounts(
        requestedMounts: [LXISHMountSpec],
        config: LXISHNativeConfig,
        includeDefaultMounts: Bool = true
    ) -> [LXISHMountSpec] {
        guard includeDefaultMounts else {
            return requestedMounts.map { mount in
                LXISHMountSpec(
                    hostPath: URL(fileURLWithPath: mount.hostPath, isDirectory: true)
                        .standardizedFileURL
                        .path,
                    guestPath: mount.guestPath,
                    readOnly: mount.readOnly,
                    purpose: mount.purpose
                )
            }
        }
        let workspaceGuestPath = LXISHGuestPaths.workspace(config.stableWorkspaceId)
        var mounts: [LXISHMountSpec] = [
            LXISHMountSpec(
                hostPath: config.persistentHomeURL.path,
                guestPath: LXISHGuestPaths.home,
                readOnly: false,
                purpose: "home"
            )
        ]
        if !config.workspaceHostPath.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            mounts.append(
                LXISHMountSpec(
                    hostPath: URL(fileURLWithPath: config.workspaceHostPath, isDirectory: true)
                        .standardizedFileURL
                        .path,
                    guestPath: workspaceGuestPath,
                    readOnly: false,
                    purpose: "workspace"
                )
            )
        }
        for mount in requestedMounts where mount.guestPath != LXISHGuestPaths.home && mount.guestPath != workspaceGuestPath {
            mounts.append(
                LXISHMountSpec(
                    hostPath: URL(fileURLWithPath: mount.hostPath, isDirectory: true)
                        .standardizedFileURL
                        .path,
                    guestPath: mount.guestPath,
                    readOnly: mount.readOnly,
                    purpose: mount.purpose
                )
            )
        }
        return mounts
    }
}

enum LXISHExecutionMountScope {
    static func withMounts<T>(
        runtimeMounts: inout [LXISHMountSpec],
        requestedMounts: [LXISHMountSpec]?,
        includeDefaultMounts: Bool,
        apply: ([LXISHMountSpec], Bool) throws -> Void,
        body: () throws -> T
    ) throws -> T {
        if includeDefaultMounts {
            runtimeMounts = requestedMounts ?? runtimeMounts
            try apply(runtimeMounts, true)
            return try body()
        }

        let persistentMounts = runtimeMounts
        runtimeMounts = requestedMounts ?? []

        do {
            try apply(runtimeMounts, false)
            let result = try body()
            runtimeMounts = persistentMounts
            do {
                try apply(persistentMounts, true)
            } catch {
                throw LXISHBridgeError.io(
                    "isolated run completed but failed to restore runtime mounts: \(error.localizedDescription)"
                )
            }
            return result
        } catch {
            let executionError = error
            runtimeMounts = persistentMounts
            do {
                try apply(persistentMounts, true)
            } catch {
                throw LXISHBridgeError.io(
                    "\(executionError.localizedDescription); failed to restore runtime mounts after isolated run: \(error.localizedDescription)"
                )
            }
            throw executionError
        }
    }
}

private enum LXISHBridgeError: LocalizedError {
    case restartRequired(String)
    case invalidRequest(String)
    case networkPolicyUnavailable(String)
    case unavailable(String)
    case resourceLimitExceeded(String)
    case io(String)

    var errorDescription: String? {
        switch self {
        case let .restartRequired(message),
             let .invalidRequest(message),
             let .networkPolicyUnavailable(message),
             let .unavailable(message),
             let .resourceLimitExceeded(message),
             let .io(message):
            return message
        }
    }

    var code: String {
        switch self {
        case .restartRequired: return "restart_required"
        case .invalidRequest: return "invalid_request"
        case .networkPolicyUnavailable: return "network_policy_unavailable"
        case .unavailable: return "unavailable"
        case .resourceLimitExceeded: return "resource_limit_exceeded"
        case .io: return "io"
        }
    }
}

private final class LXISHKernelRuntimeBridge {
    private var kernelObject: AnyObject?
    private var mountedGuestPaths: [String] = []
    private var interactiveShellOpen = false

    static func isDeviceBridgeAvailable() -> Bool {
        #if targetEnvironment(simulator)
        return false
        #else
        return mlrISHClassesLinked() && NSClassFromString("ISHKernel") != nil
        #endif
    }

    static func availabilityReason() -> String {
        #if targetEnvironment(simulator)
        return "iSH bridge is disabled in the iOS Simulator"
        #else
        return isDeviceBridgeAvailable() ? "" : "OpenMinis ISHKernel is not linked into the app target"
        #endif
    }

    static func refreshDnsIfAvailable() {
        #if !targetEnvironment(simulator)
        do {
            try LXISHKernelRuntimeBridge().refreshDns()
        } catch {
            return
        }
        #endif
    }

    func boot(withRootPath rootPath: String) throws {
        let kernel = try resolveKernel()
        let selector = NSSelectorFromString("bootWithRootPath:")
        guard let method = class_getInstanceMethod(type(of: kernel), selector) else {
            throw LXISHBridgeError.unavailable("ISHKernel is missing bootWithRootPath:")
        }
        typealias Fn = @convention(c) (AnyObject, Selector, NSString) -> Int32
        let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
        let result = fn(kernel, selector, rootPath as NSString)
        guard result >= 0 else {
            throw LXISHBridgeError.unavailable("ISHKernel boot failed: \(result)")
        }
    }

    func configureMounts(_ mounts: [[String: Any]]) throws {
        let kernel = try resolveKernel()
        let unmountSelector = NSSelectorFromString("bindUnmountPath:")
        if let method = class_getInstanceMethod(type(of: kernel), unmountSelector) {
            typealias Fn = @convention(c) (AnyObject, Selector, NSString) -> Int32
            let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
            for guestPath in mountedGuestPaths.reversed() {
                _ = fn(kernel, unmountSelector, guestPath as NSString)
            }
        }
        mountedGuestPaths.removeAll()

        let mountSelector = NSSelectorFromString("bindMountPath:toHostPath:readOnly:")
        guard let method = class_getInstanceMethod(type(of: kernel), mountSelector) else {
            throw LXISHBridgeError.unavailable("ISHKernel is missing bindMountPath:toHostPath:readOnly:")
        }
        typealias Fn = @convention(c) (AnyObject, Selector, NSString, NSString, ObjCBool) -> Int32
        let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
        for mount in mounts {
            guard let guestPath = mount["guest_path"] as? String,
                  let hostPath = mount["host_path"] as? String
            else {
                throw LXISHBridgeError.invalidRequest("Mount entries require guest_path and host_path")
            }
            let readOnly = (mount["read_only"] as? Bool) ?? false
            let result = fn(kernel, mountSelector, guestPath as NSString, hostPath as NSString, ObjCBool(readOnly))
            guard result >= 0 else {
                throw LXISHBridgeError.unavailable("bind mount failed for \(guestPath) -> \(hostPath) (\(result))")
            }
            mountedGuestPaths.append(guestPath)
        }
    }

    func openInteractiveShell(
        withCommand command: [String],
        cols: UInt16,
        rows: UInt16,
        sink: @escaping (Data) -> Void
    ) throws {
        let kernel = try resolveKernel()
        let setSinkSelector = NSSelectorFromString("setOutputCallback:")
        if let method = class_getInstanceMethod(type(of: kernel), setSinkSelector) {
            typealias Fn = @convention(c) (AnyObject, Selector, AnyObject?) -> Void
            let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
            let block: @convention(block) (Data) -> Void = sink
            fn(kernel, setSinkSelector, unsafeBitCast(block, to: AnyObject.self))
        }
        try resizeColumns(cols, rows: rows)

        let executeSelector = NSSelectorFromString("executeCommand:")
        guard let method = class_getInstanceMethod(type(of: kernel), executeSelector) else {
            throw LXISHBridgeError.unavailable("ISHKernel is missing executeCommand:")
        }
        typealias Fn = @convention(c) (AnyObject, Selector, NSArray) -> Int32
        let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
        let result = fn(kernel, executeSelector, command as NSArray)
        guard result >= 0 else {
            throw LXISHBridgeError.unavailable("interactive shell launch failed: \(result)")
        }
        interactiveShellOpen = true
    }

    func writeInputData(_ data: Data) throws {
        let kernel = try resolveKernel()
        guard interactiveShellOpen else {
            throw LXISHBridgeError.invalidRequest("interactive shell is not open")
        }
        let selector = NSSelectorFromString("sendInput:")
        guard let method = class_getInstanceMethod(type(of: kernel), selector) else {
            throw LXISHBridgeError.unavailable("ISHKernel is missing sendInput:")
        }
        typealias Fn = @convention(c) (AnyObject, Selector, NSData) -> Void
        let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
        fn(kernel, selector, data as NSData)
    }

    func resizeColumns(_ cols: UInt16, rows: UInt16) throws {
        let kernel = try resolveKernel()
        let selector = NSSelectorFromString("setTerminalSize:rows:")
        guard let method = class_getInstanceMethod(type(of: kernel), selector) else {
            throw LXISHBridgeError.unavailable("ISHKernel is missing setTerminalSize:rows:")
        }
        typealias Fn = @convention(c) (AnyObject, Selector, Int32, Int32) -> Void
        let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
        fn(kernel, selector, Int32(cols), Int32(rows))
    }

    func refreshDns() throws {
        let kernel = try resolveKernel()
        let selector = NSSelectorFromString("refreshDns")
        guard let method = class_getInstanceMethod(type(of: kernel), selector) else {
            throw LXISHBridgeError.unavailable("ISHKernel is missing refreshDns")
        }
        typealias Fn = @convention(c) (AnyObject, Selector) -> Void
        let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
        fn(kernel, selector)
    }

    func closeInteractiveShell() throws {
        if interactiveShellOpen {
            try writeInputData(Data("exit\n".utf8))
        }
        interactiveShellOpen = false
    }

    /// Flag-only release for a shell that ALREADY died in the guest (observed
    /// via ISHProcessExited). Unlike [`closeInteractiveShell`], this must not
    /// write `exit\n` — there is no shell left to read it, and the write would
    /// land in the next session's input if one races in.
    func markInteractiveShellClosed() {
        interactiveShellOpen = false
    }

    private func resolveKernel() throws -> AnyObject {
        if let kernelObject {
            return kernelObject
        }
        guard let kernelClass = NSClassFromString("ISHKernel") else {
            throw LXISHBridgeError.unavailable(Self.availabilityReason())
        }
        let selector = NSSelectorFromString("shared")
        guard let method = class_getClassMethod(kernelClass, selector) else {
            throw LXISHBridgeError.unavailable("ISHKernel is missing shared")
        }
        typealias Fn = @convention(c) (AnyClass, Selector) -> AnyObject
        let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
        let kernel = fn(kernelClass, selector)
        kernelObject = kernel
        return kernel
    }
}

private final class LXISHDNSRefreshMonitor {
    static let shared = LXISHDNSRefreshMonitor()

    private let lock = NSLock()
    private var monitor: NWPathMonitor?
    private var started = false

    func startIfNeeded() {
        #if targetEnvironment(simulator)
        return
        #else
        lock.lock()
        defer { lock.unlock() }
        guard !started else { return }
        let monitor = NWPathMonitor()
        monitor.pathUpdateHandler = { [weak self] path in
            self?.handlePathUpdate(path)
        }
        monitor.start(queue: DispatchQueue(label: "org.mobile-linux.ish-native.dns-monitor"))
        self.monitor = monitor
        started = true
        #endif
    }

    /// NWPathMonitor only fires when the path actually changes, so refreshing on
    /// every satisfied update is not a busy loop — and it is the only policy
    /// that is correct here. Deduplicating on the interface-*type* set missed
    /// the most common real case: moving between two Wi-Fi networks keeps both
    /// the type and the `en0` interface identical while the resolvers behind
    /// them change completely, and NWPath exposes nothing that distinguishes
    /// them. Requiring a previous signature was wrong for the same reason in
    /// the other direction — it discarded the first update, which is the one
    /// that lands just after the runtime boots.
    private func handlePathUpdate(_ path: NWPath) {
        guard path.status == .satisfied else { return }
        LXISHKernelRuntimeBridge.refreshDnsIfAvailable()
    }
}

struct LXISHMemoryLimitObservation: Sendable {
    private enum Trigger: String, Sendable {
        case memoryLimit = "memory_limit"
        case memoryWarning = "memory_warning"
    }

    let limitBytes: UInt64
    private(set) var observedResidentBytesAtExceed: UInt64?
    private(set) var peakResidentBytes: UInt64 = 0
    private var trigger: Trigger?

    init(limitBytes: UInt64) {
        self.limitBytes = limitBytes
    }

    @discardableResult
    mutating func recordWatchdogSample(residentBytes: UInt64) -> Bool {
        peakResidentBytes = max(peakResidentBytes, residentBytes)
        guard trigger == nil, residentBytes > limitBytes else { return false }
        trigger = .memoryLimit
        observedResidentBytesAtExceed = residentBytes
        return true
    }

    @discardableResult
    mutating func recordMemoryWarning(residentBytes: UInt64) -> Bool {
        peakResidentBytes = max(peakResidentBytes, residentBytes)
        guard trigger == nil else { return false }
        trigger = .memoryWarning
        observedResidentBytesAtExceed = residentBytes
        return true
    }

    var exceeded: Bool { trigger != nil }

    var resourceLimitMessage: String? {
        guard let trigger, let observedResidentBytesAtExceed else { return nil }
        return "resource_limit_exceeded: reason=\(trigger.rawValue) "
            + "observed=\(observedResidentBytesAtExceed) "
            + "peak=\(peakResidentBytes) limit=\(limitBytes) bytes"
    }
}

final class LXISHMemoryWarningObserver {
    private let center: NotificationCenter
    private let lock = NSLock()
    private var token: NSObjectProtocol?

    init(
        center: NotificationCenter = .default,
        name: Notification.Name = UIApplication.didReceiveMemoryWarningNotification,
        onWarning: @escaping () -> Void
    ) {
        self.center = center
        token = center.addObserver(forName: name, object: nil, queue: nil) { _ in
            onWarning()
        }
    }

    func cancel() {
        lock.lock()
        let token = self.token
        self.token = nil
        lock.unlock()
        if let token {
            center.removeObserver(token)
        }
    }

    deinit {
        cancel()
    }
}

private struct LXISHExecutionPolicyOutcome {
    let observation: LXISHMemoryLimitObservation?

    var resourceLimitExceeded: Bool { observation?.exceeded == true }
    var resourceLimitMessage: String {
        if let message = observation?.resourceLimitMessage { return message }
        let observed = observation?.observedResidentBytesAtExceed ?? 0
        let peak = observation?.peakResidentBytes ?? observed
        let limit = observation?.limitBytes ?? 0
        return "resource_limit_exceeded: reason=unknown "
            + "observed=\(observed) peak=\(peak) limit=\(limit) bytes"
    }
}

private final class LXISHExecutionPolicyLease: @unchecked Sendable {
    private let context: UInt64
    private let memoryLimitBytes: UInt64?
    private let lock = NSLock()
    private var timer: DispatchSourceTimer?
    private var finished = false
    private var released = false
    private var memoryObservation: LXISHMemoryLimitObservation?

    init(networkPolicy: Int32, memoryLimitBytes: UInt64?) throws {
        guard lxISHExecutionPolicyVersion() == 1 else {
            if networkPolicy != 0 {
                throw LXISHBridgeError.networkPolicyUnavailable(
                    "iSH network-policy hook is unavailable or has an unsupported version"
                )
            }
            if memoryLimitBytes != nil {
                throw LXISHBridgeError.resourceLimitExceeded(
                    "iSH memory-limit hook is unavailable or has an unsupported version"
                )
            }
            throw LXISHBridgeError.unavailable(
                "iSH execution-context hook is unavailable or has an unsupported version"
            )
        }
        let context = lxISHExecutionContextNext()
        guard context != 0,
              lxISHExecutionPolicyRegister(context, networkPolicy) == 0
        else {
            if networkPolicy != 0 {
                throw LXISHBridgeError.networkPolicyUnavailable(
                    "iSH execution-policy registry is full"
                )
            }
            if memoryLimitBytes != nil {
                throw LXISHBridgeError.resourceLimitExceeded(
                    "iSH execution-policy registry is full"
                )
            }
            throw LXISHBridgeError.unavailable("iSH execution-policy registry is full")
        }
        self.context = context
        self.memoryLimitBytes = memoryLimitBytes
        memoryObservation = memoryLimitBytes.map(LXISHMemoryLimitObservation.init(limitBytes:))
    }

    var fsContext: UInt64 { context }

    func startWatchdog(onExceeded: @escaping @Sendable () -> Void) {
        guard memoryLimitBytes != nil else { return }
        let timer = DispatchSource.makeTimerSource(
            queue: DispatchQueue.global(qos: .utility)
        )
        timer.schedule(deadline: .now() + .milliseconds(250), repeating: .milliseconds(250))
        timer.setEventHandler { [weak self] in
            guard let self else { return }
            self.lock.lock()
            let shouldTerminate: Bool
            if !self.finished, !self.released, var observation = self.memoryObservation {
                let residentBytes = lxISHExecutionResidentBytes(self.context)
                shouldTerminate = observation.recordWatchdogSample(residentBytes: residentBytes)
                self.memoryObservation = observation
            } else {
                shouldTerminate = false
            }
            self.lock.unlock()
            if shouldTerminate {
                onExceeded()
            }
        }
        lock.lock()
        if finished {
            lock.unlock()
            timer.cancel()
            return
        }
        self.timer = timer
        lock.unlock()
        timer.resume()
    }

    @discardableResult
    func finish() -> LXISHExecutionPolicyOutcome {
        lock.lock()
        if finished {
            let result = LXISHExecutionPolicyOutcome(observation: memoryObservation)
            lock.unlock()
            return result
        }
        finished = true
        let result = LXISHExecutionPolicyOutcome(observation: memoryObservation)
        lock.unlock()
        releaseWhenInactive()
        return result
    }

    /// Reads the latest diagnostics without shortening the policy lifetime.
    /// The timeout path uses this before its existing three-second termination
    /// grace so an unrelated timeout keeps the same cleanup semantics.
    func currentOutcome() -> LXISHExecutionPolicyOutcome {
        lock.lock()
        let result = LXISHExecutionPolicyOutcome(observation: memoryObservation)
        lock.unlock()
        return result
    }

    /// Marks a system memory warning as a resource-limit failure for a
    /// synchronous build. The caller terminates the guest process group only
    /// when this returns true, so repeated notifications cannot race multiple
    /// TERM/KILL escalations.
    @discardableResult
    func recordMemoryWarning() -> Bool {
        guard memoryLimitBytes != nil else { return false }
        lock.lock()
        let shouldTerminate: Bool
        if !finished, !released, var observation = memoryObservation {
            let residentBytes = lxISHExecutionResidentBytes(context)
            shouldTerminate = observation.recordMemoryWarning(residentBytes: residentBytes)
            memoryObservation = observation
        } else {
            shouldTerminate = false
        }
        lock.unlock()
        return shouldTerminate
    }

    private func releaseWhenInactive() {
        if lxISHExecutionContextActive(context) != 0 {
            DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + .milliseconds(250)) { [self] in
                releaseWhenInactive()
            }
            return
        }
        lock.lock()
        if released {
            lock.unlock()
            return
        }
        released = true
        let timer = self.timer
        self.timer = nil
        lock.unlock()
        timer?.cancel()
        lxISHExecutionPolicyUnregister(context)
    }

    func finishAfterTerminationGrace() {
        DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + .seconds(3)) { [self] in
            _ = finish()
        }
    }

    deinit {
        if !released {
            lxISHExecutionPolicyUnregister(context)
        }
    }
}

private final class LXISHBoundedOutput {
    private let limit: Int
    private let lock = NSLock()
    private var stdout = Data()
    private var stderr = Data()

    init(limit: Int = 64 * 1024) {
        self.limit = limit
    }

    func append(_ line: String, isStdErr: Bool) {
        let bytes = Data((line + "\n").utf8)
        lock.lock()
        if isStdErr {
            append(bytes, to: &stderr)
        } else {
            append(bytes, to: &stdout)
        }
        lock.unlock()
    }

    func snapshot() -> (stdout: String, stderr: String) {
        lock.lock()
        let captured = (stdout, stderr)
        lock.unlock()
        return (
            String(decoding: captured.0, as: UTF8.self),
            String(decoding: captured.1, as: UTF8.self)
        )
    }

    private func append(_ bytes: Data, to buffer: inout Data) {
        buffer.append(bytes)
        if buffer.count > limit {
            buffer.removeFirst(buffer.count - limit)
        }
    }
}

private final class LXISHShellExecutorRuntimeBridge {
    static func isDeviceBridgeAvailable() -> Bool {
        #if targetEnvironment(simulator)
        return false
        #else
        return mlrISHClassesLinked() && NSClassFromString("ISHShellExecutor") != nil
        #endif
    }

    static func availabilityReason() -> String {
        #if targetEnvironment(simulator)
        return "iSH shell executor is disabled in the iOS Simulator"
        #else
        return isDeviceBridgeAvailable() ? "" : "OpenMinis ISHShellExecutor is not linked into the app target"
        #endif
    }

    func runExecutable(
        _ executable: String,
        arguments: [String],
        environment: [String: String],
        stdin: String?,
        cwd: String?,
        timeout: Double,
        networkPolicy: Int32,
        memoryLimitBytes: UInt64?
    ) throws -> LXISHShellExecutionResultBox {
        guard let executorClass = NSClassFromString("ISHShellExecutor") else {
            throw LXISHBridgeError.unavailable(Self.availabilityReason())
        }
        let selector = NSSelectorFromString(
            "executeExecutable:arguments:environment:stdinData:fsContext:lineCallback:completion:"
        )
        guard let method = class_getClassMethod(executorClass, selector) else {
            let message = "ISHShellExecutor is missing its fsContext execution entry point"
            if networkPolicy != 0 {
                throw LXISHBridgeError.networkPolicyUnavailable(message)
            }
            if memoryLimitBytes != nil {
                throw LXISHBridgeError.resourceLimitExceeded(message)
            }
            throw LXISHBridgeError.unavailable(message)
        }
        if memoryLimitBytes != nil,
           class_getClassMethod(executorClass, NSSelectorFromString("killProcessGroup:")) == nil
        {
            throw LXISHBridgeError.resourceLimitExceeded(
                "ISHShellExecutor cannot terminate an over-limit guest process group"
            )
        }
        let policyLease = try LXISHExecutionPolicyLease(
            networkPolicy: networkPolicy,
            memoryLimitBytes: memoryLimitBytes
        )

        let launch: (String, [String])
        if let cwd, !cwd.isEmpty {
            launch = (
                "/bin/sh",
                ["-c", "cd \"$1\" && shift && exec \"$@\"", "lingxi-run", cwd, executable] + arguments
            )
        } else {
            launch = (executable, arguments)
        }

        let semaphore = DispatchSemaphore(value: 0)
        let resultLock = NSLock()
        let streamedOutput = LXISHBoundedOutput()
        var completedResult: AnyObject?
        typealias LineBlock = @convention(block) (NSString, Bool) -> Void
        let lineBlock: LineBlock = { line, isStdErr in
            streamedOutput.append(line as String, isStdErr: isStdErr)
        }
        typealias CompletionBlock = @convention(block) (AnyObject) -> Void
        let completion: CompletionBlock = { result in
            resultLock.lock()
            completedResult = result
            resultLock.unlock()
            semaphore.signal()
        }
        typealias Fn = @convention(c) (
            AnyClass,
            Selector,
            NSString,
            NSArray,
            NSDictionary,
            NSData?,
            UInt64,
            AnyObject?,
            AnyObject
        ) -> Int32
        let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
        let stdinData = stdin.map { Data($0.utf8) } as NSData?
        let pid = fn(
            executorClass,
            selector,
            launch.0 as NSString,
            launch.1 as NSArray,
            environment as NSDictionary,
            stdinData,
            policyLease.fsContext,
            lineBlock as AnyObject,
            completion as AnyObject
        )
        guard pid >= 0 else {
            _ = policyLease.finish()
            throw LXISHBridgeError.unavailable("ISHShellExecutor failed to launch process: \(pid)")
        }
        // Only bounded synchronous commands (the build/export path) respond
        // to process-wide memory pressure. Long-lived Full runtimes use
        // `spawnExecutable` and intentionally do not install this observer.
        let memoryWarningObserver: LXISHMemoryWarningObserver? = if memoryLimitBytes != nil {
            LXISHMemoryWarningObserver { [weak self] in
                guard policyLease.recordMemoryWarning() else { return }
                self?.killProcessGroup(pid, executorClass: executorClass)
            }
        } else {
            nil
        }
        defer { memoryWarningObserver?.cancel() }
        policyLease.startWatchdog { [weak self] in
            self?.killProcessGroup(pid, executorClass: executorClass)
        }

        let waitResult: DispatchTimeoutResult
        if timeout > 0 {
            waitResult = semaphore.wait(timeout: .now() + timeout)
        } else {
            semaphore.wait()
            waitResult = .success
        }
        if waitResult == .timedOut {
            killProcessGroup(pid, executorClass: executorClass)
            let policyOutcome = policyLease.currentOutcome()
            if policyOutcome.resourceLimitExceeded {
                _ = policyLease.finish()
                let captured = streamedOutput.snapshot()
                return LXISHShellExecutionResultBox(
                    exitCode: -1,
                    errorCode: -5,
                    stdoutText: captured.stdout,
                    stderrText: policyOutcome.resourceLimitMessage,
                    durationSeconds: timeout
                )
            }
            // killProcessGroup escalates from TERM to KILL asynchronously.
            // Keep the registry entry alive until no surviving descendant can
            // regain unrestricted socket access during that grace period.
            policyLease.finishAfterTerminationGrace()
            let captured = streamedOutput.snapshot()
            let timeoutError = captured.stderr.isEmpty
                ? "command timed out"
                : captured.stderr + "command timed out\n"
            return LXISHShellExecutionResultBox(
                exitCode: -1,
                errorCode: -3,
                stdoutText: captured.stdout,
                stderrText: timeoutError,
                durationSeconds: timeout
            )
        }

        resultLock.lock()
        let resultObject = completedResult
        resultLock.unlock()
        guard let resultObject else {
            _ = policyLease.finish()
            throw LXISHBridgeError.unavailable("ISHShellExecutor completed without a result")
        }
        let policyOutcome = policyLease.finish()
        return LXISHShellExecutionResultBox(
            exitCode: intValue(from: resultObject, selector: "exitCode"),
            errorCode: policyOutcome.resourceLimitExceeded ? -5 : intValue(from: resultObject, selector: "error"),
            stdoutText: stringValue(from: resultObject, selector: "output"),
            stderrText: policyOutcome.resourceLimitExceeded
                ? policyOutcome.resourceLimitMessage
                : stringValue(from: resultObject, selector: "errorOutput"),
            durationSeconds: doubleValue(from: resultObject, selector: "duration")
        )
    }

    func spawnExecutable(
        _ executable: String,
        arguments: [String],
        environment: [String: String],
        stdin: String?,
        cwd: String?,
        networkPolicy: Int32,
        memoryLimitBytes: UInt64?,
        lineSink: @escaping (String, Bool) -> Void,
        completion: @escaping (LXISHShellExecutionResultBox) -> Void
    ) throws -> Int32 {
        guard let executorClass = NSClassFromString("ISHShellExecutor") else {
            throw LXISHBridgeError.unavailable(Self.availabilityReason())
        }
        let selector = NSSelectorFromString(
            "executeExecutable:arguments:environment:stdinData:fsContext:lineCallback:completion:"
        )
        guard let method = class_getClassMethod(executorClass, selector) else {
            let message = "ISHShellExecutor is missing its fsContext execution entry point"
            if networkPolicy != 0 {
                throw LXISHBridgeError.networkPolicyUnavailable(message)
            }
            if memoryLimitBytes != nil {
                throw LXISHBridgeError.resourceLimitExceeded(message)
            }
            throw LXISHBridgeError.unavailable(message)
        }
        if memoryLimitBytes != nil,
           class_getClassMethod(executorClass, NSSelectorFromString("killProcessGroup:")) == nil
        {
            throw LXISHBridgeError.resourceLimitExceeded(
                "ISHShellExecutor cannot terminate an over-limit guest process group"
            )
        }
        let policyLease = try LXISHExecutionPolicyLease(
            networkPolicy: networkPolicy,
            memoryLimitBytes: memoryLimitBytes
        )

        let launch: (String, [String])
        if let cwd, !cwd.isEmpty {
            launch = (
                "/bin/sh",
                ["-c", "cd \"$1\" && shift && exec \"$@\"", "lingxi-background", cwd, executable] + arguments
            )
        } else {
            launch = (executable, arguments)
        }

        typealias LineBlock = @convention(block) (NSString, Bool) -> Void
        let lineBlock: LineBlock = { line, isStdErr in
            lineSink(line as String, isStdErr)
        }
        typealias CompletionBlock = @convention(block) (AnyObject) -> Void
        let completionBlock: CompletionBlock = { [weak self] result in
            guard let self else { return }
            let policyOutcome = policyLease.finish()
            completion(
                LXISHShellExecutionResultBox(
                    exitCode: self.intValue(from: result, selector: "exitCode"),
                    errorCode: policyOutcome.resourceLimitExceeded ? -5 : self.intValue(from: result, selector: "error"),
                    stdoutText: self.stringValue(from: result, selector: "output"),
                    stderrText: policyOutcome.resourceLimitExceeded
                        ? policyOutcome.resourceLimitMessage
                        : self.stringValue(from: result, selector: "errorOutput"),
                    durationSeconds: self.doubleValue(from: result, selector: "duration")
                )
            )
        }
        typealias Fn = @convention(c) (
            AnyClass,
            Selector,
            NSString,
            NSArray,
            NSDictionary,
            NSData?,
            UInt64,
            AnyObject?,
            AnyObject
        ) -> Int32
        let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
        let pid = fn(
            executorClass,
            selector,
            launch.0 as NSString,
            launch.1 as NSArray,
            environment as NSDictionary,
            stdin.map { Data($0.utf8) } as NSData?,
            policyLease.fsContext,
            lineBlock as AnyObject,
            completionBlock as AnyObject
        )
        guard pid >= 0 else {
            _ = policyLease.finish()
            throw LXISHBridgeError.unavailable("ISHShellExecutor failed to launch background process: \(pid)")
        }
        policyLease.startWatchdog { [weak self] in
            self?.killProcessGroup(pid, executorClass: executorClass)
        }
        return pid
    }

    func spawnRawExecutable(
        _ executable: String,
        arguments: [String],
        environment: [String: String],
        cwd: String?,
        networkPolicy: Int32,
        memoryLimitBytes: UInt64?,
        dataSink: @escaping (Data, Bool) -> Void,
        completion: @escaping (LXISHShellExecutionResultBox) -> Void
    ) throws -> Int32 {
        guard let executorClass = NSClassFromString("ISHShellExecutor") else {
            throw LXISHBridgeError.unavailable(Self.availabilityReason())
        }
        let selector = NSSelectorFromString(
            "executeRawExecutable:arguments:environment:fsContext:dataCallback:completion:"
        )
        guard let method = class_getClassMethod(executorClass, selector) else {
            let message = "ISHShellExecutor is missing its raw stdio entry point"
            if networkPolicy != 0 { throw LXISHBridgeError.networkPolicyUnavailable(message) }
            if memoryLimitBytes != nil { throw LXISHBridgeError.resourceLimitExceeded(message) }
            throw LXISHBridgeError.unavailable(message)
        }
        let policyLease = try LXISHExecutionPolicyLease(
            networkPolicy: networkPolicy,
            memoryLimitBytes: memoryLimitBytes
        )
        let launch: (String, [String])
        if let cwd, !cwd.isEmpty {
            launch = (
                "/bin/sh",
                ["-c", "cd \"$1\" && shift && exec \"$@\"", "lingxi-raw-stdio", cwd, executable] + arguments
            )
        } else {
            launch = (executable, arguments)
        }
        typealias DataBlock = @convention(block) (NSData, Bool) -> Void
        let dataBlock: DataBlock = { data, isStdErr in dataSink(data as Data, isStdErr) }
        typealias CompletionBlock = @convention(block) (AnyObject) -> Void
        let completionBlock: CompletionBlock = { [weak self] result in
            guard let self else { return }
            let outcome = policyLease.finish()
            completion(
                LXISHShellExecutionResultBox(
                    exitCode: self.intValue(from: result, selector: "exitCode"),
                    errorCode: outcome.resourceLimitExceeded ? -5 : self.intValue(from: result, selector: "error"),
                    stdoutText: "",
                    stderrText: outcome.resourceLimitExceeded ? outcome.resourceLimitMessage : "",
                    durationSeconds: self.doubleValue(from: result, selector: "duration")
                )
            )
        }
        typealias Fn = @convention(c) (
            AnyClass, Selector, NSString, NSArray, NSDictionary, UInt64, AnyObject, AnyObject
        ) -> Int32
        let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
        let pid = fn(
            executorClass,
            selector,
            launch.0 as NSString,
            launch.1 as NSArray,
            environment as NSDictionary,
            policyLease.fsContext,
            dataBlock as AnyObject,
            completionBlock as AnyObject
        )
        guard pid >= 0 else {
            _ = policyLease.finish()
            throw LXISHBridgeError.unavailable("ISHShellExecutor failed to launch raw stdio process: \(pid)")
        }
        policyLease.startWatchdog { [weak self] in
            self?.killProcessGroup(pid, executorClass: executorClass)
        }
        return pid
    }

    func writeRawStdin(_ data: Data, pid: Int32) throws {
        guard let executorClass = NSClassFromString("ISHShellExecutor") else {
            throw LXISHBridgeError.unavailable(Self.availabilityReason())
        }
        let selector = NSSelectorFromString("writeRawStdin:pid:")
        guard let method = class_getClassMethod(executorClass, selector) else {
            throw LXISHBridgeError.unavailable("ISHShellExecutor is missing writeRawStdin:pid:")
        }
        typealias Fn = @convention(c) (AnyClass, Selector, NSData, Int32) -> Bool
        let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
        guard fn(executorClass, selector, data as NSData, pid) else {
            throw LXISHBridgeError.io("raw stdio stdin is closed")
        }
    }

    func closeRawStdin(pid: Int32) throws {
        guard let executorClass = NSClassFromString("ISHShellExecutor") else {
            throw LXISHBridgeError.unavailable(Self.availabilityReason())
        }
        let selector = NSSelectorFromString("closeRawStdinForPid:")
        guard let method = class_getClassMethod(executorClass, selector) else {
            throw LXISHBridgeError.unavailable("ISHShellExecutor is missing closeRawStdinForPid:")
        }
        typealias Fn = @convention(c) (AnyClass, Selector, Int32) -> Bool
        let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
        _ = fn(executorClass, selector, pid)
    }

    func killProcessGroup(_ pid: Int32) throws {
        guard pid > 1 else {
            throw LXISHBridgeError.invalidRequest("refusing to terminate iSH pid \(pid)")
        }
        guard let executorClass = NSClassFromString("ISHShellExecutor") else {
            throw LXISHBridgeError.unavailable(Self.availabilityReason())
        }
        let selector = NSSelectorFromString("killProcessGroup:")
        guard let method = class_getClassMethod(executorClass, selector) else {
            throw LXISHBridgeError.unavailable("ISHShellExecutor is missing killProcessGroup:")
        }
        typealias Fn = @convention(c) (AnyClass, Selector, Int32) -> Void
        let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
        fn(executorClass, selector, pid)
    }

    private func killProcessGroup(_ pid: Int32, executorClass: AnyClass) {
        let selector = NSSelectorFromString("killProcessGroup:")
        guard let method = class_getClassMethod(executorClass, selector) else { return }
        typealias Fn = @convention(c) (AnyClass, Selector, Int32) -> Void
        let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
        fn(executorClass, selector, pid)
    }

    private func intValue(from object: AnyObject, selector: String) -> Int {
        let sel = NSSelectorFromString(selector)
        guard let method = class_getInstanceMethod(type(of: object), sel) else { return 0 }
        typealias Fn = @convention(c) (AnyObject, Selector) -> Int32
        let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
        return Int(fn(object, sel))
    }

    private func doubleValue(from object: AnyObject, selector: String) -> Double {
        let sel = NSSelectorFromString(selector)
        guard let method = class_getInstanceMethod(type(of: object), sel) else { return 0 }
        typealias Fn = @convention(c) (AnyObject, Selector) -> Double
        let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
        return fn(object, sel)
    }

    private func stringValue(from object: AnyObject, selector: String) -> String {
        let sel = NSSelectorFromString(selector)
        guard let method = class_getInstanceMethod(type(of: object), sel) else { return "" }
        typealias Fn = @convention(c) (AnyObject, Selector) -> AnyObject?
        let fn = unsafeBitCast(method_getImplementation(method), to: Fn.self)
        return (fn(object, sel) as? String) ?? ""
    }
}

/// Decoder for the iSH kernel's wait-status encoding (`do_exit(status << 8)`
/// on a normal exit; the low 7 bits carry a fatal signal). Internal — the
/// unit suite pins the decode against both encodings.
enum LXISHGuestWaitStatus {
    /// Shell-convention result: normal exit → `(status >> 8) & 0xff`;
    /// signal death → `128 + signal` with a human-readable detail.
    static func decode(_ status: Int32) -> (code: Int32, detail: String?) {
        let signal = status & 0x7f
        if signal == 0 {
            return ((status >> 8) & 0xff, nil)
        }
        return (128 + signal, "terminated by signal \(signal)")
    }
}

private final class LXISHNativeCoordinator {
    static let shared = LXISHNativeCoordinator()

    /// Posted by the embedded iSH kernel's `exit_hook` for guest init (pid 1)
    /// and DIRECT children of init — which in this app are exactly the
    /// interactive shell (`executeCommand:` → `become_new_init_child`) and the
    /// background executor's processes. Payload: `{"pid": pid_t, "code":
    /// wait-status}` (ISHKernel.m `handle_process_exit`).
    private static let guestProcessExited = Notification.Name("ISHProcessExited")

    private var processExitObserver: NSObjectProtocol?

    private init() {
        // Observe unconditionally: without this, a user typing `exit` (or the
        // shell crashing) produced NO event anywhere in the stack — the screen
        // stayed .ready with a dead caret forever, and the one-PTY slot stayed
        // occupied so even a manual restart was refused.
        processExitObserver = NotificationCenter.default.addObserver(
            forName: Self.guestProcessExited,
            object: nil,
            queue: nil
        ) { [weak self] note in
            guard let pid = (note.userInfo?["pid"] as? NSNumber)?.int32Value else { return }
            let code = (note.userInfo?["code"] as? NSNumber)?.int32Value ?? 0
            self?.handleGuestProcessExit(pid: pid, waitStatus: code)
        }
    }

    /// A guest init-or-direct-child process exited. Background executor
    /// children settle through their own completion callbacks and are
    /// excluded by pid; whatever remains while a PTY session is open is the
    /// interactive shell itself (pid 1 means guest init died, which the shell
    /// cannot survive either). Emit the `pty_closed` the Rust reader already
    /// understands, carrying the REAL exit code, and free the one-PTY slot so
    /// the restart link actually works.
    private func handleGuestProcessExit(pid: Int32, waitStatus: Int32) {
        NSLog("LXISHBridge: guest process exit pid=%d rawStatus=0x%x", pid, waitStatus)
        queue.async {
            for (key, runtime) in self.runtimes {
                var runtime = runtime
                guard let sessionId = runtime.ptySessionId else { continue }
                if pid != 1,
                   runtime.backgroundProcesses.values.contains(where: { $0.guestPid == pid }) {
                    continue
                }
                let status = LXISHGuestWaitStatus.decode(waitStatus)
                NSLog(
                    "LXISHBridge: attributing exit pid=%d to PTY session %@ (code=%d detail=%@)",
                    pid, sessionId, status.code, status.detail ?? "nil"
                )
                runtime.ptySessionId = nil
                runtime.nextSequence += 1
                runtime.events.append(
                    LXISHPtyEventPayload(
                        sequence: runtime.nextSequence,
                        sessionId: sessionId,
                        kind: "pty_closed",
                        dataBase64: nil,
                        detail: pid == 1 ? "guest init exited" : status.detail,
                        exitCode: status.code
                    )
                )
                runtime.kernel.markInteractiveShellClosed()
                self.runtimes[key] = runtime
            }
        }
    }

    private final class BackgroundProcessState {
        let processId: String
        let guestPid: Int32
        var killRequested = false
        var terminal = false
        var restoreMounts: [LXISHMountSpec]?
        var networkPolicyEnforced = false
        var memoryLimitEnforced = false

        init(processId: String, guestPid: Int32) {
            self.processId = processId
            self.guestPid = guestPid
        }
    }

    private final class RawStdioState {
        let sessionId: String
        let guestPid: Int32
        let networkPolicyEnforced: Bool
        let memoryLimitEnforced: Bool
        var stdout = Data()
        var stderr = Data()
        var terminal = false
        var exitCode: Int?
        var failure: String?

        init(
            sessionId: String,
            guestPid: Int32,
            networkPolicyEnforced: Bool,
            memoryLimitEnforced: Bool
        ) {
            self.sessionId = sessionId
            self.guestPid = guestPid
            self.networkPolicyEnforced = networkPolicyEnforced
            self.memoryLimitEnforced = memoryLimitEnforced
        }
    }

    private struct RuntimeState {
        var config: LXISHNativeConfig
        var kernel = LXISHKernelRuntimeBridge()
        var executor = LXISHShellExecutorRuntimeBridge()
        var mounts: [LXISHMountSpec] = []
        var kernelBooted = false
        var ptySessionId: String?
        var nextSequence: UInt64 = 0
        var events: [LXISHPtyEventPayload] = []
        var backgroundProcesses: [String: BackgroundProcessState] = [:]
        var backgroundEvents: [LXISHBackgroundEventPayload] = []
        var rawStdioSessions: [String: RawStdioState] = [:]
    }

    private let queue = DispatchQueue(label: "org.mobile-linux.ish-native.bridge")
    private let rootfsManager = LXISHNativeRootfsManager()
    private var runtimes: [String: RuntimeState] = [:]
    private let sharedKernel = LXISHKernelRuntimeBridge()
    private var kernelHasBooted = false

    func availability() -> String {
        encodeEnvelope(
            ok: LXISHKernelRuntimeBridge.isDeviceBridgeAvailable() && LXISHShellExecutorRuntimeBridge.isDeviceBridgeAvailable(),
            payload: [
                "available": LXISHKernelRuntimeBridge.isDeviceBridgeAvailable() && LXISHShellExecutorRuntimeBridge.isDeviceBridgeAvailable(),
                "kernel_available": LXISHKernelRuntimeBridge.isDeviceBridgeAvailable(),
                "shell_executor_available": LXISHShellExecutorRuntimeBridge.isDeviceBridgeAvailable(),
                "backend": "ios-ish",
                "kernel_reason": LXISHKernelRuntimeBridge.availabilityReason(),
                "shell_reason": LXISHShellExecutorRuntimeBridge.availabilityReason()
            ]
        )
    }

    func installRootfs(config: LXISHNativeConfig) -> String {
        execute(config: config) { runtime in
            try self.requireNoIsolatedProcess(runtime)
            let status = try self.rootfsManager.installIfNeeded(for: config)
            runtime.mounts = try self.rootfsManager.mountsForBoot(for: config)
            return ["status": status]
        }
    }

    func repairRootfs(config: LXISHNativeConfig) -> String {
        execute(config: config) { runtime in
            guard !self.kernelHasBooted else {
                throw LXISHBridgeError.restartRequired("restart the app process before repairing a booted iSH rootfs")
            }
            let status = try self.rootfsManager.repair(for: config)
            runtime.mounts = try self.rootfsManager.mountsForBoot(for: config)
            return ["status": status]
        }
    }

    func resetRootfs(config: LXISHNativeConfig) -> String {
        execute(config: config) { runtime in
            guard !self.kernelHasBooted else {
                throw LXISHBridgeError.restartRequired("restart the app process before resetting a booted iSH rootfs")
            }
            let status = try self.rootfsManager.reset(for: config)
            runtime.mounts = []
            runtime.ptySessionId = nil
            runtime.events.removeAll()
            return ["status": status]
        }
    }

    func boot(config: LXISHNativeConfig) -> String {
        execute(config: config) { runtime in
            if runtime.kernelBooted { return ["status": self.rootfsManager.status(for: config)] }
            _ = try self.rootfsManager.installIfNeeded(for: config)
            runtime.mounts = try self.rootfsManager.mountsForBoot(for: config)
            guard LXISHKernelRuntimeBridge.isDeviceBridgeAvailable() else {
                throw LXISHBridgeError.unavailable(LXISHKernelRuntimeBridge.availabilityReason())
            }
            do {
                try self.bootKernel(config: config, runtime: &runtime)
                try self.applyMountsIfNeeded(requestedMounts: runtime.mounts, to: &runtime)
            } catch {
                throw LXISHBridgeError.unavailable(error.localizedDescription)
            }
            let status = self.rootfsManager.status(for: config)
            return ["status": status]
        }
    }

    func configureMounts(config: LXISHNativeConfig, mounts: [LXISHMountSpec]) -> String {
        execute(config: config) { runtime in
            try self.requireNoIsolatedProcess(runtime)
            runtime.mounts = mounts
            try self.ensureHostEndpointsExist(mounts)
            try self.ensureGuestMountParentsExist(mounts, runtime: &runtime)
            if LXISHKernelRuntimeBridge.isDeviceBridgeAvailable() {
                try runtime.kernel.configureMounts(mounts.map(self.dictionary(from:)))
            }
            return ["status": self.rootfsManager.status(for: config)]
        }
    }

    func runSync(config: LXISHNativeConfig, request: LXISHRunRequest) -> String {
        execute(config: config) { runtime in
            try self.requireNoIsolatedProcess(runtime)
            let executionPolicy = try self.executionPolicy(for: request)
            let environment = self.preparedEnvironment(from: request.env, cwd: request.cwd, config: config)
            _ = try self.rootfsManager.installIfNeeded(for: config)
            guard LXISHKernelRuntimeBridge.isDeviceBridgeAvailable(),
                  LXISHShellExecutorRuntimeBridge.isDeviceBridgeAvailable()
            else {
                let message = LXISHShellExecutorRuntimeBridge.availabilityReason()
                if executionPolicy.networkPolicy != 0 {
                    throw LXISHBridgeError.networkPolicyUnavailable(message)
                }
                if executionPolicy.memoryLimitBytes != nil {
                    throw LXISHBridgeError.resourceLimitExceeded(message)
                }
                throw LXISHBridgeError.unavailable(message)
            }
            try self.bootKernel(config: config, runtime: &runtime)
            try self.validateEnvironment(environment)
            var runtimeMounts = runtime.mounts
            defer { runtime.mounts = runtimeMounts }
            return try LXISHExecutionMountScope.withMounts(
                runtimeMounts: &runtimeMounts,
                requestedMounts: request.mounts,
                includeDefaultMounts: request.includeDefaultMounts ?? true,
                apply: { mounts, includeDefaultMounts in
                    try self.applyMountsIfNeeded(
                        requestedMounts: mounts,
                        to: &runtime,
                        includeDefaultMounts: includeDefaultMounts
                    )
                },
                body: {
                    let result = try runtime.executor.runExecutable(
                        request.command,
                        arguments: request.args,
                        environment: environment,
                        stdin: request.stdin,
                        cwd: request.cwd,
                        timeout: self.timeoutSeconds(from: request),
                        networkPolicy: executionPolicy.networkPolicy,
                        memoryLimitBytes: executionPolicy.memoryLimitBytes
                    )
                    if result.errorCode == -5 {
                        throw LXISHBridgeError.resourceLimitExceeded(result.stderrText)
                    }
                    return [
                        "result": LXISHRunResultPayload(
                            stdout: result.stdoutText,
                            stderr: result.stderrText,
                            exitCode: result.exitCode,
                            timedOut: result.errorCode == -3,
                            cancelled: result.errorCode == -4,
                            durationSeconds: result.durationSeconds,
                            networkPolicyEnforced: executionPolicy.networkPolicy != 0,
                            memoryLimitEnforced: executionPolicy.memoryLimitBytes != nil
                        )
                    ]
                }
            )
        }
    }

    func spawnBackground(config: LXISHNativeConfig, request: LXISHRunRequest) -> String {
        execute(config: config) { runtime in
            try self.requireNoIsolatedProcess(runtime)
            let executionPolicy = try self.executionPolicy(for: request)
            let environment = self.preparedEnvironment(from: request.env, cwd: request.cwd, config: config)
            _ = try self.rootfsManager.installIfNeeded(for: config)
            let previousMounts = runtime.mounts
            let isolated = request.includeDefaultMounts == false
            if isolated && (runtime.ptySessionId != nil || runtime.rawStdioSessions.values.contains(where: { !$0.terminal }) || runtime.backgroundProcesses.values.contains(where: { !$0.terminal })) {
                throw LXISHBridgeError.invalidRequest("isolated execution requires other guest sessions to be closed")
            }
            runtime.mounts = request.mounts ?? runtime.mounts
            guard LXISHKernelRuntimeBridge.isDeviceBridgeAvailable(),
                  LXISHShellExecutorRuntimeBridge.isDeviceBridgeAvailable()
            else {
                let message = LXISHShellExecutorRuntimeBridge.availabilityReason()
                if executionPolicy.networkPolicy != 0 {
                    throw LXISHBridgeError.networkPolicyUnavailable(message)
                }
                if executionPolicy.memoryLimitBytes != nil {
                    throw LXISHBridgeError.resourceLimitExceeded(message)
                }
                throw LXISHBridgeError.unavailable(message)
            }
            try self.bootKernel(config: config, runtime: &runtime)
            try self.applyMountsIfNeeded(requestedMounts: runtime.mounts, to: &runtime, includeDefaultMounts: !isolated)
            var launched = false
            defer {
                if isolated && !launched {
                    runtime.mounts = previousMounts
                    try? self.applyMountsIfNeeded(requestedMounts: previousMounts, to: &runtime)
                }
            }
            try self.validateEnvironment(environment)

            let processId = UUID().uuidString.lowercased()
            let runtimeKey = config.runtimeKey
            let pid = try runtime.executor.spawnExecutable(
                request.command,
                arguments: request.args,
                environment: environment,
                stdin: request.stdin,
                cwd: request.cwd,
                networkPolicy: executionPolicy.networkPolicy,
                memoryLimitBytes: executionPolicy.memoryLimitBytes,
                lineSink: { [weak self] line, isStdErr in
                    self?.recordBackgroundLine(
                        runtimeKey: runtimeKey,
                        processId: processId,
                        line: line,
                        isStdErr: isStdErr
                    )
                },
                completion: { [weak self] result in
                    self?.recordBackgroundCompletion(
                        runtimeKey: runtimeKey,
                        processId: processId,
                        result: result
                    )
                }
            )
            runtime.backgroundProcesses[processId] = BackgroundProcessState(
                processId: processId,
                guestPid: pid
            )
            runtime.backgroundProcesses[processId]?.restoreMounts = isolated ? previousMounts : nil
            runtime.backgroundProcesses[processId]?.networkPolicyEnforced = executionPolicy.networkPolicy != 0
            runtime.backgroundProcesses[processId]?.memoryLimitEnforced = executionPolicy.memoryLimitBytes != nil
            launched = true
            return [
                "process_id": processId,
                "guest_pid": Int(pid),
                "network_policy_enforced": executionPolicy.networkPolicy != 0,
                "memory_limit_enforced": executionPolicy.memoryLimitBytes != nil,
            ]
        }
    }

    func killBackground(config: LXISHNativeConfig, request: LXISHBackgroundProcessRequest) -> String {
        execute(config: config) { runtime in
            guard let process = runtime.backgroundProcesses[request.processId] else {
                throw LXISHBridgeError.invalidRequest("unknown background process")
            }
            if process.terminal {
                return ["process_id": request.processId, "already_stopped": true]
            }
            try runtime.executor.killProcessGroup(process.guestPid)
            process.killRequested = true
            return ["process_id": request.processId, "termination_requested": true]
        }
    }

    func pollBackground(config: LXISHNativeConfig, request: LXISHBackgroundPollRequest) -> String {
        queue.sync {
            let key = config.runtimeKey
            guard let runtime = runtimes[key] else {
                return encodeEnvelope(ok: true, payload: ["events": [LXISHBackgroundEventPayload]()])
            }
            guard runtime.backgroundProcesses[request.processId] != nil else {
                return encodeError(code: "invalid_request", message: "unknown background process")
            }
            let filtered = runtime.backgroundEvents.filter { event in
                guard event.processId == request.processId else { return false }
                guard let after = request.afterSequence else { return true }
                return event.sequence > after
            }
            let limit = Int(request.limit ?? UInt32.max)
            return encodeEnvelope(ok: true, payload: ["events": Array(filtered.prefix(limit))])
        }
    }

    func openRawStdio(config: LXISHNativeConfig, request: LXISHRawStdioOpenRequest) -> String {
        execute(config: config) { runtime in
            try self.requireNoIsolatedProcess(runtime)
            let runRequest = LXISHRunRequest(
                command: request.command,
                args: request.args,
                cwd: request.cwd,
                env: request.env,
                stdin: nil,
                timeoutMs: nil,
                network: request.network,
                resourceLimits: request.resourceLimits,
                mounts: request.mounts,
                includeDefaultMounts: true
            )
            let executionPolicy = try self.executionPolicy(for: runRequest)
            let environment = self.preparedEnvironment(from: request.env, cwd: request.cwd, config: config)
            _ = try self.rootfsManager.installIfNeeded(for: config)
            runtime.mounts = request.mounts ?? runtime.mounts
            guard LXISHKernelRuntimeBridge.isDeviceBridgeAvailable(),
                  LXISHShellExecutorRuntimeBridge.isDeviceBridgeAvailable()
            else {
                throw LXISHBridgeError.unavailable(LXISHShellExecutorRuntimeBridge.availabilityReason())
            }
            try self.bootKernel(config: config, runtime: &runtime)
            try self.applyMountsIfNeeded(requestedMounts: runtime.mounts, to: &runtime)
            try self.validateEnvironment(environment)

            let sessionId = UUID().uuidString.lowercased()
            let runtimeKey = config.runtimeKey
            let ingress = LXISHRawOutputIngress()
            let pid = try runtime.executor.spawnRawExecutable(
                request.command,
                arguments: request.args,
                environment: environment,
                cwd: request.cwd,
                networkPolicy: executionPolicy.networkPolicy,
                memoryLimitBytes: executionPolicy.memoryLimitBytes,
                dataSink: { [weak self] data, isStdErr in
                    self?.recordRawStdioData(
                        runtimeKey: runtimeKey,
                        sessionId: sessionId,
                        ingress: ingress,
                        data: data,
                        isStdErr: isStdErr
                    )
                },
                completion: { [weak self] result in
                    self?.recordRawStdioCompletion(
                        runtimeKey: runtimeKey,
                        sessionId: sessionId,
                        result: result
                    )
                }
            )
            runtime.rawStdioSessions[sessionId] = RawStdioState(
                sessionId: sessionId,
                guestPid: pid,
                networkPolicyEnforced: executionPolicy.networkPolicy != 0,
                memoryLimitEnforced: executionPolicy.memoryLimitBytes != nil
            )
            return [
                "session_id": sessionId,
                "network_policy_enforced": executionPolicy.networkPolicy != 0,
                "memory_limit_enforced": executionPolicy.memoryLimitBytes != nil,
            ]
        }
    }

    func writeRawStdio(config: LXISHNativeConfig, request: LXISHRawStdioRequest) -> String {
        guard let encoded = request.dataBase64, let data = Data(base64Encoded: encoded) else {
            return encodeError(code: "invalid_request", message: "raw stdio write requires valid data_base64")
        }
        var target: (LXISHShellExecutorRuntimeBridge, Int32)?
        let validation = execute(config: config) { runtime in
            guard let session = runtime.rawStdioSessions[request.sessionId], !session.terminal else {
                throw LXISHBridgeError.invalidRequest("unknown or closed raw stdio session")
            }
            target = (runtime.executor, session.guestPid)
            return [:]
        }
        guard let (executor, pid) = target else { return validation }
        // Do not hold the coordinator while waiting for guest stdin capacity:
        // close/cancel and output callbacks must continue to make progress.
        do {
            try executor.writeRawStdin(data, pid: pid)
            return encodeEnvelope(ok: true, payload: ["written": data.count])
        } catch let error as LXISHBridgeError {
            return encodeError(code: error.code, message: error.localizedDescription)
        } catch {
            return encodeError(code: "io", message: error.localizedDescription)
        }
    }

    func readRawStdio(config: LXISHNativeConfig, request: LXISHRawStdioRequest) -> String {
        execute(config: config) { runtime in
            guard let session = runtime.rawStdioSessions[request.sessionId] else {
                throw LXISHBridgeError.invalidRequest("unknown raw stdio session")
            }
            let maxBytes = max(1, min(Int(request.maxBytes ?? 65_536), 1_048_576))
            let reportFailure = session.terminal && session.stdout.isEmpty && session.stderr.isEmpty
            let chunk = LXISHRawOutputChunk.drain(
                stdout: &session.stdout, stderr: &session.stderr,
                terminal: session.terminal, maxBytes: maxBytes
            )
            return [
                "stdout_base64": chunk.stdout.base64EncodedString(),
                "stderr_base64": chunk.stderr.base64EncodedString(),
                "closed": chunk.closed && (session.failure == nil || reportFailure),
                "exit_code": session.exitCode ?? NSNull(),
                "error": reportFailure ? LXISHRawOutputChunk.failurePayload(session.failure) : NSNull(),
            ]
        }
    }

    func closeRawStdio(config: LXISHNativeConfig, request: LXISHRawStdioRequest) -> String {
        execute(config: config) { runtime in
            guard let session = runtime.rawStdioSessions[request.sessionId] else {
                return ["already_closed": true]
            }
            if !session.terminal {
                try runtime.executor.closeRawStdin(pid: session.guestPid)
                try runtime.executor.killProcessGroup(session.guestPid)
            }
            return ["termination_requested": !session.terminal, "closed": session.terminal]
        }
    }

    func disposeRawStdio(config: LXISHNativeConfig, request: LXISHRawStdioRequest) -> String {
        execute(config: config) { runtime in
            guard let session = runtime.rawStdioSessions[request.sessionId] else {
                return ["disposed": true]
            }
            guard session.terminal else {
                throw LXISHBridgeError.invalidRequest("raw stdio process has not been reaped")
            }
            runtime.rawStdioSessions.removeValue(forKey: request.sessionId)
            return ["disposed": true]
        }
    }

    private func recordRawStdioData(
        runtimeKey: String,
        sessionId: String,
        ingress: LXISHRawOutputIngress,
        data: Data,
        isStdErr: Bool
    ) {
        switch ingress.reserve(data.count) {
        case .rejected:
            return
        case .overflow:
            queue.async {
                guard let runtime = self.runtimes[runtimeKey],
                      let session = runtime.rawStdioSessions[sessionId], !session.terminal else { return }
                session.failure = "raw stdio pending output exceeded the 4 MiB callback buffer"
                try? runtime.executor.killProcessGroup(session.guestPid)
            }
            return
        case .accepted:
            break
        }
        queue.async {
            defer { ingress.release(data.count) }
            guard let runtime = self.runtimes[runtimeKey],
                  let session = runtime.rawStdioSessions[sessionId],
                  !session.terminal
            else { return }
            let buffered = session.stdout.count + session.stderr.count
            if buffered + data.count > 4 * 1_048_576 {
                session.failure = "raw stdio output exceeded the 4 MiB unread buffer"
                try? runtime.executor.killProcessGroup(session.guestPid)
            } else if isStdErr {
                session.stderr.append(data)
            } else {
                session.stdout.append(data)
            }
        }
    }

    private func recordRawStdioCompletion(
        runtimeKey: String,
        sessionId: String,
        result: LXISHShellExecutionResultBox
    ) {
        queue.async {
            guard let runtime = self.runtimes[runtimeKey],
                  let session = runtime.rawStdioSessions[sessionId]
            else { return }
            session.terminal = true
            session.exitCode = result.exitCode
            if result.errorCode == -5 {
                session.failure = result.stderrText.isEmpty
                    ? "raw stdio process exceeded its memory limit"
                    : result.stderrText
            }
        }
    }

    func probeLoopback(config: LXISHNativeConfig, request: LXISHLoopbackProbeRequest) -> String {
        guard request.port > 0,
              let port = NWEndpoint.Port(rawValue: request.port)
        else {
            return encodeError(code: "invalid_request", message: "loopback port must be greater than zero")
        }
        let semaphore = DispatchSemaphore(value: 0)
        let resultLock = NSLock()
        var reachable = false
        var finished = false
        let connection = NWConnection(host: "127.0.0.1", port: port, using: .tcp)
        connection.stateUpdateHandler = { state in
            switch state {
            case .ready:
                resultLock.lock()
                if !finished {
                    reachable = true
                    finished = true
                    semaphore.signal()
                }
                resultLock.unlock()
            case .failed, .cancelled:
                resultLock.lock()
                if !finished {
                    finished = true
                    semaphore.signal()
                }
                resultLock.unlock()
            default:
                break
            }
        }
        connection.start(queue: DispatchQueue(label: "org.mobile-linux.ish-native.loopback-probe"))
        _ = semaphore.wait(timeout: .now() + .milliseconds(Int(request.timeoutMs)))
        connection.cancel()
        resultLock.lock()
        let result = reachable
        resultLock.unlock()
        return encodeEnvelope(ok: true, payload: ["reachable": result])
    }

    func openPty(config: LXISHNativeConfig, request: LXISHPtyOpenRequest) -> String {
        execute(config: config) { runtime in
            try self.requireNoIsolatedProcess(runtime)
            let environment = self.preparedEnvironment(from: request.env, cwd: request.cwd, config: config)
            _ = try self.rootfsManager.installIfNeeded(for: config)
            runtime.mounts = request.mounts ?? runtime.mounts
            guard LXISHKernelRuntimeBridge.isDeviceBridgeAvailable() else {
                throw LXISHBridgeError.unavailable(LXISHKernelRuntimeBridge.availabilityReason())
            }
            try self.bootKernel(config: config, runtime: &runtime)
            try self.applyMountsIfNeeded(requestedMounts: runtime.mounts, to: &runtime)
            if runtime.ptySessionId != nil {
                throw LXISHBridgeError.unavailable("only one interactive PTY session is supported per managed root")
            }
            let sessionId = UUID().uuidString.lowercased()
            // A fresh session starts from a fresh journal. `pollOutput` never
            // prunes, and the Rust reader for a NEW session polls from
            // `after_sequence = None` — so a reopen (the terminal's 重新启动
            // shell) would replay every retained event of the previous
            // session as foreign-session traffic before reaching its own.
            // `nextSequence` keeps rising so cursors stay monotonic.
            runtime.events.removeAll()
            try self.validateEnvironment(environment)
            let ptyCommand = self.ptyCommand(
                from: LXISHPtyOpenRequest(
                    command: request.command,
                    args: request.args,
                    cwd: request.cwd,
                    env: environment,
                    cols: request.cols,
                    rows: request.rows,
                    mounts: request.mounts
                )
            )
            try runtime.kernel.openInteractiveShell(
                withCommand: ptyCommand,
                cols: request.cols,
                rows: request.rows
            ) { data in
                self.queue.async {
                    guard var current = self.runtimes[config.runtimeKey],
                          // A straggler callback from a PREVIOUS shell can fire
                          // after its session closed and a new one opened; the
                          // Rust reader treats any foreign-session event as a
                          // runtime error, which the terminal renders as a
                          // failure of the NEW session.
                          current.ptySessionId == sessionId
                    else { return }
                    current.nextSequence += 1
                    current.events.append(
                        LXISHPtyEventPayload(
                            sequence: current.nextSequence,
                            sessionId: sessionId,
                            kind: "pty_output",
                            dataBase64: data.base64EncodedString(),
                            detail: nil,
                            exitCode: nil
                        )
                    )
                    self.runtimes[config.runtimeKey] = current
                }
            }
            runtime.ptySessionId = sessionId
            return ["session_id": sessionId, "available": true]
        }
    }

    func writePty(config: LXISHNativeConfig, request: LXISHPtyWriteRequest) -> String {
        execute(config: config) { runtime in
            guard runtime.ptySessionId == request.sessionId else {
                throw LXISHBridgeError.invalidRequest("unknown PTY session")
            }
            guard let data = Data(base64Encoded: request.dataBase64) else {
                throw LXISHBridgeError.invalidRequest("data_base64 is not valid base64")
            }
            try runtime.kernel.writeInputData(data)
            return ["session_id": request.sessionId]
        }
    }

    func resizePty(config: LXISHNativeConfig, request: LXISHPtyResizeRequest) -> String {
        execute(config: config) { runtime in
            guard runtime.ptySessionId == request.sessionId else {
                throw LXISHBridgeError.invalidRequest("unknown PTY session")
            }
            try runtime.kernel.resizeColumns(request.cols, rows: request.rows)
            return ["session_id": request.sessionId]
        }
    }

    func closePty(config: LXISHNativeConfig, request: LXISHPtyCloseRequest) -> String {
        execute(config: config) { runtime in
            guard runtime.ptySessionId == request.sessionId else {
                throw LXISHBridgeError.invalidRequest("unknown PTY session")
            }
            // Release the slot BEFORE asking the kernel to close, not after.
            // `ptySessionId` is our own bookkeeping and only one PTY is allowed
            // per managed root, so a `closeInteractiveShell` that throws used to
            // leave the slot occupied forever — every later open in this process
            // was refused, with no way back short of killing the app. `execute`
            // persists the mutated runtime on the throwing path too, so this
            // assignment survives the error.
            runtime.ptySessionId = nil
            runtime.nextSequence += 1
            runtime.events.append(
                LXISHPtyEventPayload(
                    sequence: runtime.nextSequence,
                    sessionId: request.sessionId,
                    kind: "pty_closed",
                    dataBase64: nil,
                    detail: nil,
                    exitCode: nil
                )
            )
            try runtime.kernel.closeInteractiveShell()
            return ["session_id": request.sessionId]
        }
    }

    func pollOutput(config: LXISHNativeConfig, request: LXISHPollRequest) -> String {
        queue.sync {
            let key = config.runtimeKey
            guard let runtime = runtimes[key] else {
                return encodeEnvelope(ok: true, payload: ["events": [LXISHPtyEventPayload]()])
            }
            let filtered = runtime.events.filter { event in
                guard let after = request.afterSequence else { return true }
                return event.sequence > after
            }
            let limit = Int(request.limit ?? UInt32.max)
            return encodeEnvelope(ok: true, payload: ["events": Array(filtered.prefix(limit))])
        }
    }

    private func recordBackgroundLine(
        runtimeKey: String,
        processId: String,
        line: String,
        isStdErr: Bool
    ) {
        queue.async {
            guard var runtime = self.runtimes[runtimeKey],
                  let process = runtime.backgroundProcesses[processId],
                  !process.terminal
            else { return }
            runtime.nextSequence += 1
            runtime.backgroundEvents.append(
                LXISHBackgroundEventPayload(
                    sequence: runtime.nextSequence,
                    processId: processId,
                    kind: isStdErr ? "stderr_chunk" : "stdout_line",
                    line: isStdErr ? nil : line,
                    dataBase64: isStdErr ? Data("\(line)\n".utf8).base64EncodedString() : nil,
                    exitCode: nil,
                    cancelled: nil,
                    detail: nil
                )
            )
            self.trimBackgroundEvents(&runtime)
            self.runtimes[runtimeKey] = runtime
        }
    }

    private func recordBackgroundCompletion(
        runtimeKey: String,
        processId: String,
        result: LXISHShellExecutionResultBox
    ) {
        queue.async {
            guard var runtime = self.runtimes[runtimeKey],
                  let process = runtime.backgroundProcesses[processId],
                  !process.terminal
            else { return }
            process.terminal = true
            var terminalError: LXISHErrorPayload?
            if let previousMounts = process.restoreMounts {
                runtime.mounts = previousMounts
                do { try self.applyMountsIfNeeded(requestedMounts: previousMounts, to: &runtime) }
                catch { terminalError = LXISHErrorPayload(code: "io", message: "restore isolated mounts: \(error.localizedDescription)") }
                process.restoreMounts = nil
            }
            if result.errorCode == -5 {
                terminalError = LXISHErrorPayload(code: "resource_limit_exceeded", message: result.stderrText)
            }
            let cancelled = process.killRequested || result.errorCode == -4
            let detail: String?
            switch result.errorCode {
            case -3: detail = "background process timed out"
            case -4: detail = "background process cancelled"
            case -5: detail = "resource_limit_exceeded"
            case 0: detail = nil
            default: detail = "background process failed with executor error \(result.errorCode)"
            }
            runtime.nextSequence += 1
            runtime.backgroundEvents.append(
                LXISHBackgroundEventPayload(
                    sequence: runtime.nextSequence,
                    processId: processId,
                    kind: "process_exited",
                    line: nil,
                    dataBase64: nil,
                    exitCode: result.exitCode,
                    cancelled: cancelled,
                    detail: detail,
                    result: LXISHRunResultPayload(
                        stdout: result.stdoutText, stderr: result.stderrText,
                        exitCode: result.exitCode, timedOut: result.errorCode == -3,
                        cancelled: cancelled, durationSeconds: result.durationSeconds,
                        networkPolicyEnforced: process.networkPolicyEnforced,
                        memoryLimitEnforced: process.memoryLimitEnforced
                    ),
                    error: terminalError
                )
            )
            self.trimBackgroundEvents(&runtime)
            self.runtimes[runtimeKey] = runtime
        }
    }

    private func trimBackgroundEvents(_ runtime: inout RuntimeState) {
        let overflow = runtime.backgroundEvents.count - 4096
        if overflow > 0 {
            runtime.backgroundEvents.removeFirst(overflow)
        }
    }

    private func requireNoIsolatedProcess(_ runtime: RuntimeState) throws {
        if runtime.backgroundProcesses.values.contains(where: { !$0.terminal && $0.restoreMounts != nil }) {
            throw LXISHBridgeError.invalidRequest("an isolated guest process owns the mount scope; wait or cancel it before starting another session")
        }
    }

    private func bootKernel(config: LXISHNativeConfig, runtime: inout RuntimeState) throws {
        if let overlay = config.rootfsPatchPath {
            overlay.withCString { mlr_ish_set_overlay_bundle($0) }
        } else {
            mlr_ish_set_overlay_bundle(nil)
        }
        try runtime.kernel.boot(withRootPath: config.rootfsURL.path)
        kernelHasBooted = true
        runtime.kernelBooted = true
    }

    private func execute(config: LXISHNativeConfig, work: (inout RuntimeState) throws -> [String: Any]) -> String {
        queue.sync {
            // The embedded iSH kernel and mount namespace are process-global.
            // Never let a second logical root silently reuse that kernel.
            if let existing = runtimes.values.first, !existing.config.sameKernel(as: config) {
                return encodeError(code: "invalid_request", message: "iSH permits one immutable configuration per app process")
            }
            if runtimes.contains(where: { key, state in
                key != config.runtimeKey && (state.ptySessionId != nil
                    || state.backgroundProcesses.values.contains(where: { !$0.terminal })
                    || state.rawStdioSessions.values.contains(where: { !$0.terminal }))
            }) {
                return encodeError(code: "invalid_request", message: "another workspace has live guest tasks; close those sessions before switching mounts")
            }
            var runtime = runtimes[config.runtimeKey] ?? RuntimeState(config: config, kernel: sharedKernel)
            do {
                LXISHDNSRefreshMonitor.shared.startIfNeeded()
                let payload = try work(&runtime)
                runtimes[config.runtimeKey] = runtime
                return encodeEnvelope(ok: true, payload: payload)
            } catch let error as LXISHBridgeError {
                runtimes[config.runtimeKey] = runtime
                return encodeError(code: error.code, message: error.localizedDescription)
            } catch {
                runtimes[config.runtimeKey] = runtime
                return encodeError(code: "io", message: error.localizedDescription)
            }
        }
    }

    private func applyMountsIfNeeded(
        requestedMounts: [LXISHMountSpec],
        to runtime: inout RuntimeState,
        includeDefaultMounts: Bool = true
    ) throws {
        let mounts = LXISHRuntimeMountPlanner.effectiveMounts(
            requestedMounts: requestedMounts,
            config: runtime.config,
            includeDefaultMounts: includeDefaultMounts
        )
        try ensureHostEndpointsExist(mounts)
        try ensureGuestMountParentsExist(mounts, runtime: &runtime)
        guard !mounts.isEmpty else { return }
        try runtime.kernel.configureMounts(mounts.map(dictionary(from:)))
    }

    /// The host half of every bind: the directory actually being shared.
    private func ensureHostEndpointsExist(_ mounts: [LXISHMountSpec]) throws {
        for mount in mounts {
            try FileManager.default.createDirectory(
                at: URL(fileURLWithPath: mount.hostPath, isDirectory: true),
                withIntermediateDirectories: true
            )
        }
    }

    /// The guest half: every ANCESTOR of a mount point must exist *inside the
    /// fakefs*, which means having a row in `meta.db` — not merely a directory
    /// under `data/`.
    ///
    /// `fakefs_bind_mount` registers exactly the path it binds and none of its
    /// parents, so a build mount such as
    /// `/var/lingxi/local-app-build/<id>/store/project` would otherwise leave
    /// its intermediate directories unknown to the guest and every lookup
    /// through them would return ENOENT. Mount points one level under a
    /// directory baked into the rootfs image (`/workspace/<id>`) were the only
    /// ones that ever worked, and they worked by accident.
    ///
    /// Creating the parents with `FileManager` — which is what this used to do
    /// — is worse than doing nothing. `fakefs_mkdir` calls the host `mkdir`
    /// first and rolls the transaction back on failure, so a directory that
    /// already exists under `data/` makes EEXIST permanent: the guest can
    /// never register the path afterwards. They have to be made through the
    /// guest.
    private func ensureGuestMountParentsExist(
        _ mounts: [LXISHMountSpec],
        runtime: inout RuntimeState
    ) throws {
        // Needs a booted kernel: the only way to create a path the fakefs
        // knows about is to run `mkdir` inside it. Callers that configure
        // mounts before boot skip this; the run path boots first and then
        // reaches here before the caller's command.
        let parents = LXISHRuntimeMountPlanner.guestMountParents(of: mounts)
        guard !parents.isEmpty,
              runtime.kernelBooted,
              LXISHShellExecutorRuntimeBridge.isDeviceBridgeAvailable()
        else {
            return
        }

        // Ask the guest — not the host filesystem — which are missing. Only
        // the guest's answer reflects `meta.db`, and `meta.db` is what path
        // lookup consults.
        let probe = try runGuestHelper(
            ["-c", #"for d in "$@"; do [ -d "$d" ] || printf '%s\n' "$d"; done"#, "sh"] + parents,
            runtime: &runtime
        )
        let missing = probe.split(separator: "\n").map(String.init).filter { !$0.isEmpty }
        guard !missing.isEmpty else { return }

        // A path the guest cannot see is one nothing in the guest can be
        // using, so clearing the stale scaffold underneath it is safe — and it
        // is the only way to lift the EEXIST that would otherwise make the
        // `mkdir` below fail forever. Shallowest first: removing a parent
        // takes its children with it. Any bind leaf destroyed here is
        // re-established by the `configureMounts` that follows.
        let dataRoot = runtime.config.rootfsDataURL.standardizedFileURL
        for path in missing.sorted(by: LXISHRuntimeMountPlanner.shallowestFirst) {
            guard let scaffold = LXISHRuntimeMountPlanner.guestMountPointURL(
                for: path,
                under: dataRoot
            ) else {
                continue
            }
            try? FileManager.default.removeItem(at: scaffold)
        }

        _ = try runGuestHelper(["-c", #"mkdir -p "$@""#, "sh"] + missing, runtime: &runtime)
    }

    /// Run one short `/bin/sh` command inside the guest and return its stdout.
    /// Used only for mount-point bookkeeping, before the caller's real command.
    private func runGuestHelper(
        _ arguments: [String],
        runtime: inout RuntimeState
    ) throws -> String {
        let result = try runtime.executor.runExecutable(
            "/bin/sh",
            arguments: arguments,
            environment: ["PATH": "/bin:/usr/bin:/sbin:/usr/sbin"],
            stdin: nil,
            cwd: "/",
            timeout: 30,
            networkPolicy: 0,
            memoryLimitBytes: nil
        )
        return result.stdoutText
    }


    private func executionPolicy(
        for request: LXISHRunRequest
    ) throws -> (networkPolicy: Int32, memoryLimitBytes: UInt64?) {
        let networkPolicy: Int32
        switch request.network {
        case "allowed": networkPolicy = 0
        case "disabled": networkPolicy = 1
        case "loopback-only": networkPolicy = 2
        default:
            throw LXISHBridgeError.invalidRequest(
                "unsupported iSH network policy: \(request.network)"
            )
        }

        let limits = request.resourceLimits
        if limits?.maxCpuSeconds != nil || limits?.maxProcesses != nil || limits?.maxOpenFiles != nil {
            throw LXISHBridgeError.resourceLimitExceeded(
                "iSH supports only the per-execution memory limit"
            )
        }
        let memoryLimitBytes: UInt64?
        if let megabytes = limits?.maxMemoryMb {
            guard megabytes > 0 else {
                throw LXISHBridgeError.invalidRequest("max_memory_mb must be greater than zero")
            }
            memoryLimitBytes = UInt64(megabytes) * 1024 * 1024
        } else {
            memoryLimitBytes = nil
        }
        return (networkPolicy, memoryLimitBytes)
    }

    private func timeoutSeconds(from request: LXISHRunRequest) -> Double {
        guard let timeoutMs = request.timeoutMs else { return 0 }
        return Double(timeoutMs) / 1000
    }

    private func preparedEnvironment(
        from requestEnvironment: [String: String],
        cwd: String?,
        config: LXISHNativeConfig
    ) -> [String: String] {
        LXISHGuestEnvironment.merged(
            requestEnvironment: requestEnvironment,
            cwd: cwd,
            stableWorkspaceId: config.stableWorkspaceId
        )
    }

    private func ptyCommand(from request: LXISHPtyOpenRequest) -> [String] {
        guard request.cwd?.isEmpty == false || !request.env.isEmpty else {
            return [request.command] + request.args
        }
        let script = """
        if [ -n "$1" ]; then cd "$1" || exit $?; fi
        shift
        while [ "$1" != "--" ]; do export "$1" || exit $?; shift; done
        shift
        exec "$@"
        """
        let environment = request.env
            .sorted { $0.key < $1.key }
            .map { "\($0.key)=\($0.value)" }
        return ["/bin/sh", "-c", script, "lingxi-pty", request.cwd ?? ""]
            + environment + ["--", request.command] + request.args
    }

    private func validateEnvironment(_ environment: [String: String]) throws {
        for (key, value) in environment {
            guard !key.isEmpty,
                  !key.contains("="),
                  !key.utf8.contains(0),
                  !value.utf8.contains(0)
            else {
                throw LXISHBridgeError.invalidRequest("environment contains an invalid key or NUL byte")
            }
        }
    }

    private func dictionary(from mount: LXISHMountSpec) -> [String: Any] {
        [
            "host_path": mount.hostPath,
            "guest_path": mount.guestPath,
            "read_only": mount.readOnly,
            "purpose": mount.purpose
        ]
    }
}

private struct AnyEncodable: Encodable {
    private let encodeImpl: (Encoder) throws -> Void

    init<T: Encodable>(_ value: T) {
        encodeImpl = value.encode
    }

    func encode(to encoder: Encoder) throws {
        try encodeImpl(encoder)
    }
}

private struct Envelope: Encodable {
    var ok: Bool
    var payload: [String: AnyEncodable]

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: DynamicCodingKey.self)
        try container.encode(ok, forKey: DynamicCodingKey("ok"))
        for (key, value) in payload {
            try container.encode(value, forKey: DynamicCodingKey(key))
        }
    }
}

private struct DynamicCodingKey: CodingKey {
    var stringValue: String
    var intValue: Int?

    init(_ stringValue: String) {
        self.stringValue = stringValue
        self.intValue = nil
    }

    init?(stringValue: String) {
        self.init(stringValue)
    }

    init?(intValue: Int) {
        self.stringValue = "\(intValue)"
        self.intValue = intValue
    }
}

func encodeEnvelope(ok: Bool, payload: [String: Any]) -> String {
    let converted = payload.reduce(into: [String: AnyEncodable]()) { result, item in
        switch item.value {
        case let value as AnyEncodable:
            result[item.key] = value
        case let value as String:
            result[item.key] = AnyEncodable(value)
        case let value as [String: String]:
            result[item.key] = AnyEncodable(value)
        case let value as Bool:
            result[item.key] = AnyEncodable(value)
        case let value as Int:
            result[item.key] = AnyEncodable(value)
        case let value as UInt64:
            result[item.key] = AnyEncodable(value)
        case let value as UInt32:
            result[item.key] = AnyEncodable(value)
        case let value as [LXISHPtyEventPayload]:
            result[item.key] = AnyEncodable(value)
        case let value as [LXISHBackgroundEventPayload]:
            result[item.key] = AnyEncodable(value)
        case let value as LXISHRootfsStatus:
            result[item.key] = AnyEncodable(value)
        case let value as LXISHRunResultPayload:
            result[item.key] = AnyEncodable(value)
        default:
            break
        }
    }
    let data = (try? LXISHBridgeJSON.encoder().encode(Envelope(ok: ok, payload: converted))) ?? Data("{\"ok\":false}".utf8)
    return String(decoding: data, as: UTF8.self)
}

private func encodeError(code: String, message: String) -> String {
    encodeEnvelope(ok: false, payload: ["error": AnyEncodable(LXISHErrorPayload(code: code, message: message))])
}

private func decodeConfig(_ pointer: UnsafePointer<CChar>?) throws -> LXISHNativeConfig {
    try decode(pointer, as: LXISHNativeConfig.self)
}

private func decode<T: Decodable>(_ pointer: UnsafePointer<CChar>?, as type: T.Type) throws -> T {
    guard let pointer else {
        throw LXISHBridgeError.invalidRequest("missing JSON payload")
    }
    let string = String(cString: pointer)
    guard let data = string.data(using: .utf8) else {
        throw LXISHBridgeError.invalidRequest("payload is not valid UTF-8")
    }
    do {
        return try LXISHBridgeJSON.decoder().decode(T.self, from: data)
    } catch {
        throw LXISHBridgeError.invalidRequest(error.localizedDescription)
    }
}

private func bridgeString(_ value: String) -> UnsafeMutablePointer<CChar>? {
    strdup(value)
}

private func bridgingResult(_ block: () throws -> String) -> UnsafeMutablePointer<CChar>? {
    do {
        return bridgeString(try block())
    } catch let error as LXISHBridgeError {
        return bridgeString(encodeError(code: error.code, message: error.localizedDescription))
    } catch {
        return bridgeString(encodeError(code: "io", message: error.localizedDescription))
    }
}

private func aggregatePtyReadJSON(from envelopeString: String) -> String {
    guard let data = envelopeString.data(using: .utf8),
          let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
    else {
        return envelopeString
    }
    guard json["ok"] as? Bool == true else {
        if let error = json["error"],
           let payload = try? JSONSerialization.data(withJSONObject: ["error": error], options: [.sortedKeys])
        {
            return String(decoding: payload, as: UTF8.self)
        }
        return envelopeString
    }
    let events = json["events"] as? [[String: Any]] ?? []
    var combined = Data()
    var lastSequence: UInt64 = 0
    var sessionId: String?
    var closed = false
    for event in events {
        if let sequence = event["sequence"] as? NSNumber {
            lastSequence = max(lastSequence, sequence.uint64Value)
        }
        if sessionId == nil {
            sessionId = event["session_id"] as? String
        }
        if let kind = event["kind"] as? String {
            if kind == "pty_output",
               let base64 = event["data_base64"] as? String,
               let chunk = Data(base64Encoded: base64) {
                combined.append(chunk)
            } else if kind == "pty_closed" {
                closed = true
            }
        }
    }
    let payload: [String: Any] = [
        "session_id": sessionId ?? "",
        "data_base64": combined.base64EncodedString(),
        "last_sequence": lastSequence,
        "closed": closed
    ]
    guard let payloadData = try? JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys]) else {
        return envelopeString
    }
    return String(decoding: payloadData, as: UTF8.self)
}

@_cdecl("mlr_ish_is_available")
func mlr_ish_is_available() -> Bool {
    LXISHKernelRuntimeBridge.isDeviceBridgeAvailable() && LXISHShellExecutorRuntimeBridge.isDeviceBridgeAvailable()
}

@_cdecl("mlr_ish_availability_json")
func mlr_ish_availability_json() -> UnsafeMutablePointer<CChar>? {
    bridgeString(LXISHNativeCoordinator.shared.availability())
}

@_cdecl("mlr_ish_install_rootfs_json")
func mlr_ish_install_rootfs_json(_ configJSON: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        LXISHNativeCoordinator.shared.installRootfs(config: try decodeConfig(configJSON))
    }
}

@_cdecl("mlr_ish_repair_rootfs_json")
func mlr_ish_repair_rootfs_json(_ configJSON: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        LXISHNativeCoordinator.shared.repairRootfs(config: try decodeConfig(configJSON))
    }
}

@_cdecl("mlr_ish_reset_rootfs_json")
func mlr_ish_reset_rootfs_json(_ configJSON: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        LXISHNativeCoordinator.shared.resetRootfs(config: try decodeConfig(configJSON))
    }
}

@_cdecl("mlr_ish_boot_json")
func mlr_ish_boot_json(_ configJSON: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        LXISHNativeCoordinator.shared.boot(config: try decodeConfig(configJSON))
    }
}

private struct LXISHMountsEnvelope: Codable {
    var mounts: [LXISHMountSpec]
}

@_cdecl("mlr_ish_configure_mounts_json")
func mlr_ish_configure_mounts_json(
    _ configJSON: UnsafePointer<CChar>?,
    _ mountsJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        let config = try decodeConfig(configJSON)
        let mounts = try decode(mountsJSON, as: LXISHMountsEnvelope.self)
        return LXISHNativeCoordinator.shared.configureMounts(config: config, mounts: mounts.mounts)
    }
}

@_cdecl("mlr_ish_run_sync_json")
func mlr_ish_run_sync_json(
    _ configJSON: UnsafePointer<CChar>?,
    _ requestJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        let config = try decodeConfig(configJSON)
        let request = try decode(requestJSON, as: LXISHRunRequest.self)
        return LXISHNativeCoordinator.shared.runSync(config: config, request: request)
    }
}

@_cdecl("mlr_ish_background_spawn_json")
func mlr_ish_background_spawn_json(
    _ configJSON: UnsafePointer<CChar>?,
    _ requestJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        let config = try decodeConfig(configJSON)
        let request = try decode(requestJSON, as: LXISHRunRequest.self)
        return LXISHNativeCoordinator.shared.spawnBackground(config: config, request: request)
    }
}

@_cdecl("mlr_ish_background_kill_json")
func mlr_ish_background_kill_json(
    _ configJSON: UnsafePointer<CChar>?,
    _ requestJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        let config = try decodeConfig(configJSON)
        let request = try decode(requestJSON, as: LXISHBackgroundProcessRequest.self)
        return LXISHNativeCoordinator.shared.killBackground(config: config, request: request)
    }
}

@_cdecl("mlr_ish_background_poll_json")
func mlr_ish_background_poll_json(
    _ configJSON: UnsafePointer<CChar>?,
    _ requestJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        let config = try decodeConfig(configJSON)
        let request = try decode(requestJSON, as: LXISHBackgroundPollRequest.self)
        return LXISHNativeCoordinator.shared.pollBackground(config: config, request: request)
    }
}

@_cdecl("mlr_ish_raw_stdio_open_json")
func mlr_ish_raw_stdio_open_json(
    _ configJSON: UnsafePointer<CChar>?,
    _ requestJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        let config = try decodeConfig(configJSON)
        let request = try decode(requestJSON, as: LXISHRawStdioOpenRequest.self)
        return LXISHNativeCoordinator.shared.openRawStdio(config: config, request: request)
    }
}

@_cdecl("mlr_ish_raw_stdio_write_json")
func mlr_ish_raw_stdio_write_json(
    _ configJSON: UnsafePointer<CChar>?,
    _ requestJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        let config = try decodeConfig(configJSON)
        let request = try decode(requestJSON, as: LXISHRawStdioRequest.self)
        return LXISHNativeCoordinator.shared.writeRawStdio(config: config, request: request)
    }
}

@_cdecl("mlr_ish_raw_stdio_read_json")
func mlr_ish_raw_stdio_read_json(
    _ configJSON: UnsafePointer<CChar>?,
    _ requestJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        let config = try decodeConfig(configJSON)
        let request = try decode(requestJSON, as: LXISHRawStdioRequest.self)
        return LXISHNativeCoordinator.shared.readRawStdio(config: config, request: request)
    }
}

@_cdecl("mlr_ish_raw_stdio_close_json")
func mlr_ish_raw_stdio_close_json(
    _ configJSON: UnsafePointer<CChar>?,
    _ requestJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        let config = try decodeConfig(configJSON)
        let request = try decode(requestJSON, as: LXISHRawStdioRequest.self)
        return LXISHNativeCoordinator.shared.closeRawStdio(config: config, request: request)
    }
}

@_cdecl("mlr_ish_raw_stdio_dispose_json")
func mlr_ish_raw_stdio_dispose_json(
    _ configJSON: UnsafePointer<CChar>?,
    _ requestJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        let config = try decodeConfig(configJSON)
        let request = try decode(requestJSON, as: LXISHRawStdioRequest.self)
        return LXISHNativeCoordinator.shared.disposeRawStdio(config: config, request: request)
    }
}

@_cdecl("mlr_ish_probe_loopback_json")
func mlr_ish_probe_loopback_json(
    _ configJSON: UnsafePointer<CChar>?,
    _ requestJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        let config = try decodeConfig(configJSON)
        let request = try decode(requestJSON, as: LXISHLoopbackProbeRequest.self)
        return LXISHNativeCoordinator.shared.probeLoopback(config: config, request: request)
    }
}

@_cdecl("mlr_ish_pty_open_json")
func mlr_ish_pty_open_json(
    _ configJSON: UnsafePointer<CChar>?,
    _ requestJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        let config = try decodeConfig(configJSON)
        let request = try decode(requestJSON, as: LXISHPtyOpenRequest.self)
        return LXISHNativeCoordinator.shared.openPty(config: config, request: request)
    }
}

@_cdecl("mlr_ish_pty_write_json")
func mlr_ish_pty_write_json(
    _ configJSON: UnsafePointer<CChar>?,
    _ requestJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        let config = try decodeConfig(configJSON)
        let request = try decode(requestJSON, as: LXISHPtyWriteRequest.self)
        return LXISHNativeCoordinator.shared.writePty(config: config, request: request)
    }
}

@_cdecl("mlr_ish_pty_resize_json")
func mlr_ish_pty_resize_json(
    _ configJSON: UnsafePointer<CChar>?,
    _ requestJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        let config = try decodeConfig(configJSON)
        let request = try decode(requestJSON, as: LXISHPtyResizeRequest.self)
        return LXISHNativeCoordinator.shared.resizePty(config: config, request: request)
    }
}

@_cdecl("mlr_ish_pty_close_json")
func mlr_ish_pty_close_json(
    _ configJSON: UnsafePointer<CChar>?,
    _ requestJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        let config = try decodeConfig(configJSON)
        let request = try decode(requestJSON, as: LXISHPtyCloseRequest.self)
        return LXISHNativeCoordinator.shared.closePty(config: config, request: request)
    }
}

@_cdecl("mlr_ish_poll_output_json")
func mlr_ish_poll_output_json(
    _ configJSON: UnsafePointer<CChar>?,
    _ requestJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    bridgingResult {
        let config = try decodeConfig(configJSON)
        let request = try decode(requestJSON, as: LXISHPollRequest.self)
        return LXISHNativeCoordinator.shared.pollOutput(config: config, request: request)
    }
}

@_cdecl("mlr_ish_free_string")
func mlr_ish_free_string(_ value: UnsafeMutablePointer<CChar>?) {
    guard let value else { return }
    free(value)
}


/// Bounds pending callback payloads even while the coordinator handles a blocking guest operation.
final class LXISHRawOutputIngress {
    enum Reservation { case accepted, overflow, rejected }
    private let lock = NSLock()
    private var pending = 0
    private var failed = false
    private let limit: Int

    init(limit: Int = 4 * 1_048_576) { self.limit = limit }

    func reserve(_ count: Int) -> Reservation {
        lock.lock()
        defer { lock.unlock() }
        guard !failed else { return .rejected }
        guard count <= limit - pending else {
            failed = true
            return .overflow
        }
        pending += count
        return .accepted
    }

    func release(_ count: Int) {
        lock.lock()
        pending -= count
        lock.unlock()
    }
}

/// EOF is observable only after the final bytes of both streams have been delivered.
struct LXISHRawOutputChunk {
    let stdout: Data
    let stderr: Data
    let closed: Bool

    static func failurePayload(_ message: String?) -> Any {
        guard let message else { return NSNull() }
        return ["code": "resource_limit_exceeded", "message": message]
    }

    static func drain(stdout: inout Data, stderr: inout Data, terminal: Bool, maxBytes: Int) -> Self {
        let stdoutCount = min(maxBytes, stdout.count)
        let stderrCount = min(maxBytes - stdoutCount, stderr.count)
        let output = Data(stdout.prefix(stdoutCount))
        let error = Data(stderr.prefix(stderrCount))
        stdout.removeFirst(stdoutCount)
        stderr.removeFirst(stderrCount)
        return Self(stdout: output, stderr: error, closed: terminal && stdout.isEmpty && stderr.isEmpty)
    }
}
