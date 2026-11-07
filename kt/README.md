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
# Build host cdylib, regenerate UniFFI bindings, and test the packaged JVM JAR.
# Requires JDK 17 via JAVA_HOME or PATH; Nix supplies Rust, Just, and OpenSSL.
nix develop --command just test-kotlin

# Equivalent without Nix, with Rust, Just, OpenSSL, and JDK 17 installed:
just test-kotlin

# Assemble the KMP artifacts for publishing (requires per-target libs).
./kt/gradlew -p kt :web-transport:assemble
```

`kt/scripts/check.sh` uses the checksum-pinned Gradle 8.10.2 wrapper, builds
`web-transport-ffi` in release mode for the host, regenerates UniFFI Kotlin
bindings, and stages the host library in the JNA resource layout. Missing tools
or an unsupported host fail the command. Supported hosts are Linux GNU x86_64
and aarch64, macOS x86_64 and aarch64, and Windows MSVC x86_64 under Bash.

`:web-transport:jvmTest` loads main classes and native resources from `jvmJar`,
with loose build outputs removed from its runtime classpath. The smoke test
checks JNA extraction, connects a loopback client/server on an ephemeral port,
exchanges a bidirectional stream, and closes sessions and endpoints. Fresh test
credentials live under `.tmp/` and are removed on exit; the loopback client
disables certificate verification. No publishing or Android SDK is required.
The independent Kotlin/JVM PR job runs this same command on Linux and uploads
JUnit XML and HTML results. The release matrix and packaging remain separate.

After staging with `just test-kotlin`, rerun through that command so test
credentials are regenerated. Results are in
`kt/web-transport/build/reports/tests/jvmTest/index.html`.

Keep each generated `Session` handle alive while using its streams. To close a
session or endpoint, supply code and reason explicitly, for example
`session.close(0u, "done")` or `client.close(0uL, "done")`. Parameterless `close()`
is UniFFI's `AutoCloseable` handle destruction; `use { ... }` releases handles
after explicit lifecycle cleanup.

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
