# [L] Document every exported API and enforce it

## Goal

Every exported item in the Rust crates, the TypeScript packages and the handwritten Python wrapper has a doc comment, and `just check` fails when a new one lacks it.

## Plan

The 2026-11-22 docs audit counted undocumented exports with `cargo clippy --workspace --all-features -- -A clippy::all -W missing_docs`: 81 in web-transport-proto, 70 in web-transport-ffi, 52 in web-transport-iroh, 50 each in web-transport-noq and web-transport-quinn, 47 in web-transport-quiche, 25 in qmux, 7 in web-transport-wasm, and 6 each in web-transport, web-transport-trait and web-transport-node (which has no crate doc). Most are enum variants and struct fields.

- web-transport-ffi comes first: UniFFI copies its `///` docs into the generated Kotlin and Swift, which have no other API reference. Make its `pub` helpers that are not exports, such as the `map_*_error` functions, `pub(crate)` instead of documenting them.
- Enforce with `#![warn(missing_docs)]` in each crate, which `just check`'s clippy `-D warnings` turns into errors.
- TypeScript: `install`, `Session`, `SessionOptions`, `Request` and `Server` in `@moq/web-transport`, the default `Session` export in `@moq/qmux`, and `WebSocketStreamOptions` in `@moq/web-socket-stream` lack TSDoc. Biome has no missing-doc rule; pick a gate, such as TypeDoc's `validation.notDocumented` with warnings as errors.
- Python: 31 public methods and properties in `py/web-transport/python/web_transport` lack docstrings; ruff's `D1` rules can gate them.
- Coordinate noq and iroh with the open rind migrations ([#492](https://github.com/moq-dev/web-transport/pull/492), [#493](https://github.com/moq-dev/web-transport/pull/493)), which rewrite their sources.
