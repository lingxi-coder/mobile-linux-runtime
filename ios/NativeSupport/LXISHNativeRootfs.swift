//
//  LXISHNativeRootfs.swift
//  LingxiCode
//
//  Rootfs lifecycle and zip extraction logic adapted from OpenMinis
//  `src/ios/iSH/RootfsManager.swift`, trimmed to LingXi's mobile-linux bridge.
//
//  SPDX-License-Identifier: GPL-3.0-only
//

import CryptoKit
import Foundation

/// Returned to Rust under the envelope's `"status"` key. Rust is uniformly
/// snake_case on both directions of this bridge, and this struct was emitting
/// camelCase — latent only because `parse_native_ok` reads `{ok, error}` and
/// throws the rest away. The first Rust-side reader of a real status field
/// would have reproduced the config-decode outage exactly.
struct LXISHRootfsStatus: Codable {
    var state: String
    var backend: String
    var mode: String
    var platform: String
    var abi: String
    var version: String?
    var managedRoot: String?
    var activeRoot: String?
    var stagedRoot: String?
    var archiveSha256: String?
    var installedSizeBytes: UInt64?
    var writableGuestPaths: [String]
    var lastError: String?


    enum CodingKeys: String, CodingKey {
        case state
        case backend
        case mode
        case platform
        case abi
        case version
        case managedRoot = "managed_root"
        case activeRoot = "active_root"
        case stagedRoot = "staged_root"
        case archiveSha256 = "archive_sha256"
        case installedSizeBytes = "installed_size_bytes"
        case writableGuestPaths = "writable_guest_paths"
        case lastError = "last_error"
    }
}

/// The guest workspace that exists whether or not a project is open.
///
/// A terminal is a shell on the Linux runtime; it is not a property of a
/// project. The Settings page has always opened one this way — a persisted
/// workspace id under Application Support — while the terminal insisted on a
/// project and refused to start without one. Both now resolve the same
/// workspace, so they are the same machine rather than two.
/// The Swift twin of Rust's guest-path atlas
/// (`traits::mobile_linux::guest_paths`) — the ONE definition on this side of
/// the FFI of the default LingXi-profile guest paths and workspace format.
/// Seven Swift sites used to restate these as independent string literals.
/// The two twins are byte-pinned against each other:
/// `guest_paths::tests::atlas_atoms_are_pinned` (Rust) and
/// `testGuestPathAtlasMatchesRustTwin` (Swift) assert the SAME literals, so
/// drift on either side fails one of the pins.
public enum LXISHGuestPaths {
    /// The default LingXi guest home, host-backed by the selected profile's
    /// persistent account mount in `LXISHNativeRootfsManager`.
    public static let home = "/home/lingxi"
    public static let adminHome = "/root"
    public static let user = "lingxi"
    public static let uid = 1000
    /// Scratch areas writable inside the guest.
    public static let scratch = ["/tmp", "/var/tmp"]
    /// Parent of every workspace mount for the default LingXi profile.
    public static let workspaceRoot = "/home/lingxi/lingxi"
    /// The default LingXi workspace format.
    public static func workspace(_ stableWorkspaceId: String) -> String {
        "\(workspaceRoot)/\(stableWorkspaceId)"
    }
    public static func home(linuxUser: String) -> String { "/home/\(linuxUser)" }
    public static func workspaceRoot(linuxUser: String, brandId: String) -> String {
        "\(home(linuxUser: linuxUser))/\(brandId)"
    }
    public static func workspace(linuxUser: String, brandId: String, stableWorkspaceId: String) -> String {
        "\(workspaceRoot(linuxUser: linuxUser, brandId: brandId))/\(stableWorkspaceId)"
    }


}

/// Decoded from the JSON that Rust's `NativeConfigPayload`
/// (`platforms/ios-ish-runtime`) writes for every config-bearing call across
/// the C ABI. serde emits Rust field names verbatim — `managed_root`, not
/// `managedRoot` — and this struct did not say so, while `LXISHMountSpec`,
/// `LXISHRunRequest` and the rest all do. Every call that carries a config
/// (install, boot, run, pty_open, …) therefore failed to decode on arrival,
/// and the terminal surfaced the decoder's "The data couldn't be read because
/// it is missing." as its launch failure. `LXISHRootfsStatus` above had the
/// same defect in the response direction; that one was latent, this one was
/// not.
struct LXISHNativeConfig: Codable, Hashable {
    var managedRoot: String
    var brandId: String
    var linuxUser: String
    var configDirName: String
    var workspaceHostPath: String
    var stableWorkspaceId: String
    var abi: String
    var rootfsVersion: String
    var archiveSha256: String?
    var authorizationFile: String?
    var rootfsArchivePath: String?
    var defaultMountPath: String?
    var rootfsPatchPath: String?


