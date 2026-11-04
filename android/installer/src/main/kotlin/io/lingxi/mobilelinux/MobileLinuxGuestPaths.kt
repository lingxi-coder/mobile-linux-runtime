package io.lingxi.mobilelinux

/** Product identity supplied by the embedding app and matched against its rootfs. */
data class BrandRuntimeProfile(
    val brandId: String,
    val linuxUser: String,
    val configDirName: String,
) {
    init {
        require(brandId.length in 1..63 && brandId.matches(Regex("[a-z0-9][a-z0-9._-]*"))) {
            "brand id must be one lowercase path component"
        }
        require(linuxUser.length in 1..32 && linuxUser != "root" &&
            linuxUser.matches(Regex("[a-z_][a-z0-9_-]*"))) {
            "Linux user must be a safe non-root account name"
        }
        require(configDirName.length in 2..64 && configDirName.matches(Regex("\\.[a-z0-9][a-z0-9._-]*"))) {
            "config directory name must be one hidden path component"
        }
    }
}

/** Android twin of the profile-specific Rust and Swift guest path contract. */
object MobileLinuxGuestPaths {
    const val UID = 1000
    const val ADMIN_HOME = "/root"

    fun home(profile: BrandRuntimeProfile): String = "/home/${profile.linuxUser}"
    fun workspaceRoot(profile: BrandRuntimeProfile): String = "${home(profile)}/${profile.configDirName}"
    fun workspace(profile: BrandRuntimeProfile, id: String): String = "${workspaceRoot(profile)}/$id"
    fun writableRoots(profile: BrandRuntimeProfile): Set<String> =
        linkedSetOf(home(profile), ADMIN_HOME, "/tmp", "/var/tmp", workspaceRoot(profile))
}
