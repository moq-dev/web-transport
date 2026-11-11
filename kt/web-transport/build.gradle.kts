// Kotlin Multiplatform module for web-transport-ffi.
//
// Publishes `dev.moq:web-transport` with both JVM and Android variants. Consumers add
// `dev.moq:web-transport:VERSION` and Gradle metadata resolution picks the right one.
//
// Source set hierarchy:
//   commonMain                       (empty today; reserved for future K/N targets)
//   └─ jvmAndAndroidMain             Wrappers + UniFFI-generated kotlin (uses JNA)
//      ├─ jvmMain                    JVM-specific: native libs as JAR resources
//      └─ androidMain                Android-specific: native libs in jniLibs, and a
//                                    startup provider that loads them via System.loadLibrary
//
// Native libraries are populated by `kt/scripts/package.sh`:
//   src/jvmMain/resources/<os>-<arch>/<libname>              (JNA classpath layout)
//   src/androidMain/jniLibs/<abi>/libweb_transport_ffi.so    (Android packaging layout)
//
// Android target is opt-in via `-Pandroid.enabled=true` so contributors
// without the Android SDK (or Google maven access) can still build/test
// the JVM variant. CI always sets the flag. The Android-specific config is
// inlined here (gated by androidEnabled) instead of in a separate
// `apply(from = "android.gradle.kts")` script because Gradle Kotlin DSL
// doesn't generate type-safe accessors for plugins applied from
// `apply(from = ...)` files — that path failed in CI with "Unresolved
// reference: kotlin / androidTarget / compileOptions" once -Pandroid.enabled
// was set.
//
// Publishing uses com.vanniktech.maven.publish, which handles the Sonatype
// Central Portal upload protocol + GPG signing in a single Gradle task.
// CI runs `:web-transport:publishAndReleaseToMavenCentral`. Credentials are picked
// up from env vars set by kotlin.yml:
//   ORG_GRADLE_PROJECT_mavenCentralUsername
//   ORG_GRADLE_PROJECT_mavenCentralPassword
//   ORG_GRADLE_PROJECT_signingInMemoryKey
//   ORG_GRADLE_PROJECT_signingInMemoryKeyPassword
// signAllPublications() is gated on ORG_GRADLE_PROJECT_signingInMemoryKey
// being present so PR smoke tests (which run publishToMavenLocal without
// the signing key) don't trip on "no configured signatory".

import com.vanniktech.maven.publish.SonatypeHost
import java.security.MessageDigest
import java.time.Duration
import org.gradle.api.tasks.testing.logging.TestExceptionFormat
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

val androidEnabled = providers.gradleProperty("android.enabled").orNull == "true"
val artifactSmokeEnabled = providers.gradleProperty("artifactSmoke").orNull == "true"

plugins {
    kotlin("multiplatform") version "2.0.21"
    id("com.vanniktech.maven.publish") version "0.30.0"
    // AGP needs to be on the buildscript classpath (so the Kotlin DSL
    // resolves `androidTarget {}`, `LibraryExtension`, `ndk {}`, etc.)
    // *and* its version pinned here, but `apply false` keeps it off the
    // project unless `-Pandroid.enabled=true`. The pluginManagement block
    // in settings.gradle.kts only resolves the plugin marker; the
    // classpath/types come from this block.
    id("com.android.library") version "8.7.3" apply false
}

// AGP is applied imperatively only when the Android target is enabled, so
// contributors without Google maven access can still build the JVM variant.
if (androidEnabled) {
    apply(plugin = "com.android.library")
}

// rustls-platform-verifier verifies on Android through a small Kotlin component
// that is not on Maven Central. Bundle its classes into the AAR so consumers need
// no extra repository. Only the classes are taken: its manifest would impose an
// app-wide network security config, which only aids best-effort revocation fetches.
val rustlsPlatformVerifierJar: FileCollection? = if (androidEnabled) {
    val aar = configurations.create("rustlsPlatformVerifier") {
        isCanBeConsumed = false
        isTransitive = false
    }
    val version = providers.gradleProperty("rustlsPlatformVerifier.version").get()
    dependencies.add(aar.name, "org.rustls:rustls-platform-verifier:$version@aar")
    val expectedSha256 = providers.gradleProperty("rustlsPlatformVerifier.sha256").get()
    val extract = tasks.register<Copy>("extractRustlsPlatformVerifier") {
        val aarFiles: FileCollection = aar
        inputs.property("sha256", expectedSha256)
        from({ zipTree(aarFiles.singleFile) }) { include("classes.jar") }
        into(layout.buildDirectory.dir("rustls-platform-verifier"))
        doFirst {
            val digest = MessageDigest.getInstance("SHA-256").digest(aarFiles.singleFile.readBytes())
            val actual = digest.joinToString("") { "%02x".format(it) }
            check(actual == expectedSha256) {
                "rustls-platform-verifier AAR SHA-256 is $actual, expected $expectedSha256"
            }
        }
    }
    files(layout.buildDirectory.file("rustls-platform-verifier/classes.jar")).builtBy(extract)
} else {
    null
}

