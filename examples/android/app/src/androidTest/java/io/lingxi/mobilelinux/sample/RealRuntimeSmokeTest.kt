package io.lingxi.mobilelinux.sample

import android.os.Build
import android.util.Log
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import io.lingxi.mobilelinux.MobileLinuxRuntime
import io.lingxi.mobilelinux.RootfsInstaller
import io.lingxi.mobilelinux.bindings.*
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.ByteArrayOutputStream
import java.io.File
import java.net.InetAddress
import java.net.ServerSocket
import java.security.MessageDigest

/** Device acceptance against caller-staged real Alpine bytes and Maven-only SDK artifacts. */
@RunWith(AndroidJUnit4::class)
class RealRuntimeSmokeTest {
    private val context get() = InstrumentationRegistry.getInstrumentation().targetContext
    private val checks = mutableListOf<String>()
    private fun passed(name: String) { checks += name; Log.i("MobileLinuxSdkSmoke", "PASS $name") }
    private fun sha(file: File): String = MessageDigest.getInstance("SHA-256").let { digest ->
        file.inputStream().buffered().use { input ->
            val buffer = ByteArray(1024 * 1024)
            while (true) { val count = input.read(buffer); if (count < 0) break; digest.update(buffer, 0, count) }
        }
        digest.digest().joinToString("") { "%02x".format(it) }
    }
    private fun command(script: String, network: NetworkPolicyFfi = NetworkPolicyFfi.DISABLED,
                        python: Boolean = false, raw: Boolean = false) = MobileLinuxCommandRequestFfi(
        command = if (python) "/usr/bin/python3" else "/bin/sh",
        args = if (python) listOf("-u", "-c", script) else listOf("-c", script),
        cwd = "/root", env = emptyList(), stdin = null,
        timeoutMs = if (raw) null else 15_000uL, network = network,
        resourceLimits = ResourceLimitsFfi(null, null, null, null), mounts = emptyList(),
    )
    private suspend fun eventsUntil(handle: RuntimeHandle, predicate: (MobileLinuxEventFfi) -> Boolean): MobileLinuxEventFfi =
        withTimeout(15_000) {
            var after: ULong? = null
            var found: MobileLinuxEventFfi? = null
            while (found == null) {
                for (event in handle.readEvents(after, 512u)) {
                    after = event.sequence
                    if (predicate(event)) { found = event; break }
                }
                if (found == null) delay(20)
            }
            found
        }