    var runtimeKey: String {
        normalizedManagedRoot.path + "\n" + brandId + "\n" + linuxUser + "\n" + configDirName + "\n" + workspaceHostPath + "\n" + stableWorkspaceId
    }

    func sameKernel(as other: LXISHNativeConfig) -> Bool {
        managedRoot == other.managedRoot && brandId == other.brandId && linuxUser == other.linuxUser
            && configDirName == other.configDirName && abi == other.abi && rootfsVersion == other.rootfsVersion
            && archiveSha256 == other.archiveSha256 && authorizationFile == other.authorizationFile
            && rootfsArchivePath == other.rootfsArchivePath && defaultMountPath == other.defaultMountPath
            && rootfsPatchPath == other.rootfsPatchPath
    }

    var normalizedManagedRoot: URL {
        URL(fileURLWithPath: managedRoot, isDirectory: true).standardizedFileURL
    }

    var rootfsURL: URL {
        normalizedManagedRoot.appendingPathComponent("alpine-rootfs", isDirectory: true)
    }

    var rootfsDataURL: URL {
        rootfsURL.appendingPathComponent("data", isDirectory: true)
    }

    var guestHome: String { LXISHGuestPaths.home(linuxUser: linuxUser) }
    var workspaceRoot: String { LXISHGuestPaths.workspaceRoot(linuxUser: linuxUser, brandId: brandId) }
    var guestWorkspace: String {
        LXISHGuestPaths.workspace(linuxUser: linuxUser, brandId: brandId, stableWorkspaceId: stableWorkspaceId)
    }
    var guestConfigHome: String { "\(guestHome)/\(configDirName)" }

    func validateBrandProfile() throws {
        func matches(_ value: String, _ pattern: String) -> Bool {
            value.range(of: pattern, options: .regularExpression) != nil
        }
        guard matches(brandId, "^[a-z0-9][a-z0-9._-]{0,62}$"),
              matches(linuxUser, "^[a-z_][a-z0-9_-]{0,31}$"), linuxUser != "root",
              matches(configDirName, "^\\.[a-z0-9][a-z0-9._-]{0,62}$")
        else {
            throw LXISHRootfsError.corrupt("invalid selected brand runtime profile")
        }
    }

    var persistentHomeURL: URL {
        normalizedManagedRoot.appendingPathComponent("persistent/\(linuxUser)", isDirectory: true)
    }

    var persistentAdminHomeURL: URL {
        normalizedManagedRoot.appendingPathComponent("persistent/root", isDirectory: true)
    }

    var mountsCacheURL: URL {
        normalizedManagedRoot.appendingPathComponent("mounts.json")
    }

    enum CodingKeys: String, CodingKey {
        case managedRoot = "managed_root"
        case brandId = "brand_id"
        case linuxUser = "linux_user"
        case configDirName = "config_dir_name"
        case workspaceHostPath = "workspace_host_path"
        case stableWorkspaceId = "stable_workspace_id"
        case abi
        case rootfsVersion = "rootfs_version"
        case archiveSha256 = "archive_sha256"
        case authorizationFile = "authorization_file"
        case rootfsArchivePath = "rootfs_archive_path"
        case defaultMountPath = "default_mount_path"
        case rootfsPatchPath = "rootfs_patch_path"
    }
}

enum LXISHRootfsError: LocalizedError {
    case archiveMissing
    case corrupt(String)

    var errorDescription: String? {
        switch self {
        case .archiveMissing:
            return "bundled alpine-rootfs.zip not found"
        case let .corrupt(message):
            return message
        }
    }
}

private struct LXISHInstalledRootfsMetadata: Codable {
    var abi: String
    var brandId: String
    var linuxUser: String
    var configDirName: String
    var rootfsVersion: String
    var archiveSha256: String?
    var arch: String
    var updatedAt: String


    enum CodingKeys: String, CodingKey {
        case abi
        case brandId = "brand_id"
        case linuxUser = "linux_user"
        case configDirName = "config_dir_name"
        case rootfsVersion = "rootfs_version"
        case archiveSha256 = "archive_sha256"
        case arch
        case updatedAt = "updated_at"
    }
}

final class LXISHNativeRootfsManager {
    private let fileManager = FileManager.default
    private let currentArch = "aarch64"

