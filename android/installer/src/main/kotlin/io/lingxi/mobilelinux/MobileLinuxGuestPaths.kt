package io.lingxi.mobilelinux

/** Android twin of the Rust and Swift mobile guest path contract. */
object MobileLinuxGuestPaths {
    const val USER = "lingxi"
    const val UID = 1000
    const val HOME = "/home/lingxi"
    const val ADMIN_HOME = "/root"
    const val WORKSPACE_ROOT = "$HOME/lingxi"
    fun workspace(id: String): String = "$WORKSPACE_ROOT/$id"
    val writableRoots: Set<String> = linkedSetOf(HOME, ADMIN_HOME, "/tmp", "/var/tmp", WORKSPACE_ROOT)
}