    @Test(timeout = 900_000)
    fun verifiedRootfsRunsNetworkRawPtyCancelAndRestart(): Unit {
      runBlocking {
        val inputs = File(context.filesDir, "sdk-smoke-input")
        val testAssets = InstrumentationRegistry.getInstrumentation().context.assets
        if (testAssets.list("sdk-smoke-input")?.contains("rootfs-manifest.json") == true) {
            inputs.deleteRecursively()
            check(inputs.mkdirs())
            for ((assetName, name) in listOf(
                "rootfs-archive.bin" to "rootfs.tar.gz",
                "rootfs-manifest.json" to "rootfs-manifest.json",
                "rootfs.spdx.json" to "rootfs.spdx.json",
            )) {
                testAssets.open("sdk-smoke-input/$assetName").use { source ->
                    File(inputs, name).outputStream().use { destination -> source.copyTo(destination) }
                }
            }
        }
        val manifestFile = File(inputs, "rootfs-manifest.json")
        check(manifestFile.isFile) { "Stage real verified rootfs inputs into files/sdk-smoke-input before instrumentation" }
        val manifestText = manifestFile.readText()
        val manifest = JSONObject(manifestText)
        val archiveRecord = manifest.getJSONObject("archive")
        val archive = File(inputs, "rootfs.tar.gz")
        val archiveSha = sha(archive)
        assertEquals(archiveRecord.getString("sha256"), archiveSha)
        assertEquals("android", manifest.getString("platform"))
        val deviceAbi = Build.SUPPORTED_ABIS.first()
        val manifestAbi = when (deviceAbi) {
            "arm64-v8a" -> "arm64"
            "x86_64" -> "x86_64"
            else -> error("Unsupported device ABI: $deviceAbi")
        }
        assertEquals(manifestAbi, manifest.getString("abi"))
        val version = manifest.getString("rootfs_version")
        val managed = File(context.filesDir, "sdk-real-runtime")
        check(!managed.exists() || managed.deleteRecursively())
        val result = RootfsInstaller.stage(
            managedRoot = managed, expectedRootfsVersion = version,
            expectedArchiveSha = archiveSha, expectedManifestAbi = manifest.getString("abi"),
            archiveName = archiveRecord.getString("filename"), manifestJson = manifestText,
            sbomJson = File(inputs, "rootfs.spdx.json").readText(),
            copyArchive = { destination -> archive.copyTo(destination, overwrite = true) },
        )
        assertTrue(result.verifiedFiles > 10)
        passed("verified-real-rootfs-staging")
        val config = RuntimeConfig(
            platform = RuntimePlatform.ANDROID, managedRoot = managed.absolutePath,
            appSandboxRoot = context.filesDir.absolutePath, abi = deviceAbi,
            rootfsVersion = version, archiveSha256 = archiveSha,
            nativeLibraryDir = context.applicationInfo.nativeLibraryDir,
            workspaceHostPath = null, stableWorkspaceId = null, authorizationFile = null,
            rootfsArchivePath = null, defaultMountPath = null,
            protectedHostRoots = emptyList(), allowedMountRoots = emptyList(), allowedGuestRoots = emptyList(),
        )
        var runtime = MobileLinuxRuntime.create(config)
        var primaryFailure: Throwable? = null
        try {
            assertEquals(MobileLinuxRootfsStateFfi.READY, runtime.handle.repairRootfs().state)
            assertEquals(MobileLinuxRootfsStateFfi.READY, runtime.boot().state)
            val hello = runtime.execute(command("printf SDK_REAL_GUEST; uname -m"))
            assertEquals(hello.stderr, 0, hello.exitCode)
            assertTrue(hello.stdout.contains("SDK_REAL_GUEST"))
            assertTrue(hello.stdout.contains(if (deviceAbi == "arm64-v8a") "aarch64" else "x86_64"))
            passed("boot-and-real-guest-command")

            ServerSocket(0, 1, InetAddress.getByName("127.0.0.1")).use { server ->
                server.soTimeout = 15_000
                val blocked = runtime.execute(command("""
                    import socket,sys
                    try:
                        s=socket.socket();s.settimeout(3);s.connect(('127.0.0.1',${server.localPort}))
                    except OSError as error:
                        assert error.errno in (1,13),repr(error)
                        print('NETWORK_DISABLED')
                    else:
                        sys.exit(29)
                """.trimIndent(), python = true))
                assertEquals(blocked.stderr, 0, blocked.exitCode)
                assertTrue(blocked.stdout.contains("NETWORK_DISABLED"))
                assertTrue(blocked.enforcement.networkPolicyEnforced)
                passed("disabled-network-blocks-live-listener")
                val accept = async(Dispatchers.IO) {
                    server.accept().use { socket ->
                        socket.getOutputStream().write("SDK_LOOPBACK".toByteArray())
                        socket.getOutputStream().flush()
                    }
                }
                val loopback = runtime.execute(command("""
                    import socket
                    s=socket.socket();s.settimeout(5);s.connect(('127.0.0.1',${server.localPort}))
                    assert s.recv(32)==b'SDK_LOOPBACK'
                    print('LOOPBACK_ALLOWED')
                    s.close()
                    try:
                        s=socket.socket();s.settimeout(2);s.connect(('192.0.2.1',9))
                    except OSError as error:
                        assert error.errno in (1,13),repr(error)
                        print('NONLOOPBACK_DENIED')
                    else:
                        raise AssertionError('non-loopback permitted')
                """.trimIndent(), NetworkPolicyFfi.LOOPBACK_ONLY, python = true))
                assertEquals(loopback.stderr, 0, loopback.exitCode)
                accept.await()
                assertTrue(loopback.stdout.contains("LOOPBACK_ALLOWED"))
                assertTrue(loopback.stdout.contains("NONLOOPBACK_DENIED"))
                assertTrue(loopback.enforcement.networkPolicyEnforced)
                passed("loopback-allowed-external-denied")
            }

            val raw = runtime.handle.openRawStdio(command(
                "import os; data=os.read(0,4096); os.write(1,data); os.write(2,b'ERR\\x00\\xff')",
                python = true, raw = true,
            ))
            val payload = byteArrayOf(0, 1, 10, 13, 127, 128.toByte(), 255.toByte())
            runtime.handle.writeRawStdio(raw.id, payload)
            val stdout = ByteArrayOutputStream()
            val stderr = ByteArrayOutputStream()
            withTimeout(15_000) {
                while (true) {
                    val chunk = runtime.handle.readRawStdio(raw.id, 64u)
                    stdout.write(chunk.stdout); stderr.write(chunk.stderr)
                    if (chunk.closed) { assertEquals(0, chunk.exitCode); break }
                    delay(20)
                }
            }
            assertArrayEquals(payload, stdout.toByteArray())
            assertArrayEquals(byteArrayOf(69, 82, 82, 0, 255.toByte()), stderr.toByteArray())
            runtime.handle.closeRawStdio(raw.id)
            passed("raw-stdio-binary-and-final-drain")

            val blockedStdin = withTimeout(10_000) {
                runtime.execute(command("sleep 60").copy(stdin = "x".repeat(2 * 1024 * 1024), timeoutMs = 300uL))
            }
            assertTrue("Blocked child stdin must not defer its timeout", blockedStdin.timedOut)
            passed("blocked-stdin-timeout-and-reap")

            val memorySession = runtime.handle.openRawStdio(command(
                "import os,time; print(os.getpid(),flush=True); time.sleep(0.3); allocation=bytearray(128*1024*1024); time.sleep(60)",
                python = true, raw = true,
            ).copy(resourceLimits = ResourceLimitsFfi(null, 32u, null, null)))
            assertTrue("raw memory receipt must reflect an active watchdog", memorySession.enforcement.memoryLimitEnforced)
            val memoryOutput = ByteArrayOutputStream()
            withTimeout(15_000) {
                while (true) {
                    try {
                        val chunk = runtime.handle.readRawStdio(memorySession.id, 64u)
                        memoryOutput.write(chunk.stdout)
                        assertFalse("memory overrun must return a typed limit error", chunk.closed)
                    } catch (limit: MobileLinuxApiErrorFfi.ResourceLimitExceeded) {
                        assertTrue(limit.detail.contains("resident-memory limit"))
                        break
                    }
                    delay(25)
                }
            }
            val memoryPid = memoryOutput.toString(Charsets.UTF_8.name()).trim().toInt()
            try {
                runtime.handle.closeRawStdio(memorySession.id)
                fail("close must preserve the typed resource limit result")
            } catch (limit: MobileLinuxApiErrorFfi.ResourceLimitExceeded) {
                assertTrue(limit.detail.contains("resident-memory limit"))
            }
            assertEquals(MobileLinuxTaskStateFfi.FAILED, runtime.handle.taskStatus(memorySession.id)?.status)
            val memoryReaped = runtime.execute(command("test ! -e /proc/$memoryPid"))
            assertEquals("memory-limited guest must be reaped: " + memoryReaped.stderr, 0, memoryReaped.exitCode)
            passed("raw-memory-watchdog-terminates-overrun")

            val pty = runtime.handle.openPty(MobileLinuxPtyOpenRequestFfi(
                command = "/bin/sh", args = listOf("-c", "printf 'SDK_PTY_READY\\n'; sleep 60"),
                cwd = "/root", env = emptyList(), size = MobileLinuxPtySizeFfi(80u, 24u), mounts = emptyList(),
            ))
            eventsUntil(runtime.handle) { it.sessionId == pty.id && it.kind == MobileLinuxEventKindFfi.PTY_OUTPUT &&
                it.data?.toString(Charsets.UTF_8)?.contains("SDK_PTY_READY") == true }
            runtime.handle.resizePty(pty, MobileLinuxPtySizeFfi(100u, 32u))
            runtime.handle.closePty(pty)
            eventsUntil(runtime.handle) { it.sessionId == pty.id && it.kind == MobileLinuxEventKindFfi.PTY_CLOSED }
            passed("pty-output-resize-close")

            val background = runtime.handle.spawnBackground(command(
                "import os,time; print(os.getpid(),flush=True); time.sleep(60)", python = true,
            ).copy(timeoutMs = null))
            val pidEvent = eventsUntil(runtime.handle) { it.taskId == background.id && it.kind == MobileLinuxEventKindFfi.STDOUT_LINE }
            val pid = checkNotNull(pidEvent.text).trim().toInt()
            runtime.handle.killProcess(background)
            withTimeout(10_000) {
                while (runtime.handle.taskStatus(background.id)?.status != MobileLinuxTaskStateFfi.CANCELLED) delay(20)
            }
            val reaped = runtime.execute(command("test ! -e /proc/$pid"))
            assertEquals(reaped.stderr, 0, reaped.exitCode)
            passed("background-cancel-and-process-reap")

            runtime.shutdown()
            runtime.handle.destroy()
            runtime = MobileLinuxRuntime.create(config)
            assertEquals(MobileLinuxRootfsStateFfi.READY, runtime.boot().state)
            val restarted = runtime.execute(command("printf SDK_RESTARTED"))
            assertEquals(0, restarted.exitCode)
            assertEquals("SDK_RESTARTED", restarted.stdout)
            passed("shutdown-recreate-restart")
            val evidence = JSONObject().apply {
                put("deviceAbi", Build.SUPPORTED_ABIS.first()); put("sdkInt", Build.VERSION.SDK_INT)
                put("rootfsSha256", archiveSha); put("checks", org.json.JSONArray(checks))
            }
            File(context.filesDir, "sdk-smoke-evidence.json").writeText(evidence.toString(2))
            Log.i("MobileLinuxSdkSmoke", "SDK_SMOKE_EVIDENCE=" + evidence.toString())
        } catch (failure: Throwable) {
            primaryFailure = failure
            throw failure
        } finally {
            // Keep the first failure; cleanup failures still fail an otherwise successful test.
            try {
                runtime.shutdown()
            } catch (cleanupFailure: Throwable) {
                val original = primaryFailure
                if (original == null) throw cleanupFailure
                original.addSuppressed(cleanupFailure)
                Log.e("MobileLinuxSdkSmoke", "cleanup shutdown failed", cleanupFailure)
            } finally {
                runtime.handle.destroy()
            }
        }
      }
    }
}
