plugins { id("com.android.application") }
android {
    namespace = "io.lingxi.forkdiagnostic"
    compileSdk = 35
    defaultConfig {
        applicationId = "io.lingxi.forkdiagnostic"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "1"
    }
    packaging { jniLibs { useLegacyPackaging = true } }
}
