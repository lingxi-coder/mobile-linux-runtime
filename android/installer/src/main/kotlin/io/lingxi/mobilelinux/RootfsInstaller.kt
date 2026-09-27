package io.lingxi.mobilelinux

import org.apache.commons.compress.archivers.tar.TarArchiveEntry
import org.apache.commons.compress.archivers.tar.TarArchiveInputStream
import org.apache.commons.compress.compressors.gzip.GzipCompressorInputStream
import java.io.BufferedInputStream
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.nio.file.attribute.BasicFileAttributes
import java.security.MessageDigest
import java.util.ArrayDeque
import java.util.UUID

private const val ROOTFS_MANIFEST_FILE = "rootfs-manifest.json"
private const val ROOTFS_SBOM_FILE = "rootfs.spdx.json"
private const val ROOTFS_SOURCE_PINS_FILE = "mobile-linux-pins.json"
private val ROOTFS_ALLOWED_WRITABLE_PATHS = linkedSetOf("/root", "/tmp", "/var/tmp", "/workspace")
private const val MAX_ROOTFS_SYMLINK_EXPANSIONS = 40

private fun manifestAbiFor(abi: String): String =
    when (abi) {
        "arm64-v8a" -> "arm64"
        "x86_64" -> "x86_64"
        else -> error("Unsupported rootfs ABI for manifest validation: $abi")
    }

public object RootfsInstaller {
    data class StageResult(
        val stagedRoot: File,
        val manifestPath: File,
        val verifiedFiles: Int,
        val verifiedBytes: Long,
    )

    fun stage(
        managedRoot: File,
        expectedRootfsVersion: String,
        expectedArchiveSha: String,
        expectedManifestAbi: String,
        archiveName: String,
        manifestJson: String,
        sbomJson: String,
        copyArchive: (File) -> Unit,
        persistManifest: ((String) -> Unit)? = null,
    ): StageResult {
        // Reject unsafe identity before creating directories or invoking writers.
        requireSafeComponent(expectedRootfsVersion, "Rootfs version")
        requireSafeComponent(archiveName, "Rootfs archive name")
        check(managedRoot.mkdirs() || managedRoot.isDirectory) {
            "Unable to prepare managed root ${managedRoot.path}"
        }
        val manifestTarget = File(managedRoot, ROOTFS_MANIFEST_FILE)
        val writeManifest = persistManifest ?: { text: String ->
            atomicWriteText(manifestTarget, text)
        }

        val downloads = File(managedRoot, "downloads").also {
            check(it.mkdirs() || it.isDirectory) { "Unable to prepare rootfs download cache" }
        }
        val archive = File(downloads, "$archiveName.partial")
        var publishedRoot: File? = null
        try {
            copyArchive(archive)
            val actualArchiveSha = archive.sha256File()
            check(actualArchiveSha == expectedArchiveSha) {
                "Bundled rootfs SHA-256 mismatch for $archiveName"
            }
            val manifest = parseManifest(
                raw = manifestJson,
                expectedRootfsVersion = expectedRootfsVersion,
                expectedAbi = expectedManifestAbi,
                expectedArchiveName = archiveName,
                actualArchiveSha = actualArchiveSha,
                actualArchiveSize = archive.length(),
            )
            validateSbom(manifest, sbomJson)

            val stagedParent = File(managedRoot, "staged").also {
                check(it.mkdirs() || it.isDirectory) { "Unable to prepare rootfs staging root" }
            }
            val destination = File(stagedParent, manifest.rootfsVersion).canonicalFile
            check(destination.parentFile == stagedParent.canonicalFile && destination != stagedParent.canonicalFile) {
                "Unsafe staged rootfs destination: ${destination.path}"
            }
            val temporary = File(stagedParent, ".${manifest.rootfsVersion}-${UUID.randomUUID()}")
            check(temporary.mkdirs()) { "Unable to create rootfs staging directory" }
            try {
                val extracted = extractTarGz(archive, temporary)
                val inventory = collectImmutableInventory(
                    root = temporary.toPath(),
                    guestSymlinkPayloads = extracted.guestSymlinkPayloads,
                )
                val actualContentSha = canonicalInventorySha256(inventory.entries)
                check(inventory.entries == manifest.immutableFiles) {
                    "Bundled rootfs content does not match immutable inventory"
                }
                check(actualContentSha == manifest.contentSha256) {
                    "Bundled rootfs content SHA-256 mismatch"
                }
                check(Files.isExecutable(temporary.toPath().resolve("bin/sh"))) {
                    "Staged rootfs is missing executable /bin/sh"
                }
                if (destination.exists()) {
                    check(destination.deleteRecursively()) {
                        "Unable to clear stale staged rootfs at ${destination.path}"
                    }
                }
                check(temporary.renameTo(destination)) {
                    "Unable to atomically publish staged rootfs"
                }
                publishedRoot = destination
                writeManifest(manifest.rawJson)
                return StageResult(
                    stagedRoot = destination,
                    manifestPath = manifestTarget,
                    verifiedFiles = inventory.entries.size,
                    verifiedBytes = inventory.totalBytes,
                )
            } catch (failure: Throwable) {
                temporary.deleteRecursively()
                throw failure
            }
        } catch (failure: Throwable) {
            publishedRoot?.deleteRecursively()
            throw failure
        } finally {
            archive.delete()
        }
    }

