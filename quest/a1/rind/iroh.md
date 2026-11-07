# [S] Run iroh on rind

## Goal

web-transport-iroh compiles no ring or aws-lc-rs in what ships: iroh runs with `default-features = false` and rind-rustls's provider, and the crate's `aws-lc-rs`/`ring` features are removed.

## Plan

Per rind's MIGRATING.md, iroh 1.4 accepts a provider through `Endpoint::builder().crypto_provider(..)`. Pass the installed process provider, else rind. A CPU rind refuses returns an error instead of a panic. iroh-relay's `server` feature still enables ring; keep it out of this crate's normal graph, and let the epic's CI guard check that. Dev-dependencies may keep `tls-ring` for relay test fixtures only if a test needs one. Confirm with `cargo tree -e normal,build -i ring` across the crate's feature sets.