    func status(for config: LXISHNativeConfig, lastError: String? = nil) -> LXISHRootfsStatus {
        let profileIsValid = (try? config.validateBrandProfile()) != nil
        let rootfsURL = config.rootfsURL
        let dataURL = config.rootfsDataURL
        let metaDB = rootfsURL.appendingPathComponent("meta.db")
        let installed = fileManager.fileExists(atPath: dataURL.path) && fileManager.fileExists(atPath: metaDB.path)
        let installedMetadata = readInstalledMetadata(for: config)
        let metadataMatches = metadataMatches(installedMetadata, config: config)
        let accountMatches = guestAccountMatches(at: config.rootfsDataURL, config: config)
        let archMatches = readArchTag(at: rootfsURL) == currentArch
        let state: String
        if !fileManager.fileExists(atPath: rootfsURL.path) {
            state = "missing"
        } else if profileIsValid && installed && archMatches && metadataMatches && accountMatches {
            state = "ready"
        } else {
            state = "corrupt"
        }

        return LXISHRootfsStatus(
            state: state,
            backend: "ios-ish",
            mode: "mobileLinux",
            platform: "ios",
            abi: config.abi,
            version: installedMetadata?.rootfsVersion ?? config.rootfsVersion,
            managedRoot: config.normalizedManagedRoot.path,
            activeRoot: state == "ready" ? rootfsURL.path : nil,
            stagedRoot: nil,
            archiveSha256: installedMetadata?.archiveSha256 ?? config.archiveSha256,
            installedSizeBytes: state == "ready" ? directorySize(at: rootfsURL) : nil,
            writableGuestPaths: [config.workspaceRoot] + LXISHGuestPaths.scratch + [config.guestHome, LXISHGuestPaths.adminHome],
            lastError: lastError ?? (!profileIsValid ? "invalid selected brand runtime profile" : (!accountMatches && installed ? "rootfs account database does not match selected brand profile" : nil))
        )
    }

    @discardableResult
    func installIfNeeded(for config: LXISHNativeConfig) throws -> LXISHRootfsStatus {
        try config.validateBrandProfile()
        try fileManager.createDirectory(at: config.normalizedManagedRoot, withIntermediateDirectories: true)
        try migrateLegacyHomeIfNeeded(for: config)
        try ensurePersistentHome(for: config)
        let existing = status(for: config)
        if existing.state == "ready" {
            try ensureGuestDirectories(for: config)
            return status(for: config)
        }
        guard let zipURL = resolveBundledArchive(for: config) else {
            throw LXISHRootfsError.archiveMissing
        }
        try verifyArchiveHashIfNeeded(for: config, zipURL: zipURL)

        let stagingURL = config.normalizedManagedRoot
            .appendingPathComponent("alpine-rootfs.staging-\(UUID().uuidString.lowercased())", isDirectory: true)

        do {
            if fileManager.fileExists(atPath: stagingURL.path) {
                try fileManager.removeItem(at: stagingURL)
            }
            try fileManager.createDirectory(at: stagingURL, withIntermediateDirectories: true)
            try LXISHZipArchive(url: zipURL).extractAll(to: stagingURL, strippingPrefix: "alpine-rootfs/")
            try currentArch.write(
                to: stagingURL.appendingPathComponent(".arch"),
                atomically: true,
                encoding: .utf8
            )
            try ensureGuestDirectories(at: stagingURL.appendingPathComponent("data", isDirectory: true), for: config)
            try applyDefaultMountOverlay(for: config, into: stagingURL.appendingPathComponent("data", isDirectory: true))
            try persistInstalledMetadata(for: config, rootfsURL: stagingURL)
            try replaceInstalledRootfs(at: config.rootfsURL, with: stagingURL)
        } catch {
            try? fileManager.removeItem(at: stagingURL)
            throw error
        }
        return status(for: config)
    }

    func repair(for config: LXISHNativeConfig) throws -> LXISHRootfsStatus {
        try config.validateBrandProfile()
        let current = status(for: config)
        if current.state == "ready" {
            try ensureGuestDirectories(for: config)
            return status(for: config)
        }
        return try installIfNeeded(for: config)
    }

    @discardableResult
    func reset(for config: LXISHNativeConfig) throws -> LXISHRootfsStatus {
        try migrateLegacyHomeIfNeeded(for: config)
        if fileManager.fileExists(atPath: config.rootfsURL.path) {
            try fileManager.removeItem(at: config.rootfsURL)
        }
        try? fileManager.removeItem(at: config.mountsCacheURL)
        return status(for: config)
    }

    /// Request mounts contain installation-specific absolute paths (including
    /// the app bundle UUID) and per-job build directories. Replaying those
    /// paths on a later boot can fail before the current request has a chance
    /// to install its valid mounts. Stable home/workspace mounts are rebuilt
    /// from `LXISHNativeConfig`, so no request mount belongs on disk.
    func mountsForBoot(for config: LXISHNativeConfig) throws -> [LXISHMountSpec] {
        if fileManager.fileExists(atPath: config.mountsCacheURL.path) {
            try fileManager.removeItem(at: config.mountsCacheURL)
        }
        return []
    }