    private fun parseManifest(
        raw: String,
        expectedRootfsVersion: String,
        expectedAbi: String,
        expectedArchiveName: String,
        actualArchiveSha: String,
        actualArchiveSize: Long,
    ): RootfsManifestData {
        val manifest = parseJsonObject(raw, "rootfs manifest")
        requireExactKeys(
            manifest,
            setOf(
                "schema_version",
                "runtime",
                "platform",
                "abi",
                "rootfs_version",
                "content_sha256",
                "sbom_filename",
                "source_pins_filename",
                "archive",
                "packages",
                "executable_allowlist",
                "immutable_files",
                "writable_paths",
            ),
            "rootfs manifest",
        )
        check(manifest.requireLong("schema_version", "rootfs manifest") == 2L) {
            "Unsupported rootfs manifest schema version"
        }
        val runtime = manifest.requireString("runtime", "rootfs manifest")
        val platform = manifest.requireString("platform", "rootfs manifest")
        val abi = manifest.requireString("abi", "rootfs manifest")
        val rootfsVersion = manifest.requireString("rootfs_version", "rootfs manifest")
        val contentSha256 = manifest.requireString("content_sha256", "rootfs manifest")
        val sbomFilename = manifest.requireString("sbom_filename", "rootfs manifest")
        val sourcePinsFilename = manifest.requireString("source_pins_filename", "rootfs manifest")
        check(runtime == "android-proot" && platform == "android") {
            "Rootfs manifest target mismatch: expected android-proot/android"
        }
        check(abi == expectedAbi) {
            "Rootfs manifest ABI mismatch: expected $expectedAbi, found $abi"
        }
        check(rootfsVersion == expectedRootfsVersion) {
            "Rootfs manifest version mismatch: expected $expectedRootfsVersion, found $rootfsVersion"
        }
        check(isLowercaseSha256(contentSha256)) {
            "rootfs manifest content_sha256 must be lowercase SHA-256"
        }
        check(sbomFilename == ROOTFS_SBOM_FILE) {
            "rootfs manifest sbom_filename must be $ROOTFS_SBOM_FILE"
        }
        check(sourcePinsFilename == ROOTFS_SOURCE_PINS_FILE) {
            "rootfs manifest source_pins_filename must be $ROOTFS_SOURCE_PINS_FILE"
        }

        val archive = manifest.requireObject("archive", "rootfs manifest")
        requireExactKeys(archive, setOf("filename", "sha256", "size_bytes"), "rootfs archive")
        val archiveName = archive.requireString("filename", "rootfs archive")
        val archiveSha = archive.requireString("sha256", "rootfs archive")
        val archiveSize = archive.requireLong("size_bytes", "rootfs archive")
        check(isSingleSafeComponent(archiveName)) {
            "Rootfs manifest archive filename is unsafe: $archiveName"
        }
        check(archiveName == expectedArchiveName) {
            "Rootfs manifest archive filename mismatch: expected $expectedArchiveName, found $archiveName"
        }
        check(isLowercaseSha256(archiveSha)) {
            "rootfs manifest archive sha256 must be lowercase SHA-256"
        }
        check(archiveSize > 0L) {
            "rootfs manifest archive size must be positive"
        }
        check(archiveSha == actualArchiveSha) {
            "Rootfs manifest archive hash mismatch"
        }
        check(archiveSize == actualArchiveSize) {
            "Rootfs manifest archive size mismatch"
        }

        val packages = manifest.requireArray("packages", "rootfs manifest").mapObjects("rootfs manifest packages") { obj ->
            requireExactKeys(
                obj,
                setOf("name", "version", "license", "architecture", "origin"),
                "rootfs manifest package",
            )
            RootfsPackageData(
                name = obj.requireString("name", "rootfs manifest package"),
                version = obj.requireString("version", "rootfs manifest package"),
                license = obj.requireString("license", "rootfs manifest package"),
                architecture = obj.requireString("architecture", "rootfs manifest package"),
                origin = obj.requireString("origin", "rootfs manifest package"),
            )
        }.also {
            check(it.isNotEmpty()) { "rootfs manifest packages must not be empty" }
            check(it.map(RootfsPackageData::name).distinct().size == it.size) {
                "rootfs manifest contains duplicate package names"
            }
        }

        val immutableFiles = manifest.requireArray("immutable_files", "rootfs manifest").mapObjects("rootfs immutable inventory") { obj ->
            requireExactKeys(
                obj,
                setOf("path", "sha256", "kind", "size_bytes"),
                "rootfs immutable inventory entry",
            )
            val path = obj.requireString("path", "rootfs immutable inventory entry")
            val sha = obj.requireString("sha256", "rootfs immutable inventory entry")
            val kind = obj.requireString("kind", "rootfs immutable inventory entry")
            val size = obj.requireLong("size_bytes", "rootfs immutable inventory entry")
            check(isValidGuestPath(path)) { "Invalid immutable inventory path: $path" }
            check(isLowercaseSha256(sha)) { "Invalid immutable inventory sha256 for $path" }
            check(kind == "regular-file" || kind == "symlink") {
                "Invalid immutable inventory kind for $path"
            }
            check(size >= 0L) { "Invalid immutable inventory size for $path" }
            RootfsImmutableEntryData(path = path, sha256 = sha, kind = kind, sizeBytes = size)
        }.also {
            check(it.isNotEmpty()) { "rootfs immutable inventory must not be empty" }
            check(it.map(RootfsImmutableEntryData::path).distinct().size == it.size) {
                "rootfs immutable inventory contains duplicate paths"
            }
        }
        val immutableByPath = immutableFiles.associateBy(RootfsImmutableEntryData::path)

        val executableAllowlist = manifest.requireArray("executable_allowlist", "rootfs manifest").mapObjects("rootfs executable allowlist") { obj ->
            val keys = jsonKeys(obj)
            check(keys == setOf("path", "sha256", "kind") || keys == setOf("path", "sha256", "kind", "size_bytes")) {
                "rootfs executable allowlist entry contains unexpected keys"
            }
            val path = obj.requireString("path", "rootfs executable allowlist entry")
            val sha = obj.requireString("sha256", "rootfs executable allowlist entry")
            val kind = obj.requireString("kind", "rootfs executable allowlist entry")
            val size = obj.longOrNull("size_bytes")
            check(isValidGuestPath(path)) { "Invalid executable allowlist path: $path" }
            check(isLowercaseSha256(sha)) { "Invalid executable allowlist sha256 for $path" }
            check(kind in setOf("elf", "shared-library", "interpreter")) {
                "Invalid executable allowlist kind for $path"
            }
            check(size == null || size >= 0L) { "Invalid executable allowlist size for $path" }
            RootfsAllowlistEntryData(path = path, sha256 = sha, kind = kind, sizeBytes = size)
        }.also {
            check(it.isNotEmpty()) { "rootfs executable allowlist must not be empty" }
            check(it.map(RootfsAllowlistEntryData::path).distinct().size == it.size) {
                "rootfs executable allowlist contains duplicate paths"
            }
            it.forEach { entry ->
                val immutable = immutableByPath[entry.path]
                check(
                    immutable != null &&
                        immutable.kind == "regular-file" &&
                        immutable.sha256 == entry.sha256 &&
                        (entry.sizeBytes == null || entry.sizeBytes == immutable.sizeBytes),
                ) {
                    "Executable allowlist entry is not bound to immutable inventory: ${entry.path}"
                }
            }
        }

        val writablePaths = manifest.requireArray("writable_paths", "rootfs manifest").mapStrings("rootfs writable paths").also {
            check(it.toSet() == ROOTFS_ALLOWED_WRITABLE_PATHS && it.size == ROOTFS_ALLOWED_WRITABLE_PATHS.size) {
                "rootfs writable paths must be exactly ${ROOTFS_ALLOWED_WRITABLE_PATHS.joinToString()}"
            }
        }

        return RootfsManifestData(
            schemaVersion = 2,
            runtime = runtime,
            platform = platform,
            abi = abi,
            rootfsVersion = rootfsVersion,
            contentSha256 = contentSha256,
            sbomFilename = sbomFilename,
            sourcePinsFilename = sourcePinsFilename,
            archive = RootfsArchiveData(
                filename = archiveName,
                sha256 = archiveSha,
                sizeBytes = archiveSize,
            ),
            packages = packages,
            executableAllowlist = executableAllowlist,
            immutableFiles = immutableFiles,
            writablePaths = writablePaths,
            rawJson = raw,
        )
    }

