pluginManagement { resolutionStrategy { eachPlugin { if (requested.id.id == "com.android.library") useModule("com.android.tools.build:gradle:${requested.version}") } }; repositories { google(); mavenCentral(); gradlePluginPortal() } }
dependencyResolutionManagement { repositories { google(); mavenCentral() } }
rootProject.name = "mobile-linux-runtime"
include(":installer", ":native-support", ":runtime", ":gradle-plugin")
