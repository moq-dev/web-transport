// Single KMP module that publishes `dev.moq:web-transport` with both JVM and
// Android variants. When `web-transport-ffi` splits further, add sibling
// modules here.

pluginManagement {
    repositories {
        gradlePluginPortal()
        google()
        mavenCentral()
    }

    // Pins the Android plugins for r8-check, which applies them without a version.
    // Without `-Pandroid.enabled=true` the plugin markers are never resolved.
    plugins {
        id("com.android.kotlin.multiplatform.library") version "9.5.2"
        id("com.android.application") version "9.5.2"
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
        // rustls-platform-verifier publishes its Android component only here;
        // web-transport/build.gradle.kts bundles it into the AAR.
        exclusiveContent {
            forRepository {
                maven("https://github.com/rustls/rustls-platform-verifier/raw/maven-archive/android-release-support/maven/")
            }
            filter { includeGroup("org.rustls") }
        }
    }
}

rootProject.name = "web-transport"
include(":web-transport")

// Minified consumer that checks the AAR's R8 keep rules; needs the Android SDK.
if (providers.gradleProperty("android.enabled").orNull == "true") {
    include(":r8-check")
}
