// Single KMP module that publishes `dev.moq:web-transport` with both JVM and
// Android variants. When `web-transport-ffi` splits further, add sibling
// modules here.

pluginManagement {
    repositories {
        gradlePluginPortal()
        google()
        mavenCentral()
    }

    // Pinning the Android plugin version here lets `build.gradle.kts` apply
    // it via `apply(plugin = "com.android.library")` without redeclaring the
    // version. When `-Pandroid.enabled=true` isn't set, this is dormant and
    // the plugin marker is never resolved.
    plugins {
        id("com.android.library") version "8.7.3"
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
