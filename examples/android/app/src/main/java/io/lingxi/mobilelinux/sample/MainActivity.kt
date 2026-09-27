package io.lingxi.mobilelinux.sample
import android.app.Activity
import android.os.Bundle
import android.widget.TextView
import io.lingxi.mobilelinux.MobileLinuxRuntime
import io.lingxi.mobilelinux.bindings.RuntimeConfig
import io.lingxi.mobilelinux.bindings.RuntimePlatform
import java.io.File

class MainActivity: Activity() {
    override fun onCreate(state: Bundle?) {
        super.onCreate(state)
        val config=RuntimeConfig(
            platform=RuntimePlatform.ANDROID,
            managedRoot=File(filesDir,"linux").absolutePath,
            appSandboxRoot=filesDir.absolutePath,
            abi=android.os.Build.SUPPORTED_ABIS.first(),
            rootfsVersion="caller-provided-rootfs",
            archiveSha256=null,
            nativeLibraryDir=applicationInfo.nativeLibraryDir,
            workspaceHostPath=null,stableWorkspaceId=null,authorizationFile=null,
            rootfsArchivePath=null,defaultMountPath=null,
            protectedHostRoots=emptyList(),allowedMountRoots=emptyList(),allowedGuestRoots=emptyList(),
        )
        val runtime=MobileLinuxRuntime.create(config)
        setContentView(TextView(this).apply { text="SDK runtime created. Supply a verified rootfs with RootfsInstaller before booting." })
        // Keep the runtime for the Activity lifetime; production apps own its lifecycle.
        retained=runtime
    }
    private var retained: MobileLinuxRuntime?=null
}