    private fun validateSbom(manifest: RootfsManifestData, raw: String) {
        val sbom = parseJsonObject(raw, "rootfs SBOM")
        val spdxVersion = sbom.stringOrNull("spdxVersion") ?: ""
        check(spdxVersion.startsWith("SPDX-2.")) {
            "Rootfs SBOM must be an SPDX 2.x document"
        }
        val packages = sbom.arrayOrNull("packages")
        check(packages != null && packages.values.isNotEmpty()) {
            "Rootfs SBOM must contain packages[]"
        }
        val names = mutableSetOf<String>()
        for (value in packages.values) {
            val obj = value as? JsonObject ?: continue
            obj.stringOrNull("name")?.takeIf(String::isNotBlank)?.let(names::add)
        }
        val missing = manifest.packages.map(RootfsPackageData::name).filterNot(names::contains)
        check(missing.isEmpty()) {
            "Rootfs SBOM is missing manifest packages: ${missing.joinToString()}"
        }
    }

    private fun extractTarGz(archive: File, destination: File): ExtractedRootfs {
        val root = destination.canonicalFile.toPath()
        val symlinkTargets = linkedMapOf<String, String>()
        val guestSymlinkPayloads = linkedMapOf<String, String>()
        TarArchiveInputStream(
            GzipCompressorInputStream(
                BufferedInputStream(FileInputStream(archive)),
            ),
        ).use { tar ->
            var entry: TarArchiveEntry? = tar.nextTarEntry
            while (entry != null) {
                val guestEntryPath = normalizeArchiveEntryPath(entry.name)
                val actualGuestPath = resolveActualGuestEntryPath(guestEntryPath, symlinkTargets)
                val target = guestPathToHostPath(root, actualGuestPath)
                check(target.startsWith(root)) {
                    "Rootfs archive path escapes staging directory: ${entry.name}"
                }
                when {
                    entry.isDirectory -> {
                        check(actualGuestPath !in symlinkTargets) {
                            "Rootfs directory collides with symlink path: ${entry.name}"
                        }
                        Files.createDirectories(target)
                    }
                    entry.isSymbolicLink -> {
                        val actualTargetGuestPath = normalizeGuestLinkTarget(actualGuestPath, entry.linkName)
                        val linkPayload = target.parent.relativize(
                            guestPathToHostPath(root, actualTargetGuestPath),
                        )
                        prepareFileTarget(target)
                        Files.createDirectories(target.parent)
                        Files.createSymbolicLink(target, linkPayload)
                        symlinkTargets[actualGuestPath] = actualTargetGuestPath
                        guestSymlinkPayloads[actualGuestPath] = entry.linkName
                    }
                    entry.isLink -> {
                        check(actualGuestPath !in symlinkTargets) {
                            "Rootfs hard link collides with symlink path: ${entry.name}"
                        }
                        val sourceGuestPath = resolveHardlinkSource(entry.linkName, symlinkTargets)
                        val source = guestPathToHostPath(root, sourceGuestPath)
                        requireNoSymlinkComponents(root, source)
                        requireNoSymlinkComponents(root, target.parent)
                        val attributes = Files.readAttributes(
                            source,
                            BasicFileAttributes::class.java,
                            LinkOption.NOFOLLOW_LINKS,
                        )
                        check(attributes.isRegularFile) {
                            "Unsafe rootfs hard link source: ${entry.linkName}"
                        }
                        prepareFileTarget(target)
                        Files.createDirectories(target.parent)
                        requireNoSymlinkComponents(root, source)
                        requireNoSymlinkComponents(root, target.parent)
                        // Android app-data SELinux can prohibit link(2). The manifest
                        // defines this alias as regular bytes, not shared inode identity.
                        Files.copy(source, target, LinkOption.NOFOLLOW_LINKS, StandardCopyOption.COPY_ATTRIBUTES)
                        check(Files.size(target) == attributes.size()) { "Incomplete rootfs hard-link copy" }
                    }
                    entry.isFile -> {
                        check(actualGuestPath !in symlinkTargets) {
                            "Rootfs file collides with symlink path: ${entry.name}"
                        }
                        prepareFileTarget(target)
                        Files.createDirectories(target.parent)
                        FileOutputStream(target.toFile()).use { tar.copyTo(it) }
                        target.toFile().setReadable(entry.mode and 0b100_100_100 != 0, false)
                        target.toFile().setWritable(entry.mode and 0b010_010_010 != 0, false)
                        target.toFile().setExecutable(entry.mode and 0b001_001_001 != 0, false)
                    }
                }
                entry = tar.nextTarEntry
            }
        }
        return ExtractedRootfs(guestSymlinkPayloads = guestSymlinkPayloads)
    }

