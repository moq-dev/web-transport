[![crates.io](https://img.shields.io/crates/v/web-transport-iroh)](https://crates.io/crates/web-transport-iroh)
[![docs.rs](https://img.shields.io/docsrs/web-transport-iroh)](https://docs.rs/web-transport-iroh)

# web-transport-iroh

Run [WebTransport] over [iroh](https://github.com/n0-computer/iroh).

WebTransport is a protocol that layers QUIC semantics on top of HTTP/3, which itself is layered on top of QUIC.
This crate allows to perform an HTTP/3 handshake and express WebTransport semantics over iroh connections.
It implements the [`web-transport-trait`] traits for iroh connections and streams.

The crate was originally derived from [`web-transport-quinn`].

See the [package and backend selection guide](../../PACKAGE-SELECTION.md) for alternatives and integration boundaries.

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


## CONNECT response metadata

`H3Request::respond` and `H3Request::reject` accept a
[`proto::ConnectResponse`](../web-transport-proto/README.md#connect-response-headers)
with ordinary headers, including repeated values.
Inspect successful headers with `Session::response()`. A rejected CONNECT
is a structured `ConnectError::ErrorStatus(Box<ConnectResponse>)` error; inspect
`response.status` and `response.headers.get_all(name)`. The response stays inside
the backend's `ClientError`, with no string conversion needed. Raw QUIC has no
CONNECT response. See the protocol crate's migration notes for the breaking
error payload change and replacement of `ConnectResponse::OK` with
`ConnectResponse::default()`.
