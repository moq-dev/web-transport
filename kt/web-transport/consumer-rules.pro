# rustls-platform-verifier calls its bundled Kotlin component through JNI,
# which shrinkers cannot see.
-keep, includedescriptorclasses class org.rustls.platformverifier.** { *; }