    private fun collectImmutableInventory(
        root: Path,
        guestSymlinkPayloads: Map<String, String>,
    ): InventorySummary {
        val entries = mutableListOf<RootfsImmutableEntryData>()
        var totalBytes = 0L
        fun visit(directory: Path) {
            val children = Files.newDirectoryStream(directory).use { stream ->
                stream.toList().sortedBy { it.fileName.toString() }
            }
            for (child in children) {
                val guestPath = hostPathToGuestPath(root, child)
                if (isWritableInventoryPath(guestPath)) continue
                val attributes = Files.readAttributes(
                    child,
                    BasicFileAttributes::class.java,
                    LinkOption.NOFOLLOW_LINKS,
                )
                when {
                    attributes.isSymbolicLink -> {
                        val linkTarget = guestSymlinkPayloads[guestPath]
                            ?: Files.readSymbolicLink(child).toString()
                        val payload = linkTarget.toByteArray(StandardCharsets.UTF_8)
                        entries += RootfsImmutableEntryData(
                            path = guestPath,
                            sha256 = payload.sha256(),
                            kind = "symlink",
                            sizeBytes = payload.size.toLong(),
                        )
                        totalBytes += payload.size.toLong()
                    }
                    attributes.isRegularFile -> {
                        entries += RootfsImmutableEntryData(
                            path = guestPath,
                            sha256 = child.toFile().sha256File(),
                            kind = "regular-file",
                            sizeBytes = attributes.size(),
                        )
                        totalBytes += attributes.size()
                    }
                    attributes.isDirectory -> visit(child)
                    else -> error("Immutable inventory supports only regular files and symlinks: $guestPath")
                }
            }
        }
        visit(root)
        val sorted = entries.sortedBy(RootfsImmutableEntryData::path)
        return InventorySummary(entries = sorted, totalBytes = totalBytes)
    }
}

