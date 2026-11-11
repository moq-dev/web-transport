# [S] Share one Android platform-verifier initializer

## Goal

`rs/web-transport-ffi/src/android.rs`, `rs/web-transport-quinn/src/android.rs` and `rs/web-transport-noq/src/android.rs` each find the process `JavaVM` and the current `Application` to initialize rustls-platform-verifier. The lookup lives in one place, so a fix to it reaches every Android client.

## Plan

Each crate keeps its own policy: the quinn and noq builders return an error when initialization fails, while the FFI retries and falls back to bundled roots until it succeeds. Only the JNI lookup and initialization call are shared. Recommended home: `web-transport-quinn`'s helper, exposed only on Android and only to the FFI (which already depends on it). `web-transport-noq` cannot depend on quinn, so if no shared crate fits without adding one just for this, keep noq's copy and say why in the PR. Verify with clippy for `aarch64-linux-android` on all three crates, as [#512](https://github.com/moq-dev/web-transport/pull/512) did.
