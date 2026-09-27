plugins { id("com.android.library"); id("org.jetbrains.kotlin.android"); id("maven-publish") }
extensions.configure<com.android.build.api.dsl.LibraryExtension> {
    namespace = "io.lingxi.mobilelinux.runtime"
    compileSdk = 37
    defaultConfig { minSdk = 26; consumerProguardFiles("consumer-rules.pro") }
    compileOptions { sourceCompatibility = JavaVersion.VERSION_17; targetCompatibility = JavaVersion.VERSION_17 }
    publishing { singleVariant("release") { withSourcesJar() } }
    sourceSets.getByName("main").assets.srcDir(providers.gradleProperty("ffiArtifacts").map { "$it/licenses" }.getOrElse("build/missing/licenses"))
    sourceSets.getByName("main").jniLibs.srcDir(providers.gradleProperty("ffiArtifacts").map { "$it/jniLibs" }.getOrElse("build/missing/jniLibs"))
    sourceSets.getByName("main").java.srcDir(providers.gradleProperty("ffiArtifacts").map { "$it/kotlin" }.getOrElse("build/missing/kotlin"))
}
dependencies { api(project(":installer"))
    api(project(":native-support"))
    implementation("net.java.dev.jna:jna:5.19.0@aar")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0") }
afterEvaluate { publishing { publications { create<MavenPublication>("release") {
    from(components["release"])
    artifactId = "mobile-linux-runtime"
    pom { name.set("Mobile Linux runtime"); description.set("Standalone mobile Linux SDK runtime"); url.set("https://github.com/lingxi-coder/mobile-linux-runtime"); licenses { license { name.set("MIT OR Apache-2.0") } } }
} }; repositories { maven { url = uri(providers.gradleProperty("sdkMavenRepo").getOrElse(rootProject.layout.buildDirectory.dir("maven").get().asFile.absolutePath)) } } } }

tasks.withType<org.jetbrains.kotlin.gradle.tasks.KotlinCompile>().configureEach { compilerOptions.jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) }

tasks.named("preBuild") { doFirst {
    val artifacts = providers.gradleProperty("ffiArtifacts").orNull ?: error("Supply -PffiArtifacts=<validated SDK artifacts>")
    for (abi in listOf("arm64-v8a", "x86_64")) for (name in listOf("libmobile_linux_runtime.so")) {
        check(file("$artifacts/jniLibs/$abi/$name").isFile) { "Missing SDK artifact $abi/$name" }
    }
} }
