package io.lingxi.mobilelinux

import org.apache.commons.compress.archivers.tar.TarArchiveEntry
import org.apache.commons.compress.archivers.tar.TarArchiveOutputStream
import org.apache.commons.compress.compressors.gzip.GzipCompressorOutputStream
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.IOException
import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.security.MessageDigest

class LinuxRuntimeBridgeRootfsInstallerTest {
    @Test
    fun stage_rejectsArchiveTraversalBeforeCallingWriter() {
        val managedRoot = Files.createTempDirectory("rootfs-traversal").toFile()
        try {
            for (name in listOf("../escape", "/tmp/escape", "..\\escape", ".", "")) {
                var writerCalled = false
                assertThrows(IllegalArgumentException::class.java) {
                    RootfsInstaller.stage(managedRoot, "version", "sha", "arm64", name, "{}", "{}", { writerCalled = true })
                }
                assertFalse(writerCalled)
            }
        } finally { managedRoot.deleteRecursively() }
    }

    @Test
    fun stage_rejectsUnsafeVersionWithoutMutatingExistingData() {
        val parent = Files.createTempDirectory("rootfs-version").toFile()
        try {
            val managed = File(parent, "managed")
            val marker = File(managed, "staged/keep/marker")
            marker.parentFile.mkdirs()
            marker.writeText("keep")
            for (version in listOf("", ".", "..", "nested/..", "a/b", "a\\b", "a\u0000b", "/tmp/outside")) {
                var copied = false
                assertThrows(IllegalArgumentException::class.java) {
                    RootfsInstaller.stage(managed, version, "sha", "arm64", "archive.tar.gz", "{}", "{}", { copied = true })
                }
                assertFalse(copied)
                assertEquals("keep", marker.readText())
                assertEquals(listOf("staged"), managed.list()!!.toList())
                assertEquals(listOf("keep"), File(managed,"staged").list()!!.toList())
            }
        } finally { parent.deleteRecursively() }
    }

    @Test
    fun stage_materializesHardlinkAsIndependentVerifiedExecutableBytes() {
        val managed = Files.createTempDirectory("rootfs-hardlink").toFile()
        try {
            val bytes = "#!/bin/sh\necho preserved\n".toByteArray()
            val archiveName = "rootfs.tar.gz"
            val archive = tarGz(TarSpec.file("bin/busybox",bytes,493),TarSpec.hardlink("bin/sh","bin/busybox"))
            val manifest = manifestJson(archiveName,archive,listOf(immutableFile("/bin/busybox",bytes),immutableFile("/bin/sh",bytes)),listOf(RootfsAllowlistEntryData("/bin/busybox",sha256(bytes),"interpreter",bytes.size.toLong())))
            val result = RootfsInstaller.stage(managed,"3.21.3",sha256(archive),"arm64",archiveName,manifest,sbomJson("busybox"),{it.writeBytes(archive)})
            val source=File(result.stagedRoot,"bin/busybox").toPath()
            val alias=File(result.stagedRoot,"bin/sh").toPath()
            assertEquals(sha256(bytes),sha256(Files.readAllBytes(alias)))
            assertEquals(bytes.size.toLong(),Files.size(alias))
            assertEquals(Files.getPosixFilePermissions(source),Files.getPosixFilePermissions(alias))
            assertTrue(Files.isExecutable(alias))
            assertFalse(Files.isSameFile(source,alias))
        } finally { managed.deleteRecursively() }
    }

    private companion object {
        const val manifestFileName = "rootfs-manifest.json"
        const val sbomFileName = "rootfs.spdx.json"
        const val sourcePinsFileName = "mobile-linux-pins.json"
    }

