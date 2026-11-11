# [S] Run iroh on rind

## Goal

web-transport-iroh compiles no ring or aws-lc-rs in what ships: iroh runs with `default-features = false` and rind-rustls's provider, and the crate's `aws-lc-rs`/`ring` features are removed.

## Plan

Per rind's MIGRATING.md, iroh 1.4 accepts a provider through `Endpoint::builder().crypto_provider(..)`. Pass the installed process provider, else rind. A CPU rind refuses returns an error instead of a panic. iroh-relay's `server` feature still enables ring; keep it out of this crate's normal graph, and let the epic's CI guard check that. Dev-dependencies may keep `tls-ring` for relay test fixtures only if a test needs one. Confirm with `cargo tree -e normal,build -i ring` across the crate's feature sets.

The draft implementation selects the provider through `crypto::default_provider()`
because this crate accepts caller-created endpoints, with no endpoint builder of
its own. It removes both provider features and the unused dev-only iroh provider,
and migrates direct-connection fixtures to `presets::Empty` with an explicit
provider. Finish transport validation and CPU-refusal coverage before readiness.

## Waiting

- moq-dev/rind: publish a stable `rind-rustls` release implementing rustls QUIC `PacketKey::encrypt_in_place_for_path` and `decrypt_in_place_for_path`. Stable 0.23.0 lacks both; iroh 1.4.1 uses Noq 1.4.0, which calls the encryption method for path 0 and unwraps its default `EncryptError` at `noq-proto/src/crypto/rustls.rs:649`, aborting before a handshake. Reproduced by `cargo test -p web-transport-iroh --all-features tests::h3_smoke -- --exact --nocapture` on the draft migration. Once published, require the fixed version and rerun raw QUIC/H3 tests and repository checks. No local duplicate packet-suite adapter is approved.
- BLAKE3-team/BLAKE3 / n0-computer/iroh: provide a stable dependency path without BLAKE3's unconditional `cc` build dependency. `cargo tree -p web-transport-iroh -e normal,build --all-features -i cc` reaches `blake3 1.8.7` through both iroh 1.4.1 and iroh-relay 1.4.1. BLAKE3's `pure` feature does not remove that build dependency, so it cannot satisfy the rind epic's normal/build graph requirement. Recheck the inverse graph after an upstream change; do not silently narrow the epic's no-C-crypto goal to TLS providers.
