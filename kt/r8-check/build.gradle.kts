// Minified Android app that depends on the library with no keep rules of its own,
// so `checkKeepRules` proves the AAR's consumer rules protect every class that
// native code or JNA reaches by name.

plugins {
    id("com.android.application")
}

android {
    namespace = "dev.moq.webtransport.r8check"
    compileSdk = 35
    defaultConfig {
        minSdk = 24
    }
    buildTypes {
        release {
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"))
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation(project(":web-transport"))
}

val checkKeepRules by tasks.registering {
    description = "Fails if R8 removed or renamed anything the library reaches by name."
    val outputs = layout.buildDirectory.dir("outputs/mapping/release")
    val mapping = outputs.map { it.file("mapping.txt") }
    val usage = outputs.map { it.file("usage.txt") }
    dependsOn("minifyReleaseWithR8")
    inputs.files(mapping, usage)
    doLast {
        val packages = listOf("uniffi.web_transport.", "com.sun.jna.", "org.rustls.platformverifier.")
        fun guarded(name: String) = packages.any { name.startsWith(it) }
        val problems = mutableListOf<String>()

        // R8 generates these itself, so nothing can reach them by name. A folded
        // static initializer is no loss either.
        fun synthetic(name: String) = "\$\$ExternalSynthetic" in name || "\$r8\$" in name || name == "<clinit>"

        // usage.txt lists each removed class bare, and each class that only lost
        // members as a `:`-suffixed header above its indented members.
        var partial: String? = null
        for (line in usage.get().asFile.readLines()) {
            if (!line.startsWith(" ")) {
                partial = line.removeSuffix(":").takeIf { line.endsWith(":") && guarded(it) }
                if (!line.endsWith(":") && guarded(line)) problems += "removed: $line"
            } else if (partial != null) {
                val member = line.trim().substringBefore("(").substringAfterLast(" ")
                if (!synthetic(member)) problems += "removed from $partial: ${line.trim()}"
            }
        }

        // Members map to new names in mapping.txt, interleaved with frames R8 inlined
        // into them. A kept member maps to its own name, so every target must be
        // one of the class's original names.
        val kept = mutableSetOf<String>()
        val members = mutableMapOf<String, Pair<MutableSet<String>, MutableSet<String>>>()
        var current: Pair<MutableSet<String>, MutableSet<String>>? = null
        for (line in mapping.get().asFile.readLines()) {
            if (line.isBlank() || line.trim().startsWith("#")) continue
            val (original, renamed) = line.trim().removeSuffix(":").split(" -> ")
            if (!line.startsWith(" ")) {
                current = null
                if (!guarded(original) || synthetic(original)) continue
                kept += packages.first { original.startsWith(it) }
                if (original != renamed) problems += "renamed: $original -> $renamed"
                current = members.getOrPut(original) { mutableSetOf<String>() to mutableSetOf() }
            } else if (current != null) {
                val member = original.replace(Regex("^\\d+:\\d+:"), "").substringBefore("(").substringAfterLast(" ")
                if (synthetic(member)) continue
                current.first += member
                current.second += renamed
            }
        }
        for ((name, names) in members) {
            (names.second - names.first).filterNot(::synthetic).forEach { problems += "renamed in $name: $it" }
        }
        (packages - kept).forEach { problems += "no classes from $it reached R8" }

        check(problems.isEmpty()) { "R8 keep rules are incomplete:\n" + problems.joinToString("\n") }
    }
}
