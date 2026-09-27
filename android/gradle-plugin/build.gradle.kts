plugins { `java-gradle-plugin`; `maven-publish` }
java { sourceCompatibility = JavaVersion.VERSION_17; targetCompatibility = JavaVersion.VERSION_17 }
repositories { google(); mavenCentral() }
dependencies { implementation(localGroovy()); compileOnly("com.android.tools.build:gradle:9.2.1") }
gradlePlugin { plugins { create("mobileLinux") { id = "io.github.lingxi-coder.mobile-linux"; implementationClass = "io.lingxi.mobilelinux.gradle.MobileLinuxPlugin" } } }
publishing { repositories { maven { url = uri(providers.gradleProperty("sdkMavenRepo").getOrElse(rootProject.layout.buildDirectory.dir("maven").get().asFile.absolutePath)) } } }
