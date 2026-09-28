# UniFFI uses JNA to look up the Rust symbols and marshal structure fields.
# Keep the generated ABI names, callbacks and JNA reflection targets intact.
-keep class io.lingxi.mobilelinux.bindings.** { *; }
-keep class com.sun.jna.** { *; }
-keep class * implements com.sun.jna.Library { *; }
-keep class * implements com.sun.jna.Callback { *; }
-dontwarn com.sun.jna.**
