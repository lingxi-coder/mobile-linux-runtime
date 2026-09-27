# AndroidX Test pulls Error Prone annotations that reference this JDK-only type.
-dontwarn javax.lang.model.element.Modifier
# JUnit discovers this instrumentation method by reflection after test-APK R8.
-keep class io.lingxi.mobilelinux.sample.RealRuntimeSmokeTest { *; }
-keepattributes *Annotation*
