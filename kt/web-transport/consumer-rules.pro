# rustls-platform-verifier calls its bundled Kotlin component through JNI,
# which shrinkers cannot see.
-keep, includedescriptorclasses class org.rustls.platformverifier.** { *; }

# JNA's native dispatch library looks up its own classes and members by name.
-dontwarn java.awt.**
-keep class com.sun.jna.** { *; }

# The UniFFI bindings bind native symbols to method names, lay out structures
# by field name, and invoke callbacks by reflection.
-keep, includedescriptorclasses class uniffi.web_transport.** { *; }
