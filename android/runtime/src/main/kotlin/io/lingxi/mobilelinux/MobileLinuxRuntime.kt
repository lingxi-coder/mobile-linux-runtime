package io.lingxi.mobilelinux

import io.lingxi.mobilelinux.bindings.*

/** One persistent runtime. The embedding app owns policy, paths and rootfs distribution. */
class MobileLinuxRuntime private constructor(val handle: RuntimeHandle) {
    companion object {
        fun create(config: RuntimeConfig): MobileLinuxRuntime = MobileLinuxRuntime(createRuntime(config))
    }
    suspend fun boot(): MobileLinuxStatusFfi = handle.boot()
    suspend fun shutdown() = handle.shutdown()
    suspend fun capability(): MobileLinuxCapabilityFfi = handle.capability()
    suspend fun execute(request: MobileLinuxCommandRequestFfi): MobileLinuxCommandResultFfi = handle.runCommand(request)
    // Raw stdio and PTY are deliberately exposed on handle without text conversion.
}
