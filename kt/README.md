# web-transport (Kotlin)

Kotlin/Android bindings for [web-transport-ffi](../rs/web-transport-ffi) via [UniFFI](https://mozilla.github.io/uniffi-rs/).

Single Kotlin Multiplatform module that publishes `dev.moq:web-transport` with both JVM and Android variants under one coordinate. Consumers add `dev.moq:web-transport:VERSION` and Gradle metadata resolution picks the right artifact for their target.

## Install

```kotlin
// build.gradle.kts
dependencies {
    implementation("dev.moq:web-transport:0.1.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
}
```

## Build

```sh
# Build host cdylib, regenerate UniFFI bindings, run :web-transport:jvmTest.
just kt check

# Assemble the KMP artifacts for publishing (requires per-target libs).
gradle -p kt :web-transport:assemble
```

`kt/scripts/check.sh` builds `web-transport-ffi` for the host, regenerates the UniFFI Kotlin bindings, drops the host cdylib into the JNA-resource layout, and runs `gradle :web-transport:jvmTest`. Skips cleanly without a JDK or `cargo`.

The Android target is opt-in via `-Pandroid.enabled=true`. Local dev without the Android SDK still builds the JVM variant.

## Layout

```
kt/
  build.gradle.kts          Root config (group, version)
  settings.gradle.kts       include(":web-transport"), pins AGP version
  gradle.properties         Defaults: version, android.useAndroidX, etc.
  web-transport/
    build.gradle.kts        KMP plugin, jvm() always, androidTarget() conditional
    android.gradle.kts      Applied only when -Pandroid.enabled=true
    src/
      commonMain/           Public Kotlin facade (reserved for future K/N)
      jvmAndAndroidMain/    UniFFI-generated kotlin (populated, gitignored)
      jvmMain/resources/    Native libs at JNA paths (populated, gitignored)
      androidMain/jniLibs/  JNI .so per ABI (populated, gitignored)
  scripts/                  check.sh, package.sh
```

## Publishing

Publishing to Maven Central is CI-only via `.github/workflows/release-kt.yml`, which runs on every `web-transport-ffi-v*` tag. See the moq-dev org docs for Sonatype credentials.
