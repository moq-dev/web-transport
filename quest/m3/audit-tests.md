# [M] Audit test coverage

## Goal

Run the `tests` audit: `quest audit tests` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

- Baseline, 2026-11-22: Rust line coverage from `cargo llvm-cov --workspace --all-features --all-targets` (not committed; `nix shell nixpkgs#cargo-llvm-cov` plus a rust-overlay toolchain with the `llvm-tools-preview` extension, passed as `LLVM_COV`/`LLVM_PROFDATA`, since the dev shell's toolchain lacks it), excluding `tests/`, `benches/`, `examples/`, and test modules: qmux 93%, web-transport-proto 94%, web-transport-quinn 84%, web-transport-quiche 83%, web-transport-noq 76%, web-transport-trait 75%, web-transport-iroh 61%, web-transport 35%.
- That tool cannot see code exercised from other languages, so these figures understate it: web-transport-wasm (3%, run by the Chromium harness in `browser.yml`), web-transport-node (42%, run by `js/web-transport` and `node.yml`), and web-transport-ffi (49%, run by the Python, Kotlin, and Swift jobs). JS, Python, Kotlin, and Swift coverage was not measured.
- Check first: the trait-level stream wrappers in each backend, owned by [backend-conformance](/quest/m2/backend-conformance.md) if still open; every package with a `*.test.ts`, `tests/`, or `Tests/` directory has a `test` script or recipe that CI runs (`js/web-socket-stream` had none); and new `sleep`, `setTimeout`, or `asyncio.sleep` in tests.
- Real time stays on purpose in three places, each commented: quiche's keep-alive test (`rs/web-transport-quiche/tests/transport.rs`; quiche keeps its own clock), the TCP handshake-timeout test in `rs/qmux/tests/negotiation.rs`, and native idle timeouts in `js/web-transport/tests/fixtures/idle.ts` and the Python idle tests, where the QUIC timer must expire.
- Hand-copied contracts to recheck: the WebTransport error-code mapping in `rs/web-transport-proto/src/error.rs` against draft-ietf-webtrans-http3, and the QMux wire format against the JS port (`bun run --cwd js/qmux test:interop`).

## Audit

- tests

## Required

- [m2](/quest/m2/README.md) - the audit runs after it

## Scheduled

- 2026-11-29 - one week after the previous run
