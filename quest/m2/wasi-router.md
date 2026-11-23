# [XS] Reject WASI in the web-transport router

## Goal

Building `web-transport` for a `wasm32-wasi*` target fails with one clear error, and its docs stop presenting WASI as a native target.

## Plan

`rs/web-transport/src/lib.rs` selects the Quinn module for `any(not(target_arch = "wasm32"), target_os = "wasi")`, but `Cargo.toml` adds `web-transport-quinn` only for `not(target_arch = "wasm32")` and adds `web-transport-wasm` for WASI, so a WASI build fails on the unresolved `web_transport_quinn` crate. Recommendation: a `compile_error!` for WASI, with the router, its doctest and the README using plain `target_arch = "wasm32"` cfgs. Supporting WASI through Quinn needs a consumer first.
