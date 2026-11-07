[![crates.io](https://img.shields.io/crates/v/web-transport)](https://crates.io/crates/web-transport)
[![docs.rs](https://img.shields.io/docsrs/web-transport)](https://docs.rs/web-transport)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport

[WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) is a new browser API powered by [QUIC](https://www.rfc-editor.org/rfc/rfc9000.html) intended as a replacement for WebSockets.
Most importantly, QUIC supports multiple independent data streams.

This crate provides a generic WebTransport implementation depending on the platform:

-   Native: [web-transport-quinn](../web-transport-quinn)
-   Browser WASM (`wasm32`, excluding WASI): [web-transport-wasm](../web-transport-wasm)

WASM builds additionally require `--cfg=web_sys_unstable_apis`; see
[web-transport-wasm](../web-transport-wasm/README.md#requirements).

## Platform API and traits

This crate exposes concrete types selected at compile time for the target platform.
The backend crates also implement [web-transport-trait](../web-transport-trait)'s
async and poll-based session and stream traits. Its `MaybeSend` and `MaybeSync`
bounds require `Send` and `Sync` when `target_family` is not `wasm` and impose no
such requirements on the `wasm` target family, including browser and WASI targets.

Use the backend types through this crate's `web_transport::quinn` or
`web_transport::wasm` re-export when your code needs these trait implementations.
The wrapper types exported by this crate do not implement them.
