pluginManagement { repositories { maven { url=uri(providers.gradleProperty("sdkMavenRepo").getOrElse("../../android/build/maven")) }; google(); mavenCentral(); gradlePluginPortal() } }
dependencyResolutionManagement { repositories { maven { url=uri(providers.gradleProperty("sdkMavenRepo").getOrElse("../../android/build/maven")) }; google(); mavenCentral() } }
rootProject.name="MobileLinuxSample"
include(":app")
