plugins { id("com.android.library"); id("org.jetbrains.kotlin.android"); id("maven-publish") }
extensions.configure<com.android.build.api.dsl.LibraryExtension> {
    namespace = "io.lingxi.mobilelinux.installer"
    compileSdk = 37
    defaultConfig { minSdk = 26 }
    compileOptions { sourceCompatibility = JavaVersion.VERSION_17; targetCompatibility = JavaVersion.VERSION_17 }
    publishing { singleVariant("release") { withSourcesJar() } }
}
dependencies { implementation("org.apache.commons:commons-compress:1.27.1")
    testImplementation("junit:junit:4.13.2") }
afterEvaluate { publishing { publications { create<MavenPublication>("release") {
    from(components["release"])
    artifactId = "mobile-linux-installer"
    pom { name.set("Mobile Linux installer"); description.set("Standalone mobile Linux SDK installer"); url.set("https://github.com/lingxi-coder/mobile-linux-runtime"); licenses { license { name.set("MIT") } } }
} }; repositories { maven { url = uri(providers.gradleProperty("sdkMavenRepo").getOrElse(rootProject.layout.buildDirectory.dir("maven").get().asFile.absolutePath)) } } } }

tasks.withType<org.jetbrains.kotlin.gradle.tasks.KotlinCompile>().configureEach { compilerOptions.jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) }
