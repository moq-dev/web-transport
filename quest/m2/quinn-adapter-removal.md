# [S] Replace the Quinn crypto adapter with upstream provider-neutral rustls

## Goal

web-transport-quinn uses quinn's own provider-neutral `rustls` support and deletes the local `web_transport_quinn::crypto` adapter.

## Plan

Bump the quinn pin in web-transport-quinn and web-transport-ffi together (FFI hands its own `quinn` types to `Client::new`) to the release that closes [quinn-rs/quinn#2341](https://github.com/quinn-rs/quinn/issues/2341) and delete the ported session. Remove the `web_transport_quinn::crypto` module outright as a breaking (`!`) change, with no re-exports or deprecation period, and give migration notes in the PR and changelog mapping each removed item (`QuicClientConfig`, `QuicServerConfig`, `HandshakeData`, `ResetKey`, `TokenKey`, `Provider`, `default_provider`, `endpoint_config`, `server_config`, `sha256`, and the error types) to its quinn or rustls replacement. Update the examples, tests, and FFI callers that use it. Keep the interop tests green, and the [rind epic](/quest/a1/rind/README.md)'s CI guard once it exists.

## Required

- [Quinn provider-neutral rustls release](/quest/m2/quinn-provider-release.md) - the upstream release this switches to
