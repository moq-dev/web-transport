# [S] Keep the Kotlin bindings working under R8

## Goal

An Android app that enables R8 minification can use `web-transport` without adding its own keep rules. `kt/web-transport/consumer-rules.pro` only keeps the rustls-platform-verifier classes, so R8 can strip or rename the JNA classes and the UniFFI-generated bindings that native code reaches by reflection.

## Plan

Add the consumer keep rules that JNA and UniFFI's Kotlin bindings document as required, scoped to the packages this library ships. Verify them with a minified R8 build of a small consumer in the existing opt-in Android build (`-Pandroid.enabled=true`), wired into the CI job that already builds the Android target if it can run there cheaply. Do not add an emulator: the bindings epic keeps mobile emulators out of scope (decided again 2026-11-11), so the check is that R8 succeeds and keeps the required classes, not a runtime handshake.
