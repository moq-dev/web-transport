[![crates.io](https://img.shields.io/crates/v/web-transport)](https://crates.io/crates/web-transport)
[![docs.rs](https://img.shields.io/docsrs/web-transport)](https://docs.rs/web-transport)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport

[WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) is a new browser API powered by [QUIC](https://www.rfc-editor.org/rfc/rfc9000.html) intended as a replacement for WebSockets.
Most importantly, QUIC supports multiple independent data streams.

This crate provides a generic WebTransport implementation depending on the platform:

-   Native: [web-transport-quinn](../web-transport-quinn)
-   WASM: [web-transport-wasm](../web-transport-wasm)


## Why no trait?

See [web-transport-trait](https://docs.rs/web-transport-trait). 

The biggest problem with async traits in Rust is `Send`.
WASM is `!Send` and as far as I can tell, it's not possible to implement a trait that both can support.
`web-transport-trait` requires `Send` which rules out WASM.

This crate skirts the issue by switching the underlying implementation based on the platform.
The compiler can then automatically apply `Send` bounds instead of explicitly requiring them.
Unfortunate, I know.
