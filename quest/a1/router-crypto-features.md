# [XS] Forward crypto-provider features through the router crate

## Goal

Native users of `web-transport` can build the router on `ring` instead of aws-lc-rs: the crate gains `aws-lc-rs` (default) and `ring` features that pass through to web-transport-quinn, and they do nothing on WASM.

## Plan

Decided with the maintainer on 2026-11-06 (planning interview):

- Forward `aws-lc-rs` and `ring` to `web-transport-quinn` with its default features off.
- With neither feature on, the native `ClientBuilder` disappears, mirroring web-transport-quinn's gate. Session and Server conversion still compile, so callers can bring their own rustls provider. No `compile_error!`.
- These features are interim. [Quinn on rind](/quest/a1/rind/quinn.md) removes them when the router moves to rind. Ship this anyway: the reporter gets a C-free option sooner, and the churn is accepted. If the rind switch lands first, delete this quest instead.
- Check that the WASM build still compiles with each feature combination.

Decision prompts (✅ chosen):

- Goal: ✅ forward features / fold into #135 quest / won't fix.
- Neither feature: ✅ gate ClientBuilder / `compile_error!`.
- After rind was planned: fold into rind / ✅ ship interim features.

## Closes

- [#477](https://github.com/moq-dev/web-transport/issues/477) - the forwarded features answer the request