internal data class ExtractedRootfs(
    val guestSymlinkPayloads: Map<String, String>,
)

internal data class RootfsManifestData(
    val schemaVersion: Int,
    val runtime: String,
    val platform: String,
    val abi: String,
    val rootfsVersion: String,
    val contentSha256: String,
    val sbomFilename: String,
    val sourcePinsFilename: String,
    val archive: RootfsArchiveData,
    val packages: List<RootfsPackageData>,
    val executableAllowlist: List<RootfsAllowlistEntryData>,
    val immutableFiles: List<RootfsImmutableEntryData>,
    val writablePaths: List<String>,
    val rawJson: String,
)

internal data class RootfsArchiveData(
    val filename: String,
    val sha256: String,
    val sizeBytes: Long,
)

internal data class RootfsPackageData(
    val name: String,
    val version: String,
    val license: String,
    val architecture: String,
    val origin: String,
)

internal data class RootfsAllowlistEntryData(
    val path: String,
    val sha256: String,
    val kind: String,
    val sizeBytes: Long?,
)

internal data class RootfsImmutableEntryData(
    val path: String,
    val sha256: String,
    val kind: String,
    val sizeBytes: Long,
)

internal data class InventorySummary(
    val entries: List<RootfsImmutableEntryData>,
    val totalBytes: Long,
)

private sealed interface JsonValue

private data class JsonObject(val fields: Map<String, JsonValue>) : JsonValue

private data class JsonArray(val values: List<JsonValue>) : JsonValue

private data class JsonString(val value: String) : JsonValue

private data class JsonNumber(val raw: String) : JsonValue

private data class JsonBoolean(val value: Boolean) : JsonValue

private data object JsonNull : JsonValue

private fun parseJsonObject(raw: String, label: String): JsonObject {
    val value = TinyJsonParser(raw, label).parse()
    return value as? JsonObject ?: error("$label must be a JSON object")
}

private fun requireExactKeys(json: JsonObject, expected: Set<String>, label: String) {
    val actual = jsonKeys(json)
    check(actual == expected) {
        "$label keys mismatch: expected ${expected.sorted()}, found ${actual.sorted()}"
    }
}

private fun jsonKeys(json: JsonObject): Set<String> = json.fields.keys

private fun JsonObject.requireString(name: String, label: String): String {
    val value = (fields[name] as? JsonString)?.value
    check(value != null) { "$label is missing $name" }
    check(value.isNotBlank()) { "$label has blank $name" }
    return value
}

private fun JsonObject.requireLong(name: String, label: String): Long {
    val value = longOrNull(name)
    check(value != null) { "$label is missing numeric $name" }
    return value
}

private fun JsonObject.longOrNull(name: String): Long? =
    when (val value = fields[name]) {
        is JsonNumber -> value.raw.toLongOrNull()
        else -> null
    }

private fun JsonObject.requireObject(name: String, label: String): JsonObject {
    val value = fields[name] as? JsonObject
    check(value != null) { "$label is missing object $name" }
    return value
}