    @Test
    fun stage_mapsAbsoluteGuestSymlinksInsideExtractionRoot_andPersistsManifest() {
        val managedRoot = Files.createTempDirectory("rootfs-stage-success").toFile()
        try {
            val archiveName = "alpine-minirootfs-3.21.3-aarch64.tar.gz"
            val busybox = "#!/bin/sh\necho busybox\n".toByteArray(StandardCharsets.UTF_8)
            val archiveBytes = tarGz(
                TarSpec.file("bin/busybox", busybox, 493),
                TarSpec.symlink("bin/sh", "/bin/busybox"),
            )
            val immutable = listOf(
                immutableFile("/bin/busybox", busybox),
                immutableSymlink("/bin/sh", "/bin/busybox"),
            )
            val manifestJson = manifestJson(
                archiveName = archiveName,
                archiveBytes = archiveBytes,
                immutableFiles = immutable,
                executableAllowlist = listOf(
                    RootfsAllowlistEntryData(
                        path = "/bin/busybox",
                        sha256 = sha256(busybox),
                        kind = "interpreter",
                        sizeBytes = busybox.size.toLong(),
                    ),
                ),
            )

            val result = RootfsInstaller.stage(
                managedRoot = managedRoot,
                expectedRootfsVersion = "3.21.3",
                expectedArchiveSha = sha256(archiveBytes),
                expectedManifestAbi = "arm64",
                archiveName = archiveName,
                manifestJson = manifestJson,
                sbomJson = sbomJson("busybox"),
                copyArchive = { target -> target.writeBytes(archiveBytes) },
            )

            val stagedRoot = result.stagedRoot.toPath()
            assertTrue(Files.isRegularFile(stagedRoot.resolve("bin/busybox")))
            assertEquals(
                "busybox",
                Files.readSymbolicLink(stagedRoot.resolve("bin/sh")).toString(),
            )
            assertTrue(Files.isExecutable(stagedRoot.resolve("bin/sh")))
            assertTrue(File(managedRoot, manifestFileName).isFile)
            assertEquals(2, result.verifiedFiles)
        } finally {
            managedRoot.deleteRecursively()
        }
    }

    @Test
    fun stage_rejectsAbsoluteGuestSymlinkEscapes() {
        val managedRoot = Files.createTempDirectory("rootfs-stage-escape").toFile()
        try {
            val archiveName = "alpine-minirootfs-3.21.3-aarch64.tar.gz"
            val busybox = "busybox".toByteArray(StandardCharsets.UTF_8)
            val archiveBytes = tarGz(
                TarSpec.file("bin/busybox", busybox, 493),
                TarSpec.symlink("bin/sh", "/../../etc/passwd"),
            )
            val immutable = listOf(
                immutableFile("/bin/busybox", busybox),
                immutableSymlink("/bin/sh", "/bin/busybox"),
            )

            assertThrows(IllegalStateException::class.java) {
                RootfsInstaller.stage(
                    managedRoot = managedRoot,
                    expectedRootfsVersion = "3.21.3",
                    expectedArchiveSha = sha256(archiveBytes),
                    expectedManifestAbi = "arm64",
                    archiveName = archiveName,
                    manifestJson = manifestJson(
                        archiveName = archiveName,
                        archiveBytes = archiveBytes,
                        immutableFiles = immutable,
                        executableAllowlist = listOf(
                            RootfsAllowlistEntryData(
                                path = "/bin/busybox",
                                sha256 = sha256(busybox),
                                kind = "interpreter",
                                sizeBytes = busybox.size.toLong(),
                            ),
                        ),
                    ),
                    sbomJson = sbomJson("busybox"),
                    copyArchive = { target -> target.writeBytes(archiveBytes) },
                )
            }

            assertFalse(File(managedRoot, "staged/3.21.3").exists())
            assertFalse(File(managedRoot, manifestFileName).exists())
        } finally {
            managedRoot.deleteRecursively()
        }
    }

