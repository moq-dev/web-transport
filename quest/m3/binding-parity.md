# [M] Bring the Kotlin and Swift bindings to parity with Python

## Goal

Kotlin and Swift users get the same defaults and enough documentation to use the bindings without reading the Python README or the Rust source.

## Plan

- `congestion_control` in `ClientConfig` and `ServerConfig` has no `#[uniffi(default)]`, so Kotlin and Swift callers must pass it while Python defaults it. Add the default.
- Python ships `generate_self_signed()` and `certificate_hash()`; Kotlin and Swift callers must compute a certificate's SHA-256 themselves for `serverCertificateHashes`. Export a hash helper from the FFI or document the platform call.
- The Kotlin and Swift READMEs have no usage example. Add one covering connect, both stream kinds, datagrams and close, plus how errors surface (`WebTransportException` in Kotlin, `WebTransportError` in Swift) and that `maxDatagramSize` can grow during a session, whereas Python caches the first value.