private fun JsonObject.requireArray(name: String, label: String): JsonArray {
    val value = fields[name] as? JsonArray
    check(value != null) { "$label is missing array $name" }
    return value
}

private fun JsonObject.stringOrNull(name: String): String? =
    (fields[name] as? JsonString)?.value

private fun JsonObject.arrayOrNull(name: String): JsonArray? =
    fields[name] as? JsonArray

private inline fun <T> JsonArray.mapObjects(
    label: String,
    transform: (JsonObject) -> T,
): List<T> = List(values.size) { index ->
    val obj = values[index] as? JsonObject
    check(obj != null) { "$label entry at index $index is not an object" }
    transform(obj)
}

private fun JsonArray.mapStrings(label: String): List<String> = List(values.size) { index ->
    val value = (values[index] as? JsonString)?.value ?: ""
    check(value.isNotBlank()) { "$label entry at index $index is blank" }
    check(isValidGuestPath(value)) { "Invalid writable guest path: $value" }
    value
}

private class TinyJsonParser(
    private val source: String,
    private val label: String,
) {
    private var index: Int = 0

    fun parse(): JsonValue {
        skipWhitespace()
        val value = parseValue()
        skipWhitespace()
        check(index == source.length) { "$label contains trailing JSON content" }
        return value
    }

    private fun parseValue(): JsonValue =
        when (peek()) {
            '{' -> parseObject()
            '[' -> parseArray()
            '"' -> JsonString(parseStringLiteral())
            't' -> parseLiteral("true", JsonBoolean(true))
            'f' -> parseLiteral("false", JsonBoolean(false))
            'n' -> parseLiteral("null", JsonNull)
            '-', in '0'..'9' -> JsonNumber(parseNumberLiteral())
            else -> error("$label contains invalid JSON at character $index")
        }

    private fun parseObject(): JsonObject {
        expect('{')
        skipWhitespace()
        val fields = linkedMapOf<String, JsonValue>()
        if (consume('}')) return JsonObject(fields)
        while (true) {
            skipWhitespace()
            val key = parseStringLiteral()
            skipWhitespace()
            expect(':')
            skipWhitespace()
            check(fields.put(key, parseValue()) == null) {
                "$label contains duplicate object key $key"
            }
            skipWhitespace()
            if (consume('}')) return JsonObject(fields)
            expect(',')
            skipWhitespace()
        }
    }

    private fun parseArray(): JsonArray {
        expect('[')
        skipWhitespace()
        val values = mutableListOf<JsonValue>()
        if (consume(']')) return JsonArray(values)
        while (true) {
            values += parseValue()
            skipWhitespace()
            if (consume(']')) return JsonArray(values)
            expect(',')
            skipWhitespace()
        }
    }

    private fun parseStringLiteral(): String {
        expect('"')
        val out = StringBuilder()
        while (true) {
            check(index < source.length) { "$label has an unterminated string literal" }
            val ch = source[index++]
            when (ch) {
                '"' -> return out.toString()
                '\\' -> {
                    check(index < source.length) { "$label has an invalid escape sequence" }
                    when (val escaped = source[index++]) {
                        '"', '\\', '/' -> out.append(escaped)
                        'b' -> out.append('\b')
                        'f' -> out.append('\u000C')
                        'n' -> out.append('\n')
                        'r' -> out.append('\r')
                        't' -> out.append('\t')
                        'u' -> {
                            val hex = source.substring(index, index + 4)
                            out.append(hex.toInt(16).toChar())
                            index += 4
                        }
                        else -> error("$label has an invalid JSON escape sequence")
                    }
                }
                else -> out.append(ch)
            }
        }
    }

    private fun parseNumberLiteral(): String {
        val start = index
        if (peek() == '-') index += 1
        consumeDigits()
        if (peekOrNull() == '.') {
            index += 1
            consumeDigits()
        }
        if (peekOrNull() == 'e' || peekOrNull() == 'E') {
            index += 1
            if (peekOrNull() == '+' || peekOrNull() == '-') index += 1
            consumeDigits()
        }
        return source.substring(start, index)
    }

    private fun consumeDigits() {
        val start = index
        while (peekOrNull() in '0'..'9') index += 1
        check(index > start) { "$label has an invalid JSON number" }
    }

    private fun <T : JsonValue> parseLiteral(expected: String, value: T): T {
        check(source.regionMatches(index, expected, 0, expected.length)) {
            "$label contains invalid JSON at character $index"
        }
        index += expected.length
        return value
    }

    private fun consume(expected: Char): Boolean =
        if (peekOrNull() == expected) {
            index += 1
            true
        } else {
            false
        }

    private fun expect(expected: Char) {
        check(consume(expected)) { "$label expected '$expected' at character $index" }
    }

    private fun skipWhitespace() {
        while (peekOrNull()?.isWhitespace() == true) index += 1
    }

    private fun peek(): Char = peekOrNull() ?: error("$label ended unexpectedly")

    private fun peekOrNull(): Char? = source.getOrNull(index)
}