    @Test
    fun stage_rejectsManifestArchiveMismatch_withoutPublishingState() {
        val managedRoot = Files.createTempDirectory("rootfs-stage-archive-mismatch").toFile()
        try {
            val archiveName = "alpine-minirootfs-3.21.3-aarch64.tar.gz"
            val busybox = "busybox".toByteArray(StandardCharsets.UTF_8)
            val archiveBytes = tarGz(
                TarSpec.file("bin/busybox", busybox, 493),
                TarSpec.symlink("bin/sh", "/bin/busybox"),
            )
            val immutable = listOf(
                immutableFile("/bin/busybox", busybox),
                immutableSymlink("/bin/sh", "/bin/busybox"),
            )
            val manifestJson = manifestJson(
                archiveName = archiveName,
                archiveBytes = archiveBytes,
                immutableFiles = immutable,
                executableAllowlist = listOf(
                    RootfsAllowlistEntryData(
                        path = "/bin/busybox",
                        sha256 = sha256(busybox),
                        kind = "interpreter",
                        sizeBytes = busybox.size.toLong(),
                    ),
                ),
                archiveSha256 = "0".repeat(64),
            )

            assertThrows(IllegalStateException::class.java) {
                RootfsInstaller.stage(
                    managedRoot = managedRoot,
                    expectedRootfsVersion = "3.21.3",
                    expectedArchiveSha = sha256(archiveBytes),
                    expectedManifestAbi = "arm64",
                    archiveName = archiveName,
                    manifestJson = manifestJson,
                    sbomJson = sbomJson("busybox"),
                    copyArchive = { target -> target.writeBytes(archiveBytes) },
                )
            }

            assertFalse(File(managedRoot, "staged/3.21.3").exists())
            assertFalse(File(managedRoot, manifestFileName).exists())
        } finally {
            managedRoot.deleteRecursively()
        }
    }

    @Test
    fun stage_rejectsContentMismatch_withoutPublishingState() {
        val managedRoot = Files.createTempDirectory("rootfs-stage-content-mismatch").toFile()
        try {
            val archiveName = "alpine-minirootfs-3.21.3-aarch64.tar.gz"
            val busybox = "busybox".toByteArray(StandardCharsets.UTF_8)
            val archiveBytes = tarGz(
                TarSpec.file("bin/busybox", busybox, 493),
                TarSpec.symlink("bin/sh", "/bin/busybox"),
            )
            val immutable = listOf(
                immutableFile("/bin/busybox", busybox),
                immutableSymlink("/bin/sh", "/bin/NOT-BUSYBOX"),
            )
            val manifestJson = manifestJson(
                archiveName = archiveName,
                archiveBytes = archiveBytes,
                immutableFiles = immutable,
                executableAllowlist = listOf(
                    RootfsAllowlistEntryData(
                        path = "/bin/busybox",
                        sha256 = sha256(busybox),
                        kind = "interpreter",
                        sizeBytes = busybox.size.toLong(),
                    ),
                ),
            )

            assertThrows(IllegalStateException::class.java) {
                RootfsInstaller.stage(
                    managedRoot = managedRoot,
                    expectedRootfsVersion = "3.21.3",
                    expectedArchiveSha = sha256(archiveBytes),
                    expectedManifestAbi = "arm64",
                    archiveName = archiveName,
                    manifestJson = manifestJson,
                    sbomJson = sbomJson("busybox"),
                    copyArchive = { target -> target.writeBytes(archiveBytes) },
                )
            }

            assertFalse(File(managedRoot, "staged/3.21.3").exists())
            assertFalse(File(managedRoot, manifestFileName).exists())
        } finally {
            managedRoot.deleteRecursively()
        }
    }

