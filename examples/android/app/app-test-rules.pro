# AndroidX Test loads Kotlin runtime classes through its instrumentation runner.
# Retain those classes in the debuggable, minified acceptance app.
-keep class kotlin.** { *; }
-keep class kotlinx.coroutines.** { *; }
# The separate instrumentation APK was compiled against the public SDK API.
# Retain that boundary for acceptance; ordinary app releases use direct calls.
-keep class io.lingxi.mobilelinux.** { *; }
