# [M] Audit the code

## Goal

Run the `code` audit: `quest audit code` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

- Baseline: the previous run read every crate, package, binding, and script at 2026-11-22.
- Rust suppressions are gated: workspace lints deny a bare `#[allow]` or one without a reason, so `cargo clippy` only shows `#[expect(..., reason)]`. Recheck those against `cargo hack check --feature-powerset`, since several (qmux `protocol`, noq crypto) depend on features. Android-only code (`rs/web-transport-ffi/src/android.rs`) and macOS-only code (qmux `socket.rs`) are not compiled by CI or the dev shell's toolchain, which has only the host and `wasm32` targets.
- Rustc and clippy miss unused `pub` items, so most dead code turns up by grepping each public name across the repo. Check first the crates that changed most since the baseline, then each `pub use` in a `lib.rs`. The NAPI surface in `rs/web-transport-node` is internal to `@moq/web-transport` (its `exports` map exposes only `.`), so unused NAPI exports are private dead code.
- No Python type checker or linter runs (Python lint CI was dropped in #249, `ruff`/`ty` in #569), so `# type: ignore` and `# noqa` comments are dead on arrival.
- Kept on purpose:
  - quinn's `quic_id`, `write_chunks`, `read_chunks`, `datagram_send_buffer_space`, and `send_datagram_wait` mirror Quinn's API for callers who asked for them (#93, #275).
  - `rs/web-transport-quinn/src/waiters.rs` and `rs/web-transport-quiche/src/waiters.rs` are byte-identical copies; sharing them needs a home in `kio`.
  - qmux's `recv_qmux00_frame` delimits draft-00 frames, which carry no length prefix, so it cannot reuse `Frame::decode`.
  - The Python error hierarchy is the wrapper's public API, not a copy of the Rust type.
  - `web-transport-node`'s `crate-type = ["cdylib", "lib"]` builds docs.rs.
- Not yet simplified, worth a second look: `qmux`'s `tls::Client` and `ws::Client` repeat the same protocol-list fields and strict-mode check; quiche's `Connection` keeps six HTTP/3-only fields as separate `Option`s and empty `Vec`s rather than one `Option<H3Session>`; qmux's session tests define `ScriptedTransport` three times.

## Audit

- code

## Required

- [m2](/quest/m2/README.md) - the audit runs after it

## Scheduled

- 2026-11-29 - one week after the previous run
