# [L] Run Quinn, FFI, Node and the router on rind

## Goal

web-transport-quinn and everything built on it (web-transport-ffi, web-transport-node, and the `web-transport` router's native path) compile no ring or aws-lc-rs. Clients and servers run TLS on rind-rustls through a QUIC crypto adapter this crate exposes. Their `aws-lc-rs`/`ring` features are removed, including the router's interim ones.

## Plan

Decided with the maintainer on 2026-11-06 (planning interview):

- quinn-proto 0.11's `crypto::rustls` module compiles only with ring or aws-lc-rs ([quinn-rs/quinn#2341](https://github.com/quinn-rs/quinn/issues/2341)), and rind ships no quinn adapter (rind's decision, 2026-10-31). So this crate carries its own adapter. It was verified that `quinn` with `default-features = false` plus `runtime-tokio`, `bloom` and `platform-verifier`, next to rind-rustls, builds with no ring, aws-lc-sys or `cc`.
- A public `web_transport_quinn::crypto` module implements quinn-proto's public crypto traits over `rustls::quic`: the session, `QuicClientConfig`/`QuicServerConfig` equivalents, packet and header keys, the Retry integrity tag (AES-128-GCM), the reset `HmacKey`, and the `HandshakeTokenKey` (HKDF). Retry, HMAC and HKDF come from rind. The session accepts any rustls provider with QUIC suites. Endpoints are built with `Endpoint::new` and an `EndpointConfig` from this module, since `Endpoint::client`/`server` need ring.
- FFI and callers of `Client::new(endpoint, quinn::ClientConfig)` build their configs from this module. Document the migration from `quinn::crypto::rustls`.
- Builders default to the installed process provider, else rind. A CPU rind refuses returns a new `UnsupportedCpu` error from the finishing methods.
- Remove `aws-lc-rs`/`ring` from web-transport-quinn, web-transport-ffi, and the router (the features from [router crypto features](/quest/a1/router-crypto-features.md)). Check that py/kt/swift and Node still build and pass their binding CI.
- Test interop against an unmodified quinn peer on aws-lc-rs: handshake, Retry, stateless reset, key update, 0-RTT rejection, and a CPU-refusal path. Use controlled time where the protocol allows.
- The adapter is deleted once upstream allows any provider; see [Quinn adapter removal](/quest/a2/quinn-adapter-removal.md).

## Related

- [Native client composition](/quest/a1/native-client-config.md) - converts a configured Quinn client into the router
- [Dual-stack bind](/quest/a1/dual-stack-bind.md) - rebuilds the same builders' endpoints
