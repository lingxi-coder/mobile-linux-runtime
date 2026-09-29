plugins { id("com.android.library"); id("org.jetbrains.kotlin.android"); id("maven-publish") }
extensions.configure<com.android.build.api.dsl.LibraryExtension> {
    namespace = "io.lingxi.mobilelinux.nativesupport"
    compileSdk = 37
    defaultConfig { minSdk = 26;  }
    compileOptions { sourceCompatibility = JavaVersion.VERSION_17; targetCompatibility = JavaVersion.VERSION_17 }
    sourceSets.getByName("main").assets.srcDir(providers.gradleProperty("nativeArtifacts").map { "$it/licenses" }.getOrElse("build/missing/licenses"))
    publishing { singleVariant("release") { withSourcesJar() } }
    sourceSets.getByName("main").jniLibs.srcDir(providers.gradleProperty("nativeArtifacts").map { "$it/jniLibs" }.getOrElse("build/missing/jniLibs"))
}
dependencies {  }
afterEvaluate { publishing { publications { create<MavenPublication>("release") {
    from(components["release"])
    artifactId = "mobile-linux-native-support"
    pom { name.set("Mobile Linux native-support"); description.set("Standalone mobile Linux SDK native-support"); url.set("https://github.com/lingxi-coder/mobile-linux-runtime"); licenses { license { name.set("GPL-3.0 (OpenMinis); GPL-2.0-or-later (PRoot); LGPL-3.0-or-later (talloc); MIT OR Apache-2.0 (SDK wrapper)") } } }
} }; repositories { maven { url = uri(providers.gradleProperty("sdkMavenRepo").getOrElse(rootProject.layout.buildDirectory.dir("maven").get().asFile.absolutePath)) } } } }

tasks.withType<org.jetbrains.kotlin.gradle.tasks.KotlinCompile>().configureEach { compilerOptions.jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) }

tasks.named("preBuild") { doFirst {
    val artifacts = providers.gradleProperty("nativeArtifacts").orNull ?: error("Supply -PnativeArtifacts=<validated SDK artifacts>")
    for (abi in listOf("arm64-v8a", "x86_64")) for (name in listOf("libproot.so", "libproot-loader.so", "libmobile_linux_policy_launcher.so")) {
        check(file("$artifacts/jniLibs/$abi/$name").isFile) { "Missing SDK artifact $abi/$name" }
    }
} }

val nativeManifestAssets = layout.buildDirectory.dir("generated/nativeManifest").get().asFile
val stageNativeManifest by tasks.registering(Copy::class) {
    from(providers.gradleProperty("nativeArtifacts").map { "$it/native-manifest.json" })
    into(File(nativeManifestAssets,"mobile-linux"))
}
extensions.configure<com.android.build.api.dsl.LibraryExtension> {
    sourceSets.getByName("main").assets.srcDir(nativeManifestAssets)
    packaging.jniLibs.keepDebugSymbols += "**/*.so"
}
tasks.named("preBuild") { dependsOn(stageNativeManifest) }
