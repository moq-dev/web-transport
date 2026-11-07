# [M] Run Noq on rind

## Goal

web-transport-noq compiles no ring or aws-lc-rs: its builders run TLS on rind-rustls, and its `aws-lc-rs`/`ring` features are removed.

## Plan

Per rind's MIGRATING.md, noq 1.4's `rustls` feature works without `ring`. The client and server use `QuicClientConfig::with_initial` and `QuicServerConfig::with_initial`, which are public, with rind-rustls's TLS13_AES_128_GCM_SHA256 QUIC suite as the initial. `EndpointConfig::new` takes a reset `HmacKey` on rind's HMAC, and `ServerConfig::new` a `HandshakeTokenKey` on rind's HKDF and AES-256-GCM. noq-proto's Retry tag already uses pure-Rust aes-gcm.

Replace `crypto::default_provider()` with the installed process provider, else rind. A CPU rind refuses returns an `UnsupportedCpu` error from the finishing methods, as [the Quinn child](/quest/a1/rind/quinn.md) does: `rind::cpu::check()` runs before the HMAC and token keys are built, even with an installed provider, and `Client::default()` documents its panic. Keep test-only ring in dev-dependencies only if a test needs it. Test a handshake and Retry against a ring-backed noq peer.