private fun isLowercaseSha256(value: String): Boolean =
    value.length == 64 && value.all { it in '0'..'9' || it in 'a'..'f' }

private fun isSingleSafeComponent(value: String): Boolean =
    value.isNotBlank() &&
        value != "." &&
        value != ".." &&
        '/' !in value &&
        '\u0000' !in value

private fun isValidGuestPath(path: String): Boolean =
    path.startsWith("/") &&
        path != "/" &&
        splitGuestPath(path).all { it.isNotBlank() && it != "." && it != ".." }

private fun splitGuestPath(path: String): List<String> =
    path.removePrefix("/").split('/').filter(String::isNotEmpty)

private fun normalizeArchiveEntryPath(name: String): String {
    check(name.isNotBlank() && !name.contains('\u0000')) {
        "Rootfs archive entry name is invalid: $name"
    }
    val trimmed = name.trimEnd('/')
    check(trimmed.isNotBlank() && !trimmed.startsWith('/')) {
        "Rootfs archive entry escapes staging directory: $name"
    }
    val rawParts = trimmed.split('/')
    val parts = if (rawParts.firstOrNull() == ".") rawParts.drop(1) else rawParts
    check(parts.isNotEmpty() && parts.all { it.isNotBlank() && it != "." && it != ".." }) {
        "Rootfs archive entry contains unsafe path components: $name"
    }
    return "/${parts.joinToString("/")}"
}

private fun normalizeAbsoluteGuestPath(path: String, label: String): String {
    check(path.startsWith("/") && !path.contains('\u0000')) { "$label must stay inside the guest root" }
    val trimmed = path.trimEnd('/')
    val parts = splitGuestPath(trimmed.ifBlank { "/" })
    check(parts.isNotEmpty() && parts.all { it.isNotBlank() && it != "." && it != ".." }) {
        "$label contains unsafe path components"
    }
    return "/${parts.joinToString("/")}"
}

private fun normalizeRelativeGuestPath(baseGuestDir: String, path: String, label: String): String {
    check(path.isNotBlank() && !path.startsWith("/") && !path.contains('\u0000')) {
        "$label must be a guest-relative path"
    }
    val resolved = splitGuestPath(baseGuestDir).toMutableList()
    for (part in path.split('/')) {
        when (part) {
            "", "." -> {}
            ".." -> {
                check(resolved.isNotEmpty()) { "$label escapes the guest root" }
                resolved.removeAt(resolved.lastIndex)
            }
            else -> resolved += part
        }
    }
    check(resolved.isNotEmpty()) { "$label resolves to the guest root" }
    return "/${resolved.joinToString("/")}"
}

private fun normalizeGuestLinkTarget(entryGuestPath: String, linkName: String): String {
    val parent = parentGuestPath(entryGuestPath)
    return if (linkName.startsWith("/")) {
        normalizeAbsoluteGuestPath(linkName, "Rootfs symlink target")
    } else {
        normalizeRelativeGuestPath(parent, linkName, "Rootfs symlink target")
    }
}

private fun resolveHardlinkSource(linkName: String, symlinkTargets: Map<String, String>): String {
    val sourceGuestPath = normalizeArchiveEntryPath(linkName)
    val actualSourceGuestPath = resolveGuestPath(sourceGuestPath, symlinkTargets, followFinalSymlink = false)
    check(actualSourceGuestPath !in symlinkTargets) {
        "Unsafe rootfs hard link source: $linkName"
    }
    return actualSourceGuestPath
}

private fun resolveActualGuestEntryPath(entryGuestPath: String, symlinkTargets: Map<String, String>): String {
    val parent = resolveGuestPath(parentGuestPath(entryGuestPath), symlinkTargets, followFinalSymlink = true)
    return childGuestPath(parent, entryGuestPath.substringAfterLast('/'))
}