    @Test
    fun stage_cleansPublishedRootWhenManifestPersistenceFails() {
        val managedRoot = Files.createTempDirectory("rootfs-stage-persist-failure").toFile()
        try {
            val archiveName = "alpine-minirootfs-3.21.3-aarch64.tar.gz"
            val busybox = "busybox".toByteArray(StandardCharsets.UTF_8)
            val archiveBytes = tarGz(
                TarSpec.file("bin/busybox", busybox, 493),
                TarSpec.symlink("bin/sh", "/bin/busybox"),
            )
            val immutable = listOf(
                immutableFile("/bin/busybox", busybox),
                immutableSymlink("/bin/sh", "/bin/busybox"),
            )

            assertThrows(IOException::class.java) {
                RootfsInstaller.stage(
                    managedRoot = managedRoot,
                    expectedRootfsVersion = "3.21.3",
                    expectedArchiveSha = sha256(archiveBytes),
                    expectedManifestAbi = "arm64",
                    archiveName = archiveName,
                    manifestJson = manifestJson(
                        archiveName = archiveName,
                        archiveBytes = archiveBytes,
                        immutableFiles = immutable,
                        executableAllowlist = listOf(
                            RootfsAllowlistEntryData(
                                path = "/bin/busybox",
                                sha256 = sha256(busybox),
                                kind = "interpreter",
                                sizeBytes = busybox.size.toLong(),
                            ),
                        ),
                    ),
                    sbomJson = sbomJson("busybox"),
                    copyArchive = { target -> target.writeBytes(archiveBytes) },
                    persistManifest = { throw IOException("disk full") },
                )
            }

            assertFalse(File(managedRoot, "staged/3.21.3").exists())
            assertFalse(File(managedRoot, manifestFileName).exists())
        } finally {
            managedRoot.deleteRecursively()
        }
    }

    private fun manifestJson(
        archiveName: String,
        archiveBytes: ByteArray,
        immutableFiles: List<RootfsImmutableEntryData>,
        executableAllowlist: List<RootfsAllowlistEntryData>,
        archiveSha256: String = sha256(archiveBytes),
    ): String {
        val packagesJson = """
            [{"name":"busybox","version":"1.0","license":"GPL-2.0-only","architecture":"aarch64","origin":"busybox"}]
        """.trimIndent()
        val allowlistJson = executableAllowlist.joinToString(prefix = "[", postfix = "]", separator = ",") { entry ->
            buildString {
                append("{")
                append(""""path":${jsonQuote(entry.path)},""")
                append(""""sha256":${jsonQuote(entry.sha256)},""")
                append(""""kind":${jsonQuote(entry.kind)}""")
                entry.sizeBytes?.let { append(""","size_bytes":$it""") }
                append("}")
            }
        }
        val immutableJson = immutableFiles.joinToString(prefix = "[", postfix = "]", separator = ",") { entry ->
            """{"path":${jsonQuote(entry.path)},"sha256":${jsonQuote(entry.sha256)},"kind":${jsonQuote(entry.kind)},"size_bytes":${entry.sizeBytes}}"""
        }
        return """
            {
              "schema_version": 2,
              "runtime": "android-proot",
              "platform": "android",
              "abi": "arm64",
              "rootfs_version": "3.21.3",
              "content_sha256": "${canonicalInventorySha256(immutableFiles)}",
              "sbom_filename": "$sbomFileName",
              "source_pins_filename": "$sourcePinsFileName",
              "archive": {
                "filename": "${jsonEscape(archiveName)}",
                "sha256": "$archiveSha256",
                "size_bytes": ${archiveBytes.size.toLong()}
              },
              "packages": $packagesJson,
              "executable_allowlist": $allowlistJson,
              "immutable_files": $immutableJson,
              "writable_paths": ["/root", "/tmp", "/var/tmp", "/workspace"]
            }
        """.trimIndent()
    }

    private fun sbomJson(vararg packageNames: String): String =
        """
            {
              "spdxVersion": "SPDX-2.3",
              "packages": [
                ${packageNames.joinToString(separator = ",") { """{"name":${jsonQuote(it)}}""" }}
              ]
            }
        """.trimIndent()

    private fun immutableFile(path: String, bytes: ByteArray): RootfsImmutableEntryData =
        RootfsImmutableEntryData(
            path = path,
            sha256 = sha256(bytes),
            kind = "regular-file",
            sizeBytes = bytes.size.toLong(),
        )

