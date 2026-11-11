# [S] Verify system roots with the OS platform verifier

## Goal

`ClientBuilder::with_system_roots()` in web-transport-quinn and web-transport-noq verifies server certificates with rustls-platform-verifier, so the OS trust store decides, including custom enterprise roots and revocation where the OS supports them. The generic `web-transport` builder and the Node binding inherit this through web-transport-quinn. The method names stay the same; the PR reports the behavior change.

## Plan

Follow moq-dev/moq#1968, which made the same switch in moq-native. Replace the `rustls_native_certs` root loading in both builders with the platform verifier on the crate's existing provider (`crypto::default_provider()`), so rind stays the default provider. Drop the `rustls-native-certs` dependency from both crates. Also drop quinn's and noq's `platform-verifier` feature, which nothing uses: it only enables `ClientConfig::try_with_platform_verifier`, which bypasses this crate's provider-neutral crypto adapter.

The FFI client does not use these builders; it already verifies with the platform verifier and initializes it on Android from its own `JNI_OnLoad` (see `rs/web-transport-ffi/README.md`). Here, document in the builders that on Android the platform verifier requires app initialization.

Coordinate with [Noq on rind](/quest/a1/rind/noq.md), which rewrites the noq client builder; whichever lands second resolves the conflict. Update the crate READMEs and doc comments that describe `with_system_roots()`.

## Related

- [Issue #506](https://github.com/moq-dev/web-transport/issues/506) - platform verifier support; closed by the FFI platform verifier PR
