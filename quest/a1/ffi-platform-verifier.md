# [M] Verify FFI clients with the OS platform verifier

## Goal

Python, Kotlin and Swift clients that use default verification (neither certificate hashes nor disabled verification) verify servers with the OS trust store on every platform, including iOS and Android. Today `rs/web-transport-ffi/src/client.rs` loads `rustls-native-certs`, which finds no roots on iOS and likely none on Android, so those clients cannot verify any server.

## Plan

Follow moq-dev/moq#1968. The FFI client verifies with rustls-platform-verifier on its existing provider, and `rustls-native-certs` is dropped. On Android, add a `JNI_OnLoad` that initializes the platform verifier from the application Context (moq uses `android.app.ActivityThread.currentApplication()`), clearing any pending JNI exception so loading the library never fails. Until initialization succeeds, Android falls back to bundled webpki-roots and logs a warning. The fallback is Android-only.

Update the Kotlin and Swift READMEs where they describe verification. Python, Kotlin and Swift share the `web-transport-ffi-v*` release.

## Closes

- [#506](https://github.com/moq-dev/web-transport/issues/506) - support for the platform verifier

## Related

- [Rust platform verifier](/quest/a1/platform-verifier.md) - the same switch in the Rust builders