private fun resolveGuestPath(
    guestPath: String,
    symlinkTargets: Map<String, String>,
    followFinalSymlink: Boolean,
): String {
    if (guestPath == "/") return "/"
    val pending = ArrayDeque(splitGuestPath(guestPath))
    val resolved = mutableListOf<String>()
    var expansions = 0
    while (pending.isNotEmpty()) {
        resolved += pending.removeFirst()
        while (true) {
            val current = "/${resolved.joinToString("/")}"
            val shouldFollow = pending.isNotEmpty() || followFinalSymlink
            val target = if (shouldFollow) symlinkTargets[current] else null
            if (target == null) break
            expansions += 1
            check(expansions <= MAX_ROOTFS_SYMLINK_EXPANSIONS) {
                "Unsafe rootfs symlink chain while resolving $guestPath"
            }
            resolved.clear()
            resolved += splitGuestPath(target)
        }
    }
    return "/${resolved.joinToString("/")}"
}

private fun parentGuestPath(path: String): String =
    path.substringBeforeLast('/', missingDelimiterValue = "").let {
        if (it.isBlank()) "/" else it
    }

private fun childGuestPath(parent: String, child: String): String =
    if (parent == "/") "/$child" else "$parent/$child"

private fun guestPathToHostPath(root: Path, guestPath: String): Path =
    root.resolve(guestPath.removePrefix("/")).normalize()

private fun hostPathToGuestPath(root: Path, path: Path): String {
    val relative = root.relativize(path).joinToString("/") { it.toString() }
    return "/$relative"
}

private fun isWritableInventoryPath(path: String): Boolean =
    ROOTFS_ALLOWED_WRITABLE_PATHS.any { path == it || path.startsWith("$it/") }

private fun prepareFileTarget(target: Path) {
    check(!Files.exists(target, LinkOption.NOFOLLOW_LINKS)) {
        "Refusing to overwrite rootfs archive entry at ${target.fileName}"
    }
}

private fun canonicalInventorySha256(entries: List<RootfsImmutableEntryData>): String =
    canonicalInventoryJson(entries).toByteArray(StandardCharsets.UTF_8).sha256()

private fun canonicalInventoryJson(entries: List<RootfsImmutableEntryData>): String =
    entries.joinToString(prefix = "[", postfix = "]", separator = ",") { entry ->
        """{"kind":${jsonQuote(entry.kind)},"path":${jsonQuote(entry.path)},"sha256":${jsonQuote(entry.sha256)},"size_bytes":${entry.sizeBytes}}"""
    }

private fun jsonQuote(value: String): String {
    val out = StringBuilder("\"")
    for (ch in value) {
        when (ch) {
            '\\' -> out.append("\\\\")
            '"' -> out.append("\\\"")
            '\b' -> out.append("\\b")
            '\u000C' -> out.append("\\f")
            '\n' -> out.append("\\n")
            '\r' -> out.append("\\r")
            '\t' -> out.append("\\t")
            else ->
                if (ch.code < 0x20) {
                    out.append("\\u").append(ch.code.toString(16).padStart(4, '0'))
                } else {
                    out.append(ch)
                }
        }
    }
    out.append('"')
    return out.toString()
}

private fun atomicWriteText(target: File, text: String) {
    val parent = target.parentFile ?: error("Atomic target must have a parent directory")
    check(parent.mkdirs() || parent.isDirectory) { "Unable to prepare manifest directory ${parent.path}" }
    val temp = File(parent, ".${target.name}.${UUID.randomUUID()}.tmp")
    temp.writeText(text, StandardCharsets.UTF_8)
    try {
        Files.move(
            temp.toPath(),
            target.toPath(),
            StandardCopyOption.REPLACE_EXISTING,
            StandardCopyOption.ATOMIC_MOVE,
        )
    } catch (_: Exception) {
        Files.move(temp.toPath(), target.toPath(), StandardCopyOption.REPLACE_EXISTING)
    } finally {
        temp.delete()
    }
}

private fun ByteArray.sha256(): String {
    val digest = MessageDigest.getInstance("SHA-256")
    digest.update(this)
    return digest.digest().joinToString("") { "%02x".format(it) }
}

private fun File.sha256File(): String {
    val digest = MessageDigest.getInstance("SHA-256")
    inputStream().buffered().use { input ->
        val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
        while (true) {
            val count = input.read(buffer)
            if (count < 0) break
            digest.update(buffer, 0, count)
        }
    }
    return digest.digest().joinToString("") { "%02x".format(it) }
}

private fun requireSafeComponent(value: String, label: String) {
    require(value.isNotBlank() && value != "." && value != ".." &&
        !value.contains('/') && !value.contains('\\') && !value.contains('\u0000') &&
        !File(value).isAbsolute) { "$label must be a single safe path component" }
}

private fun requireNoSymlinkComponents(root: Path, path: Path) {
    check(path.startsWith(root)) { "Rootfs hard-link path escapes staging root" }
    var current = root
    for (component in root.relativize(path)) {
        current = current.resolve(component)
        check(!Files.isSymbolicLink(current)) { "Rootfs hard-link path traverses a symbolic link" }
    }
}
