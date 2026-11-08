[![crates.io](https://img.shields.io/crates/v/web-transport)](https://crates.io/crates/web-transport)
[![docs.rs](https://img.shields.io/docsrs/web-transport)](https://docs.rs/web-transport)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport

[WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) is a new browser API powered by [QUIC](https://www.rfc-editor.org/rfc/rfc9000.html) intended as a replacement for WebSockets.
Most importantly, QUIC supports multiple independent data streams.

This crate provides a generic WebTransport implementation depending on the platform:

-   Native: [web-transport-quinn](../web-transport-quinn)
-   WASM: [web-transport-wasm](../web-transport-wasm)

WASM builds additionally require `--cfg=web_sys_unstable_apis`; see
[web-transport-wasm](../web-transport-wasm/README.md#requirements).

The native `ClientBuilder::with_transport_config` accepts Quinn's owned `TransportConfig`, available through `web_transport::quinn::quinn::TransportConfig`. Explicit congestion control overrides only its controller, regardless of setter order; see the [Quinn configuration documentation](../web-transport-quinn/README.md#transport-configuration) for defaults and examples.

The native `ClientBuilder` does not implement `Clone`. Construct a builder for each configuration, then clone the finished `Client` when sharing its endpoint and settings.


## Why no trait?

See [web-transport-trait](https://docs.rs/web-transport-trait). 

The biggest problem with async traits in Rust is `Send`.
WASM is `!Send` and as far as I can tell, it's not possible to implement a trait that both can support.
`web-transport-trait` requires `Send` which rules out WASM.

This crate skirts the issue by switching the underlying implementation based on the platform.
The compiler can then automatically apply `Send` bounds instead of explicitly requiring them.
Unfortunate, I know.

## Native crypto

The native Quinn backend uses rind through rind-rustls, with an installed process-wide
rustls provider taking precedence. Endpoint keys and Retry integrity always use rind.
Native builds support x86_64, aarch64 and 32-bit ARM; constructing a client or server
returns an error if the CPU lacks a required feature. This also applies with another
TLS provider installed. The native dependency graph no longer builds ring or aws-lc-rs.

WASM uses the browser's TLS and does not depend on rind. Advanced native callers use
[`web_transport_quinn::crypto`](../web-transport-quinn/README.md#crypto-and-migration).
