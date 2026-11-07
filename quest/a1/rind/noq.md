# [M] Run Noq on rind

## Goal

web-transport-noq compiles no ring or aws-lc-rs: its builders run TLS on rind-rustls, and its `aws-lc-rs`/`ring` features are removed.

## Plan

Per rind's MIGRATING.md, noq 1.4's `rustls` feature works without `ring`. The client and server use `QuicClientConfig::with_initial` and `QuicServerConfig::with_initial`, which are public, with rind-rustls's TLS13_AES_128_GCM_SHA256 QUIC suite as the initial. `EndpointConfig::new` takes a reset `HmacKey` on rind's HMAC, and `ServerConfig::new` a `HandshakeTokenKey` on rind's HKDF and AES-256-GCM. noq-proto's Retry tag already uses pure-Rust aes-gcm.

Replace `crypto::default_provider()` with the installed process provider, else rind. A CPU rind refuses returns an `UnsupportedCpu` error from the finishing methods, as [the Quinn child](/quest/a1/rind/quinn.md) does: `rind::cpu::check()` runs before the HMAC and token keys are built, even with an installed provider, and `Client::default()` documents its panic. Keep test-only ring in dev-dependencies only if a test needs it. Test a handshake and Retry against a ring-backed noq peer.

## Waiting

- moq-dev/rind: publish a stable `rind-rustls` version implementing rustls `PacketKey::encrypt_in_place_for_path` and `decrypt_in_place_for_path`, including path 0. Noq 1.4 calls these for every packet; `rind-rustls` 0.23.0 and rind main `6dbdebd578aa2c36b7109261009084148a183aad` inherit rustls's error-returning defaults, so the migration compiles but Noq panics before handshake completion. Update the dependency to that stable release and verify `rind_packet_keys_support_noq_path_api` and `ring_peer_handshake_and_retry` pass, then run the full local checks and cross-vendor review. Keep the migration draft until this clears; do not duplicate packet cryptography locally.