kotlin {
    jvm()

    if (androidEnabled) {
        androidTarget {
            publishLibraryVariants("release")
            compilerOptions { jvmTarget.set(JvmTarget.JVM_17) }
        }
    }

    @Suppress("UNUSED_VARIABLE")
    sourceSets {
        val commonMain by getting {
            dependencies {
                implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
            }
        }
        val commonTest by getting {
            dependencies {
                implementation(kotlin("test"))
                implementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.9.0")
            }
        }

        val jvmAndAndroidMain by creating {
            dependsOn(commonMain)
            dependencies {
                // compileOnly: each platform's runtime adds its own JNA artifact.
                compileOnly("net.java.dev.jna:jna:5.15.0")
            }
        }
        val jvmAndAndroidTest by creating {
            dependsOn(commonTest)
        }

        val jvmMain by getting {
            dependsOn(jvmAndAndroidMain)
            dependencies {
                implementation("net.java.dev.jna:jna:5.15.0")
            }
        }
        val jvmTest by getting {
            dependsOn(jvmAndAndroidTest)
            if (artifactSmokeEnabled) {
                kotlin.srcDir("src/jvmArtifactTest/kotlin")
            }
        }

        if (androidEnabled) {
            val androidMain by getting {
                dependsOn(jvmAndAndroidMain)
                dependencies {
                    implementation("net.java.dev.jna:jna:5.15.0@aar")
                    implementation(rustlsPlatformVerifierJar!!)
                }
            }
            val androidUnitTest by getting {
                dependsOn(jvmAndAndroidTest)
            }
        }
    }
}

if (androidEnabled) {
    extensions.configure<com.android.build.gradle.LibraryExtension>("android") {
        namespace = "dev.moq.webtransport"
        compileSdk = 35
        defaultConfig {
            minSdk = 24
            ndk {
                abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86_64")
            }
            consumerProguardFiles("consumer-rules.pro")
        }
        compileOptions {
            sourceCompatibility = JavaVersion.VERSION_17
            targetCompatibility = JavaVersion.VERSION_17
        }
        publishing {
            singleVariant("release") {
                withSourcesJar()
            }
        }
        sourceSets.getByName("main").jniLibs.srcDirs("src/androidMain/jniLibs")
    }
}

// Exercise the same JAR consumers load, without loose main classes/resources.
if (artifactSmokeEnabled) {
    tasks.named<Test>("jvmTest") {
        val jar = tasks.named<Jar>("jvmJar")
        val main = kotlin.targets.getByName("jvm").compilations.getByName("main")
        classpath = files(jar) + (classpath - main.output.allOutputs)
        systemProperty("jna.tmpdir", temporaryDir.resolve("jna").apply { mkdirs() })
        systemProperty("jna.debug_load", "true")
        timeout.set(Duration.ofMinutes(2))
        testLogging {
            events("passed", "skipped", "failed")
            exceptionFormat = TestExceptionFormat.FULL
            showStandardStreams = true
        }
    }
}

mavenPublishing {
    publishToMavenCentral(SonatypeHost.CENTRAL_PORTAL, automaticRelease = true)
    // Only sign when a non-empty signing key is configured. PR smoke tests
    // run gradle `:web-transport:publishToMavenLocal` (via package.sh)
    // without the SIGNING_* env vars; if SIGNING_KEY is declared in repo
    // secrets but unset, `${{ secrets.SIGNING_KEY }}` expands to the empty
    // string and the env var ends up as "" — a bare null-check would call
    // signAllPublications() against an empty key and fail with "Could not
    // read PGP secret key". isNullOrBlank() catches both cases. The
    // release-kt workflow always sets a real ORG_GRADLE_PROJECT_signingInMemoryKey
    // before publishAndReleaseToMavenCentral, so real releases sign as expected.
    if (!System.getenv("ORG_GRADLE_PROJECT_signingInMemoryKey").isNullOrBlank()) {
        signAllPublications()
    }
    coordinates("dev.moq", "web-transport", version.toString())

    pom {
        name.set("web-transport")
        description.set("Kotlin bindings for WebTransport over HTTP/3")
        url.set("https://github.com/moq-dev/web-transport")
        licenses {
            license {
                name.set("MIT OR Apache-2.0")
                url.set("https://github.com/moq-dev/web-transport/blob/main/LICENSE-APACHE")
            }
        }
        developers {
            developer {
                id.set("moq-dev")
                name.set("moq-dev")
                url.set("https://github.com/moq-dev")
            }
        }
        scm {
            url.set("https://github.com/moq-dev/web-transport")
            connection.set("scm:git:https://github.com/moq-dev/web-transport.git")
            developerConnection.set("scm:git:ssh://git@github.com/moq-dev/web-transport.git")
        }
    }
}
