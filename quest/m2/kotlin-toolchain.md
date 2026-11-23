# [M] Move the Kotlin binding to the current Gradle toolchain

## Goal

The Kotlin binding in `kt/` builds, tests, and publishes on the current major releases of Gradle and the Android Gradle Plugin (AGP), so routine dependency bumps stay possible.
The published Maven coordinates, JVM and Android artifacts, and the `rustls-platform-verifier` AAR bundling keep working.

## Plan

The 2026-11-22 dependency audit found `kt/` a year or more behind, since Dependabot never covered Gradle until that audit added it:

| Dependency | In use | Latest stable |
| --- | --- | --- |
| Gradle wrapper | 8.10.2 | 9.9.2 |
| AGP (`com.android.library`, `com.android.application`) | 8.7.3 | 9.5.2 |
| Kotlin Multiplatform plugin | 2.0.21 | 2.4.21 |
| `com.vanniktech.maven.publish` | 0.30.0 | 0.37.0 |
| `kotlinx-coroutines` | 1.9.0 | 1.11.0 |
| JNA | 5.15.0 | 5.19.1 |
| `compileSdk` | 35 | check the current Android API level |

Dependabot's `gradle` group takes the minor and patch bumps; this quest owns the majors, which it leaves ungrouped.
AGP 9 drops `androidTarget {}` under `com.android.library` for Kotlin Multiplatform libraries in favor of the `com.android.kotlin.multiplatform.library` plugin, so the Android target in `kt/web-transport/build.gradle.kts` needs migrating, not just a version bump.
`kt/r8-check` is a `com.android.application` that consumes the AAR to validate it; it only takes the AGP bump and stays an app.
A PR touching `kt/**` runs `release-kt.yml`, including its Android packaging and R8 check, so that PR's run validates the Android side.

Decided 2026-11-22 (maintainer, /quest-run on #569): ✅ keep this quest in m2 (rejected: drop).
