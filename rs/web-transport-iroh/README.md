[![crates.io](https://img.shields.io/crates/v/web-transport-iroh)](https://crates.io/crates/web-transport-iroh)
[![docs.rs](https://img.shields.io/docsrs/web-transport-iroh)](https://docs.rs/web-transport-iroh)

# web-transport-iroh

Run [WebTransport] over [iroh](https://github.com/n0-computer/iroh).

WebTransport is a protocol that layers QUIC semantics on top of HTTP/3, which itself is layered on top of QUIC.
This crate allows to perform an HTTP/3 handshake and express WebTransport semantics over iroh connections.
It implements the [`web-transport-trait`] traits for iroh connections and streams.

The crate was originally derived from [`web-transport-quinn`].

## TLS provider

Configure the iroh endpoint before passing it to `Client` or accepting connections:

```rust,no_run
use web_transport_iroh::{Client, crypto, iroh::{Endpoint, endpoint::presets}};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let endpoint = Endpoint::builder(presets::Empty)
    .crypto_provider(crypto::default_provider()?)
    .bind()
    .await?;
let client = Client::new(endpoint);
# client.close().await;
# Ok(())
# }
```

`crypto::default_provider()` uses the installed rustls provider first, otherwise
[rind-rustls](https://github.com/moq-dev/rind/tree/main/rind-rustls). It does not
install a process-wide provider. An unsupported CPU returns
`CryptoError::UnsupportedCpu` during provider selection, before binding the endpoint.
`Client::new()` still accepts an already configured endpoint and is infallible.

rind supports x86_64, aarch64 and 32-bit ARM; aarch64 requires NEON. Other
architectures, including WASM, cannot compile rind. An installed provider is used
without invoking rind.

The `aws-lc-rs` and `ring` features are removed. Remove them from your dependency
declaration and configure endpoints as above. iroh's `Minimal` and `N0` presets
require its built-in TLS provider features, so use `presets::Empty` and configure
relays and address lookup services as needed. The remaining features are
`metrics` and `portmapper` (both enabled by default), and optional `qlog`.
iroh's default features and iroh-relay's `server` feature remain disabled; no
test fixture needs a relay server or a separate crypto provider.

## License

Copyright 2025 N0, INC.

This project is licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

[WebTransport]: https://www.ietf.org/archive/id/draft-ietf-webtrans-overview-11.html
[`web-transport-quinn`]: https://github.com/moq-dev/web-transport/tree/main/rs/web-transport-quinn
[`web-transport-trait`]: https://github.com/moq-dev/web-transport/tree/main/rs/web-transport-trait
