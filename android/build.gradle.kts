plugins {
    id("com.android.library") version "9.2.1" apply false
    id("org.jetbrains.kotlin.android") version "2.2.20" apply false
}
allprojects {
    providers.gradleProperty("sdkBuildDir").orNull?.let { layout.buildDirectory.set(file("$it/${project.name}")) }
    group = "io.github.lingxi-coder"
    version = providers.gradleProperty("sdkVersion").getOrElse("0.1.0")
}
