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
/// the FFI of the well-known guest paths and the `/workspace/<id>` format.
/// Seven Swift sites used to restate these as independent string literals.
/// The two twins are byte-pinned against each other:
/// `guest_paths::tests::atlas_atoms_are_pinned` (Rust) and
/// `testGuestPathAtlasMatchesRustTwin` (Swift) assert the SAME literals, so
/// drift on either side fails one of the pins.
public enum LXISHGuestPaths {
    /// The guest home (`~` of the interactive shell), host-backed by the
    /// rootfs manager's `persistent/root` bind mount.
    public static let home = "/root"
    /// Scratch areas writable inside the guest.
    public static let scratch = ["/tmp", "/var/tmp"]
    /// Parent of every per-workspace mount.
    public static let workspaceRoot = "/workspace"
    /// THE `/workspace/<id>` format.
    public static func workspace(_ stableWorkspaceId: String) -> String {
        "\(workspaceRoot)/\(stableWorkspaceId)"
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
        normalizedManagedRoot.path + "\n" + workspaceHostPath + "\n" + stableWorkspaceId
    }

    func sameKernel(as other: LXISHNativeConfig) -> Bool {
        managedRoot == other.managedRoot && abi == other.abi && rootfsVersion == other.rootfsVersion
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

    var persistentHomeURL: URL {
        normalizedManagedRoot.appendingPathComponent("persistent/root", isDirectory: true)
    }

    var mountsCacheURL: URL {
        normalizedManagedRoot.appendingPathComponent("mounts.json")
    }

    enum CodingKeys: String, CodingKey {
        case managedRoot = "managed_root"
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
    var rootfsVersion: String
    var archiveSha256: String?
    var arch: String
    var updatedAt: String


    enum CodingKeys: String, CodingKey {
        case abi
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
        let rootfsURL = config.rootfsURL
        let dataURL = config.rootfsDataURL
        let metaDB = rootfsURL.appendingPathComponent("meta.db")
        let installed = fileManager.fileExists(atPath: dataURL.path) && fileManager.fileExists(atPath: metaDB.path)
        let installedMetadata = readInstalledMetadata(for: config)
        let metadataMatches = metadataMatches(installedMetadata, config: config)
        let archMatches = readArchTag(at: rootfsURL) == currentArch
        let state: String
        if !fileManager.fileExists(atPath: rootfsURL.path) {
            state = "missing"
        } else if installed && archMatches && metadataMatches {
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
            writableGuestPaths: [LXISHGuestPaths.workspace(config.stableWorkspaceId)] + LXISHGuestPaths.scratch + [LXISHGuestPaths.home],
            lastError: lastError
        )
    }

    @discardableResult
    func installIfNeeded(for config: LXISHNativeConfig) throws -> LXISHRootfsStatus {
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
        let current = status(for: config)
        if current.state == "ready" {
            try ensureGuestDirectories(for: config)
            return status(for: config)
        }
        return try installIfNeeded(for: config)
    }

    @discardableResult
    func reset(for config: LXISHNativeConfig) throws -> LXISHRootfsStatus {
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
            "workspace/\(config.stableWorkspaceId)",
            "tmp",
            "var/tmp"
        ]
        for relative in directories {
            try fileManager.createDirectory(
                at: dataRoot.appendingPathComponent(relative, isDirectory: true),
                withIntermediateDirectories: true
            )
        }
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
        try fileManager.createDirectory(at: config.persistentHomeURL, withIntermediateDirectories: true)
        let childDirectories = [".cache", ".cache/pip", ".npm", ".local"]
        for relative in childDirectories {
            try fileManager.createDirectory(
                at: config.persistentHomeURL.appendingPathComponent(relative, isDirectory: true),
                withIntermediateDirectories: true
            )
        }
    }

    private func migrateLegacyHomeIfNeeded(for config: LXISHNativeConfig) throws {
        let legacyHomeURL = config.rootfsDataURL.appendingPathComponent("root", isDirectory: true)
        guard fileManager.fileExists(atPath: legacyHomeURL.path) else { return }
        guard isDirectoryEmpty(config.persistentHomeURL) else { return }
        guard let enumerator = fileManager.enumerator(at: legacyHomeURL, includingPropertiesForKeys: [.isDirectoryKey]) else {
            return
        }
        for case let sourceURL as URL in enumerator {
            let values = try sourceURL.resourceValues(forKeys: [.isDirectoryKey])
            guard let relativePath = Self.relativePath(of: sourceURL, under: legacyHomeURL) else { continue }
            let destinationURL = config.persistentHomeURL.appendingPathComponent(relativePath)
            if values.isDirectory == true {
                try fileManager.createDirectory(at: destinationURL, withIntermediateDirectories: true)
                continue
            }
            try fileManager.createDirectory(at: destinationURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            if fileManager.fileExists(atPath: destinationURL.path) {
                continue
            }
            try fileManager.copyItem(at: sourceURL, to: destinationURL)
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
