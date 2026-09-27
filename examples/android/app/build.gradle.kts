plugins { id("com.android.application"); id("org.jetbrains.kotlin.android"); id("io.github.lingxi-coder.mobile-linux") version "0.1.0" }
android {
    namespace="io.lingxi.mobilelinux.sample"
    compileSdk=37
    testBuildType=providers.gradleProperty("sdkTestBuildType").getOrElse("debug")
    defaultConfig { applicationId="io.lingxi.mobilelinux.sample"; minSdk=26; targetSdk=35; versionCode=1; versionName="0.1.0"; testInstrumentationRunner="androidx.test.runner.AndroidJUnitRunner"; testProguardFiles("android-test-rules.pro"); ndk { abiFilters += listOf("arm64-v8a", "x86_64") } }
    compileOptions { sourceCompatibility=JavaVersion.VERSION_17; targetCompatibility=JavaVersion.VERSION_17 }
    kotlinOptions { jvmTarget="17" }
    buildTypes.getByName("release") {
        isMinifyEnabled=true
        if (providers.gradleProperty("sdkAcceptanceTest").orNull == "true") {
            // Local acceptance calls SDK APIs from a separate instrumentation APK.
            signingConfig=signingConfigs.getByName("debug")
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "app-test-rules.pro")
        } else {
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"))
        }
    }
}
dependencies { implementation("io.github.lingxi-coder:mobile-linux-runtime:0.1.0") }

dependencies {
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
    androidTestImplementation("androidx.test:runner:1.7.0")
    // AndroidX Test references these annotation classes when R8 minifies the test APK.
    androidTestImplementation("com.google.errorprone:error_prone_annotations:2.36.0")
    androidTestImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")
}