    private fun immutableSymlink(path: String, linkTarget: String): RootfsImmutableEntryData =
        RootfsImmutableEntryData(
            path = path,
            sha256 = sha256(linkTarget.toByteArray(StandardCharsets.UTF_8)),
            kind = "symlink",
            sizeBytes = linkTarget.toByteArray(StandardCharsets.UTF_8).size.toLong(),
        )

    private fun canonicalInventorySha256(entries: List<RootfsImmutableEntryData>): String {
        val canonical = entries.joinToString(prefix = "[", postfix = "]", separator = ",") { entry ->
            """{"kind":${jsonQuote(entry.kind)},"path":${jsonQuote(entry.path)},"sha256":${jsonQuote(entry.sha256)},"size_bytes":${entry.sizeBytes}}"""
        }
        return sha256(canonical.toByteArray(StandardCharsets.UTF_8))
    }

    private fun jsonEscape(value: String): String =
        buildString {
            for (ch in value) {
                when (ch) {
                    '\\' -> append("\\\\")
                    '"' -> append("\\\"")
                    '\b' -> append("\\b")
                    '\u000C' -> append("\\f")
                    '\n' -> append("\\n")
                    '\r' -> append("\\r")
                    '\t' -> append("\\t")
                    else ->
                        if (ch.code < 0x20) {
                            append("\\u").append(ch.code.toString(16).padStart(4, '0'))
                        } else {
                            append(ch)
                        }
                }
            }
        }

    private fun jsonQuote(value: String): String = "\"${jsonEscape(value)}\""

    private fun sha256(bytes: ByteArray): String {
        val digest = MessageDigest.getInstance("SHA-256")
        digest.update(bytes)
        return digest.digest().joinToString("") { "%02x".format(it) }
    }

    private fun tarGz(vararg specs: TarSpec): ByteArray {
        val buffer = ByteArrayOutputStream()
        GzipCompressorOutputStream(buffer).use { gz ->
            TarArchiveOutputStream(gz).use { tar ->
                tar.setLongFileMode(TarArchiveOutputStream.LONGFILE_POSIX)
                for (spec in specs) {
                    when {
                        spec.directory -> {
                            val entry = TarArchiveEntry(
                                if (spec.path.endsWith("/")) spec.path else "${spec.path}/",
                            )
                            entry.mode = spec.mode
                            tar.putArchiveEntry(entry)
                            tar.closeArchiveEntry()
                        }
                        spec.hardlinkTarget != null -> {
                            val entry = TarArchiveEntry(spec.path,TarArchiveEntry.LF_LINK)
                            entry.setLinkName(spec.hardlinkTarget)
                            tar.putArchiveEntry(entry)
                            tar.closeArchiveEntry()
                        }
                        spec.symlinkTarget != null -> {
                            val entry = TarArchiveEntry(spec.path, TarArchiveEntry.LF_SYMLINK)
                            entry.mode = spec.mode
                            entry.setLinkName(spec.symlinkTarget)
                            tar.putArchiveEntry(entry)
                            tar.closeArchiveEntry()
                        }
                        else -> {
                            val bytes = checkNotNull(spec.bytes)
                            val entry = TarArchiveEntry(spec.path)
                            entry.mode = spec.mode
                            entry.size = bytes.size.toLong()
                            tar.putArchiveEntry(entry)
                            tar.write(bytes)
                            tar.closeArchiveEntry()
                        }
                    }
                }
            }
        }
        return buffer.toByteArray()
    }

    private data class TarSpec(
        val path: String,
        val bytes: ByteArray? = null,
        val mode: Int = 420,
        val symlinkTarget: String? = null,
        val hardlinkTarget: String? = null,
        val directory: Boolean = false,
    ) {
        companion object {
            fun file(path: String, bytes: ByteArray, mode: Int) =
                TarSpec(path = path, bytes = bytes, mode = mode)

            fun hardlink(path: String, linkTarget: String) =
                TarSpec(path = path, hardlinkTarget = linkTarget)

            fun symlink(path: String, linkTarget: String) =
                TarSpec(path = path, mode = 511, symlinkTarget = linkTarget)
        }
    }
}
