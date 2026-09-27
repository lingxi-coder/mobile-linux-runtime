plugins { id("com.android.application"); id("org.jetbrains.kotlin.android"); id("io.github.lingxi-coder.mobile-linux") version "0.1.0" }
android {
    namespace="io.lingxi.mobilelinux.sample"
    compileSdk=37
    defaultConfig { applicationId="io.lingxi.mobilelinux.sample"; minSdk=26; targetSdk=35; versionCode=1; versionName="0.1.0"; testInstrumentationRunner="androidx.test.runner.AndroidJUnitRunner"; ndk { abiFilters += listOf("arm64-v8a", "x86_64") } }
    compileOptions { sourceCompatibility=JavaVersion.VERSION_17; targetCompatibility=JavaVersion.VERSION_17 }
    kotlinOptions { jvmTarget="17" }
}
dependencies { implementation("io.github.lingxi-coder:mobile-linux-runtime:0.1.0") }

dependencies {
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
    androidTestImplementation("androidx.test:runner:1.7.0")
    androidTestImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")
}