    private func readArchTag(at rootfsURL: URL) -> String? {
        let tagURL = rootfsURL.appendingPathComponent(".arch")
        return try? String(contentsOf: tagURL, encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func resolveBundledArchive(for config: LXISHNativeConfig) -> URL? {
        guard let path = config.rootfsArchivePath, path.hasPrefix("/") else { return nil }
        let url = URL(fileURLWithPath: path)
        return fileManager.fileExists(atPath: url.path) ? url : nil
    }

    private func resolveDefaultMountDirectory(for config: LXISHNativeConfig) -> URL? {
        guard let path = config.defaultMountPath, path.hasPrefix("/") else { return nil }
        let url = URL(fileURLWithPath: path, isDirectory: true)
        return fileManager.fileExists(atPath: url.path) ? url : nil
    }

    private func ensureGuestDirectories(for config: LXISHNativeConfig) throws {
        try ensureGuestDirectories(at: config.rootfsDataURL, for: config)
    }

    private func ensureGuestDirectories(at dataRoot: URL, for config: LXISHNativeConfig) throws {
        let directories = [
            "tmp", "var/tmp", String(config.guestHome.dropFirst()),
            String(config.workspaceRoot.dropFirst()),
            String(config.workspaceRoot.dropFirst()) + "/projects",
            String(config.guestConfigHome.dropFirst()),
            String(config.guestConfigHome.dropFirst()) + "/.apps"
        ]
        for relative in directories {
            try fileManager.createDirectory(
                at: dataRoot.appendingPathComponent(relative, isDirectory: true),
                withIntermediateDirectories: true
            )
        }
        try ensureGuestAccount(at: dataRoot, config: config)
    }

    private func ensureGuestAccount(at dataRoot: URL, config: LXISHNativeConfig) throws {
        try config.validateBrandProfile()
        for (relative, idField) in [("etc/passwd", 2), ("etc/group", 2)] {
            let file = dataRoot.appendingPathComponent(relative)
            if let attributes = try? fileManager.attributesOfItem(atPath: file.path),
               attributes[.type] as? FileAttributeType == .typeSymbolicLink {
                throw LXISHRootfsError.corrupt("guest account file is a symlink: \(relative)")
            }
            let existing = (try? String(contentsOf: file, encoding: .utf8)) ?? ""
            let identityRows = existing.split(whereSeparator: \.isNewline).filter { line in
                let fields = line.split(separator: ":", omittingEmptySubsequences: false)
                return fields.count > idField && fields[idField] == "1000"
            }
            guard identityRows.count <= 1,
                  identityRows.first.map({
                      $0.split(separator: ":", omittingEmptySubsequences: false).first.map(String.init)
                          == config.linuxUser
                  }) ?? true
            else {
                throw LXISHRootfsError.corrupt(
                    "rootfs UID/GID 1000 belongs to a different account than selected brand \(config.brandId)"
                )
            }
        }
        let accountFiles: [(String, String, NSNumber)] = [
            ("etc/passwd", "\(config.linuxUser):x:1000:1000:\(config.brandId):\(config.guestHome):/bin/sh", NSNumber(value: 0o644)),
            ("etc/group", "\(config.linuxUser):x:1000:", NSNumber(value: 0o644)),
            ("etc/shadow", "\(config.linuxUser):!:0:0:99999:7:::", NSNumber(value: 0o640))
        ]
        for (relative, record, mode) in accountFiles {
            let file = dataRoot.appendingPathComponent(relative)
            try fileManager.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            if let attributes = try? fileManager.attributesOfItem(atPath: file.path),
               attributes[.type] as? FileAttributeType == .typeSymbolicLink {
                throw LXISHRootfsError.corrupt("guest account file is a symlink: \(relative)")
            }
            let existing = (try? String(contentsOf: file, encoding: .utf8)) ?? ""
            let retained = existing.split(whereSeparator: \.isNewline).filter {
                $0.split(separator: ":", omittingEmptySubsequences: false).first.map(String.init) != config.linuxUser
            }
            let content = (retained.map(String.init) + [record]).joined(separator: "\n") + "\n"
            try Data(content.utf8).write(to: file, options: .atomic)
            try fileManager.setAttributes([.posixPermissions: mode], ofItemAtPath: file.path)
        }
        let doas = dataRoot.appendingPathComponent("etc/doas.conf")
        try fileManager.createDirectory(at: doas.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("permit nopass \(config.linuxUser) as root\n".utf8).write(to: doas, options: .atomic)
        try fileManager.setAttributes([.posixPermissions: NSNumber(value: 0o600)], ofItemAtPath: doas.path)
    }

    private func guestAccountMatches(at dataRoot: URL, config: LXISHNativeConfig) -> Bool {
        func contents(_ relative: String) -> String? {
            let file = dataRoot.appendingPathComponent(relative)
            guard (try? fileManager.attributesOfItem(atPath: file.path))?[.type] as? FileAttributeType != .typeSymbolicLink
            else { return nil }
            return try? String(contentsOf: file, encoding: .utf8)
        }
        let passwdRows = contents("etc/passwd")?.split(whereSeparator: \.isNewline) ?? []
        let passwdUidRows = passwdRows.filter { line in
            let fields = line.split(separator: ":", omittingEmptySubsequences: false)
            return fields.count > 2 && fields[2] == "1000"
        }
        let selectedPasswdRows = passwdRows.filter { line in
            line.split(separator: ":", omittingEmptySubsequences: false).first.map(String.init) == config.linuxUser
        }
        let passwd = passwdUidRows.count == 1 && selectedPasswdRows.count == 1 && selectedPasswdRows.contains { line in
            let fields = line.split(separator: ":", omittingEmptySubsequences: false)
            return fields.count >= 7 && fields[0] == config.linuxUser && fields[2] == "1000"
                && fields[3] == "1000" && fields[5] == config.guestHome && fields[6] == "/bin/sh"
        }
        let groupRows = contents("etc/group")?.split(whereSeparator: \.isNewline) ?? []
        let groupIdRows = groupRows.filter { line in
            let fields = line.split(separator: ":", omittingEmptySubsequences: false)
            return fields.count > 2 && fields[2] == "1000"
        }
        let selectedGroupRows = groupRows.filter { line in
            line.split(separator: ":", omittingEmptySubsequences: false).first.map(String.init) == config.linuxUser
        }
        let group = groupIdRows.count == 1 && selectedGroupRows.count == 1 && selectedGroupRows.contains { line in
            let fields = line.split(separator: ":", omittingEmptySubsequences: false)
            return fields.count >= 3 && fields[0] == config.linuxUser && fields[2] == "1000"
        }
        let selectedShadowRows = contents("etc/shadow")?.split(whereSeparator: \.isNewline).filter { line in
            line.split(separator: ":", omittingEmptySubsequences: false).first.map(String.init) == config.linuxUser
        } ?? []
        let shadow = selectedShadowRows.count == 1
        let doas = contents("etc/doas.conf")?.trimmingCharacters(in: .whitespacesAndNewlines)
            == "permit nopass \(config.linuxUser) as root"
        return passwd && group && shadow && doas
    }

    private func applyDefaultMountOverlay(for config: LXISHNativeConfig, into rootfsDataURL: URL) throws {
        guard let overlayURL = resolveDefaultMountDirectory(for: config) else {
            return
        }
        guard let enumerator = fileManager.enumerator(at: overlayURL, includingPropertiesForKeys: [.isDirectoryKey]) else {
            return
        }
        for case let sourceURL as URL in enumerator {
            let values = try sourceURL.resourceValues(forKeys: [.isDirectoryKey])
            guard let relativePath = Self.relativePath(of: sourceURL, under: overlayURL) else { continue }
            let destinationURL = rootfsDataURL.appendingPathComponent(relativePath)
            if values.isDirectory == true {
                try fileManager.createDirectory(at: destinationURL, withIntermediateDirectories: true)
                continue
            }
            try fileManager.createDirectory(at: destinationURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            if fileManager.fileExists(atPath: destinationURL.path) {
                try fileManager.removeItem(at: destinationURL)
            }
            try fileManager.copyItem(at: sourceURL, to: destinationURL)
        }
    }

    private func ensurePersistentHome(for config: LXISHNativeConfig) throws {
        try config.validateBrandProfile()
        try fileManager.createDirectory(at: config.persistentHomeURL, withIntermediateDirectories: true)
        try fileManager.createDirectory(at: config.persistentAdminHomeURL, withIntermediateDirectories: true)
        let childDirectories = [
            ".cache", ".cache/pip", ".npm", ".local", config.configDirName,
            "\(config.configDirName)/.apps", config.brandId,
            "\(config.brandId)/projects", "\(config.brandId)/scheduled"
        ]
        for relative in childDirectories {
            try fileManager.createDirectory(
                at: config.persistentHomeURL.appendingPathComponent(relative, isDirectory: true),
                withIntermediateDirectories: true
            )
        }
    }

    private func migrateLegacyHomeIfNeeded(for config: LXISHNativeConfig) throws {
        let oldUser = try legacyActiveLinuxUser(for: config)
        let migrations: [(String, URL, URL)] = [
            ("/root", config.rootfsDataURL.appendingPathComponent("root", isDirectory: true), config.persistentAdminHomeURL),
            ("/home/\(oldUser)", config.rootfsDataURL.appendingPathComponent("home/\(oldUser)", isDirectory: true),
             config.normalizedManagedRoot.appendingPathComponent("persistent/\(oldUser)", isDirectory: true))
        ]
        for (guestPath, source, destination) in migrations where fileManager.fileExists(atPath: source.path) {
            try mergeHome(from: source, into: destination)
            let recordDirectory = config.normalizedManagedRoot.appendingPathComponent("persistent/migrations", isDirectory: true)
            try fileManager.createDirectory(at: recordDirectory, withIntermediateDirectories: true)
            let key = guestPath.trimmingCharacters(in: CharacterSet(charactersIn: "/")).replacingOccurrences(of: "/", with: "-")
            let record = recordDirectory.appendingPathComponent("guest-home-v1-\(key).json")
            let payload: [String: Any] = [
                "schema_version": 1,
                "source_guest_path": guestPath,
                "destination_managed_relative_path": "persistent/\(guestPath == "/root" ? "root" : oldUser)",
                "source_retained": true
            ]
            let data = try JSONSerialization.data(withJSONObject: payload, options: [.prettyPrinted, .sortedKeys])
            try data.write(to: record, options: .atomic)
        }
    }

    private func legacyActiveLinuxUser(for config: LXISHNativeConfig) throws -> String {
        if let metadata = readInstalledMetadata(for: config),
           metadata.linuxUser.range(of: "^[a-z_][a-z0-9_-]{0,31}$", options: .regularExpression) != nil,
           metadata.linuxUser != "root" {
            return metadata.linuxUser
        }
        let passwd = config.rootfsDataURL.appendingPathComponent("etc/passwd")
        guard let text = try? String(contentsOf: passwd, encoding: .utf8) else { return "lingxi" }
        let users = text.split(whereSeparator: \.isNewline).compactMap { line -> String? in
            let fields = line.split(separator: ":", omittingEmptySubsequences: false)
            guard fields.count >= 7, fields[2] == "1000", fields[3] == "1000",
                  fields[5].hasPrefix("/home/") else { return nil }
            let user = String(fields[0])
            return user.range(of: "^[a-z_][a-z0-9_-]{0,31}$", options: .regularExpression) != nil && user != "root"
                ? user : nil
        }
        if users.count > 1 {
            throw LXISHRootfsError.corrupt("active rootfs has ambiguous uid-1000 accounts; refusing home migration")
        }
        return users.first ?? "lingxi"
    }

    /// Copy once before replacing the image. Existing user files and symlinks
    /// win, and interrupted copies can be retried without changing the source.
    private func mergeHome(from source: URL, into destination: URL) throws {
        guard fileManager.fileExists(atPath: source.path),
              source.resolvingSymlinksInPath() != destination.resolvingSymlinksInPath() else { return }
        try fileManager.createDirectory(at: destination, withIntermediateDirectories: true)
        for child in try fileManager.contentsOfDirectory(at: source, includingPropertiesForKeys: [.isDirectoryKey, .isSymbolicLinkKey]) {
            let target = destination.appendingPathComponent(child.lastPathComponent)
            let values = try child.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey])
            let existing = try? fileManager.attributesOfItem(atPath: target.path)
            if values.isDirectory == true && values.isSymbolicLink != true {
                if existing == nil || existing?[.type] as? FileAttributeType == .typeDirectory {
                    try mergeHome(from: child, into: target)
                }
            } else if existing == nil {
                try fileManager.copyItem(at: child, to: target)
            }
        }
    }

    private func replaceInstalledRootfs(at activeURL: URL, with stagedURL: URL) throws {
        guard fileManager.fileExists(atPath: activeURL.path) else {
            try fileManager.moveItem(at: stagedURL, to: activeURL)
            return
        }
        let backupName = "alpine-rootfs.backup-\(UUID().uuidString.lowercased())"
        let backupURL = activeURL.deletingLastPathComponent().appendingPathComponent(backupName, isDirectory: true)
        _ = try fileManager.replaceItemAt(activeURL, withItemAt: stagedURL, backupItemName: backupName)
        try? fileManager.removeItem(at: backupURL)
    }

    private func persistInstalledMetadata(for config: LXISHNativeConfig, rootfsURL: URL) throws {
        let payload = LXISHInstalledRootfsMetadata(
            abi: config.abi,
            brandId: config.brandId,
            linuxUser: config.linuxUser,
            configDirName: config.configDirName,
            rootfsVersion: config.rootfsVersion,
            archiveSha256: config.archiveSha256,
            arch: currentArch,
            updatedAt: ISO8601DateFormatter().string(from: Date())
        )
        let data = try LXISHBridgeJSON.encoder().encode(payload)
        try data.write(
            to: rootfsURL.appendingPathComponent("bridge-state.json"),
            options: .atomic
        )
    }

    private func readInstalledMetadata(for config: LXISHNativeConfig) -> LXISHInstalledRootfsMetadata? {
        let metadataURL = config.rootfsURL.appendingPathComponent("bridge-state.json")
        guard let data = try? Data(contentsOf: metadataURL) else {
            return nil
        }
        return try? LXISHBridgeJSON.decoder().decode(LXISHInstalledRootfsMetadata.self, from: data)
    }

    private func metadataMatches(_ metadata: LXISHInstalledRootfsMetadata?, config: LXISHNativeConfig) -> Bool {
        guard let metadata else { return false }
        guard metadata.arch == currentArch,
              metadata.abi == config.abi,
              metadata.brandId == config.brandId,
              metadata.linuxUser == config.linuxUser,
              metadata.configDirName == config.configDirName,
              metadata.rootfsVersion == config.rootfsVersion
        else {
            return false
        }
        return metadata.archiveSha256 == config.archiveSha256
    }

    private func verifyArchiveHashIfNeeded(for config: LXISHNativeConfig, zipURL: URL) throws {
        guard let expected = normalizedExpectedArchiveHash(config.archiveSha256)
        else {
            throw LXISHRootfsError.corrupt(
                "runtime manifest is missing a valid 64-character alpine-rootfs.zip SHA-256"
            )
        }
        let archiveData = try Data(contentsOf: zipURL, options: [.mappedIfSafe])
        let actual = SHA256.hash(data: archiveData).map { String(format: "%02x", $0) }.joined()
        guard actual == expected else {
            throw LXISHRootfsError.corrupt(
                "bundled alpine-rootfs.zip SHA-256 mismatch: expected \(expected), got \(actual)"
            )
        }
    }

    private func normalizedExpectedArchiveHash(_ value: String?) -> String? {
        guard let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased(),
              trimmed.count == 64,
              trimmed.unicodeScalars.allSatisfy({ scalar in
                  CharacterSet(charactersIn: "0123456789abcdef").contains(scalar)
              })
        else {
            return nil
        }
        return trimmed
    }

    /// Component-wise path of `url` relative to `base`, or nil when `url` does
    /// not sit beneath it.
    ///
    /// `FileManager.enumerator` can hand back URLs whose prefix differs from
    /// the base by symlink resolution (`/var` versus `/private/var` on iOS), so
    /// subtracting the base as a string leaves the entry's own absolute path in
    /// place and copies it to a garbage destination instead. Matching whole path
    /// components against both spellings keeps a mismatch a skip rather than a
    /// stray write — which matters most for the legacy `/root` migration, whose
    /// source is destroyed by the staged rootfs replacement that follows it.
    private static func relativePath(of url: URL, under base: URL) -> String? {
        let entry = url.standardizedFileURL.pathComponents
        let candidates = [
            base.standardizedFileURL,
            base.standardizedFileURL.resolvingSymlinksInPath()
        ]
        for candidate in candidates {
            let prefix = candidate.pathComponents
            guard entry.count > prefix.count,
                  Array(entry.prefix(prefix.count)) == prefix
            else {
                continue
            }
            return entry.dropFirst(prefix.count).joined(separator: "/")
        }
        return nil
    }

    private func isDirectoryEmpty(_ url: URL) -> Bool {
        guard let enumerator = fileManager.enumerator(
            at: url,
            includingPropertiesForKeys: [.isDirectoryKey],
            options: [.skipsHiddenFiles]
        ) else {
            return true
        }
        return enumerator.nextObject() == nil
    }

    private func directorySize(at url: URL) -> UInt64 {
        guard let enumerator = fileManager.enumerator(at: url, includingPropertiesForKeys: [.fileSizeKey]) else {
            return 0
        }
        var total: UInt64 = 0
        for case let fileURL as URL in enumerator {
            let values = try? fileURL.resourceValues(forKeys: [.fileSizeKey])
            let size = values?.fileSize ?? 0
            total += UInt64(max(size, 0))
        }
        return total
    }
}

private final class LXISHZipArchive {
    struct Entry {
        let path: String
        let compressedSize: UInt32
        let uncompressedSize: UInt32
        let compressionMethod: UInt16
        let localHeaderOffset: UInt32
        let isDirectory: Bool
    }

    private let fileHandle: FileHandle
    private(set) var entries: [Entry] = []

    init(url: URL) throws {
        fileHandle = try FileHandle(forReadingFrom: url)
        try parse()
    }

    deinit {
        try? fileHandle.close()
    }

    func extractAll(to destinationURL: URL, strippingPrefix prefix: String) throws {
        let fileManager = FileManager.default
        let destinationRoot = destinationURL.standardizedFileURL
        for entry in entries {
            var relativePath = entry.path
            if relativePath.hasPrefix(prefix) {
                relativePath = String(relativePath.dropFirst(prefix.count))
            }
            guard !relativePath.isEmpty else { continue }
            let components = relativePath.split(separator: "/", omittingEmptySubsequences: true)
            guard !relativePath.hasPrefix("/"),
                  !components.isEmpty,
                  !components.contains(where: { $0 == ".." || $0 == "." })
            else {
                throw LXISHRootfsError.corrupt("unsafe zip entry path: \(entry.path)")
            }
            let outputURL = destinationRoot.appendingPathComponent(relativePath).standardizedFileURL
            guard outputURL.path.hasPrefix(destinationRoot.path + "/") else {
                throw LXISHRootfsError.corrupt("zip entry escapes managed root: \(entry.path)")
            }
            if entry.isDirectory {
                try fileManager.createDirectory(at: outputURL, withIntermediateDirectories: true)
                continue
            }
            try fileManager.createDirectory(at: outputURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            let data = try extractData(for: entry)
            try data.write(to: outputURL, options: .atomic)
        }
    }

    private func parse() throws {
        fileHandle.seekToEndOfFile()
        let fileSize = fileHandle.offsetInFile
        let searchSize = min(fileSize, 65_557)
        try fileHandle.seek(toOffset: fileSize - searchSize)
        let searchData = try fileHandle.read(upToCount: Int(searchSize)) ?? Data()
        guard let eocdOffset = findEOCD(in: searchData) else {
            throw LXISHRootfsError.corrupt("invalid zip archive")
        }
        let eocdStart = Int(fileSize - searchSize) + eocdOffset
        try fileHandle.seek(toOffset: UInt64(eocdStart))
        let eocdData = try require(count: 22)
        let entryCount = readUInt16(eocdData, at: 10)
        let centralDirOffset = readUInt32(eocdData, at: 16)
        try fileHandle.seek(toOffset: UInt64(centralDirOffset))
        for _ in 0..<entryCount {
            guard let entry = try readCentralDirectoryEntry() else { break }
            entries.append(entry)
        }
    }

    private func extractData(for entry: Entry) throws -> Data {
        try fileHandle.seek(toOffset: UInt64(entry.localHeaderOffset))
        let localHeader = try require(count: 30)
        let fileNameLength = readUInt16(localHeader, at: 26)
        let extraLength = readUInt16(localHeader, at: 28)
        try fileHandle.seek(
            toOffset: UInt64(entry.localHeaderOffset) + 30 + UInt64(fileNameLength) + UInt64(extraLength)
        )
        let payload = try require(count: Int(entry.compressedSize))
        let data: Data
        switch entry.compressionMethod {
        case 0:
            data = payload
        case 8:
            data = try (payload as NSData).decompressed(using: .zlib) as Data
        default:
            throw LXISHRootfsError.corrupt("unsupported zip compression method \(entry.compressionMethod)")
        }
        guard data.count == Int(entry.uncompressedSize) else {
            throw LXISHRootfsError.corrupt("zip entry size mismatch: \(entry.path)")
        }
        return data
    }

    private func readCentralDirectoryEntry() throws -> Entry? {
        let header = try require(count: 46)
        guard readUInt32(header, at: 0) == 0x02014b50 else { return nil }
        let compressionMethod = readUInt16(header, at: 10)
        let compressedSize = readUInt32(header, at: 20)
        let uncompressedSize = readUInt32(header, at: 24)
        let fileNameLength = readUInt16(header, at: 28)
        let extraLength = readUInt16(header, at: 30)
        let commentLength = readUInt16(header, at: 32)
        let localHeaderOffset = readUInt32(header, at: 42)
        let fileNameData = try require(count: Int(fileNameLength))
        let fileName = String(data: fileNameData, encoding: .utf8) ?? ""
        try fileHandle.seek(
            toOffset: fileHandle.offsetInFile + UInt64(extraLength) + UInt64(commentLength)
        )
        return Entry(
            path: fileName,
            compressedSize: compressedSize,
            uncompressedSize: uncompressedSize,
            compressionMethod: compressionMethod,
            localHeaderOffset: localHeaderOffset,
            isDirectory: fileName.hasSuffix("/")
        )
    }

    private func findEOCD(in data: Data) -> Int? {
        guard data.count >= 22 else { return nil }
        for index in stride(from: data.count - 22, through: 0, by: -1) {
            if readUInt32(data, at: index) == 0x06054b50 {
                return index
            }
        }
        return nil
    }

    private func require(count: Int) throws -> Data {
        let data = try fileHandle.read(upToCount: count) ?? Data()
        guard data.count == count else {
            throw LXISHRootfsError.corrupt("unexpected end of archive")
        }
        return data
    }

    private func readUInt16(_ data: Data, at offset: Int) -> UInt16 {
        UInt16(data[offset]) | (UInt16(data[offset + 1]) << 8)
    }

    private func readUInt32(_ data: Data, at offset: Int) -> UInt32 {
        UInt32(data[offset])
            | (UInt32(data[offset + 1]) << 8)
            | (UInt32(data[offset + 2]) << 16)
            | (UInt32(data[offset + 3]) << 24)
    }
}
