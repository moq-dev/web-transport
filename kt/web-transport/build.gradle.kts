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
// without the Android SDK can still build/test the JVM variant. CI always sets
// the flag. AGP's Kotlin Multiplatform plugin is on the build classpath either
// way, so the Android configuration below compiles; it is applied only when
// enabled.
//
// Publishing uses com.vanniktech.maven.publish, which handles the Sonatype
// Central Portal upload protocol + GPG signing in a single Gradle task.
// CI runs `:web-transport:publishAndReleaseToMavenCentral`. Credentials are picked
// up from env vars set by release-kt.yml:
//   ORG_GRADLE_PROJECT_mavenCentralUsername
//   ORG_GRADLE_PROJECT_mavenCentralPassword
//   ORG_GRADLE_PROJECT_signingInMemoryKey
//   ORG_GRADLE_PROJECT_signingInMemoryKeyPassword
// signAllPublications() is gated on ORG_GRADLE_PROJECT_signingInMemoryKey
// being present so PR smoke tests (which run publishToMavenLocal without
// the signing key) don't trip on "no configured signatory".

import com.android.build.api.dsl.KotlinMultiplatformAndroidLibraryTarget
import java.security.MessageDigest
import java.time.Duration
import org.gradle.api.tasks.testing.logging.TestExceptionFormat
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

val androidEnabled = providers.gradleProperty("android.enabled").orNull == "true"
val artifactSmokeEnabled = providers.gradleProperty("artifactSmoke").orNull == "true"

plugins {
    kotlin("multiplatform") version "2.4.21"
    id("com.vanniktech.maven.publish") version "0.37.0"
    // On the classpath for the Android types below; settings.gradle.kts pins the version.
    id("com.android.kotlin.multiplatform.library") apply false
}

if (androidEnabled) {
    apply(plugin = "com.android.kotlin.multiplatform.library")
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
        from(aarFiles.elements.map { zipTree(it.single()) }) { include("classes.jar") }
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
        // Registered by the Android plugin, so it has no generated accessor.
        extensions.configure<KotlinMultiplatformAndroidLibraryTarget>("android") {
            namespace = "dev.moq.webtransport"
            compileSdk = 37
            minSdk = 24
            // AGP otherwise makes consumers compile against at least our compileSdk.
            aarMetadata.minCompileSdk = minSdk
            compilerOptions { jvmTarget.set(JvmTarget.JVM_17) }
            optimization {
                consumerKeepRules.apply {
                    publish = true
                    file("consumer-rules.pro")
                }
            }
        }
    }

    sourceSets {
        val commonMain = getByName("commonMain") {
            dependencies {
                implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.11.0")
            }
        }
        getByName("commonTest") {
            dependencies {
                implementation(kotlin("test"))
            }
        }

        val jvmAndAndroidMain = create("jvmAndAndroidMain") {
            dependsOn(commonMain)
            dependencies {
                // compileOnly: each platform's runtime adds its own JNA artifact.
                compileOnly("net.java.dev.jna:jna:5.19.1")
            }
        }

        getByName("jvmMain") {
            dependsOn(jvmAndAndroidMain)
            dependencies {
                implementation("net.java.dev.jna:jna:5.19.1")
            }
        }
        if (artifactSmokeEnabled) {
            getByName("jvmTest").kotlin.srcDir("src/jvmArtifactTest/kotlin")
        }

        if (androidEnabled) {
            getByName("androidMain") {
                dependsOn(jvmAndAndroidMain)
                dependencies {
                    implementation("net.java.dev.jna:jna:5.19.1@aar")
                    implementation(rustlsPlatformVerifierJar!!)
                }
            }
        }
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
    publishToMavenCentral(automaticRelease = true)
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
