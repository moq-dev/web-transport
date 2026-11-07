[![crates.io](https://img.shields.io/crates/v/web-transport-quinn)](https://crates.io/crates/web-transport-quinn)
[![docs.rs](https://img.shields.io/docsrs/web-transport-quinn)](https://docs.rs/web-transport-quinn)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport-quinn
A wrapper around the Quinn API, abstracting away the annoying HTTP/3 internals.
Provides a QUIC-like API but with web support!

## WebTransport
[WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) is a new web API that allows for low-level, bidirectional communication between a client and a server.
It's [available in the browser](https://caniuse.com/webtransport) as an alternative to HTTP and WebSockets.

WebTransport is layered on top of HTTP/3 which itself is layered on top of QUIC.
This library hides that detail and exposes only the QUIC API, delegating as much as possible to the underlying QUIC implementation (Quinn).

QUIC provides two primary APIs:

## Streams

QUIC streams are ordered, reliable, flow-controlled, and optionally bidirectional.
Both endpoints can create and close streams (including an error code) with no overhead.
You can think of them as TCP connections, but shared over a single QUIC connection.

## Datagrams

QUIC datagrams are unordered, unreliable, and not flow-controlled.
Both endpoints can send datagrams below the MTU size (~1.2kb minimum) and they might arrive out of order or not at all.
They are basically UDP packets, except they are encrypted and congestion controlled.

# Usage
Use `ClientBuilder` and `ServerBuilder` for WebTransport defaults. Advanced callers
can build a `quinn::Endpoint` and pass it to `Client::new` or `Server::new`; set the
TLS ALPN to `web_transport_quinn::ALPN` (`h3`).

## Crypto and migration

TLS uses the process-wide rustls provider if one is installed, otherwise
[rind-rustls](https://github.com/moq-dev/rind/tree/main/rind-rustls). Retry integrity,
stateless resets and handshake tokens always use rind. The normal and build
dependency graph contains no ring or aws-lc-rs; aws-lc-rs is a development dependency
for interop tests. The `ring` and `aws-lc-rs` crate features have been removed.

rind supports x86_64, aarch64 and 32-bit ARM. A CPU missing a required feature is
rejected by the builders' finishing methods with `ClientError::UnsupportedCpu` or
`ServerError::UnsupportedCpu`. `new()` stays infallible. This check also applies
when the process installed another TLS provider, because endpoint keys use rind.
`Client::default()` unwraps system-root setup and panics on an unsupported CPU;
use `ClientBuilder::new().with_system_roots()` to handle the error.

For manually configured endpoints, replace `quinn::crypto::rustls::QuicClientConfig`,
`QuicServerConfig` and `HandshakeData` with the same names under
`web_transport_quinn::crypto`. `crypto::default_provider()` and `crypto::sha256()` now return a `Result`.
Import rustls directly instead of through the feature-gated `quinn::rustls` re-export.
Use `crypto::server_config(Arc::new(crypto))?` instead of
`quinn::ServerConfig::with_crypto`, and `quinn::Endpoint::new` with
`crypto::endpoint_config()?` instead of `Endpoint::client` or `Endpoint::server`.
Those Quinn convenience methods require its ring or aws-lc-rs feature. For an IPv6
client socket, disable `IPV6_V6ONLY` before binding to retain Quinn's dual-stack
behavior; the advanced client example shows the socket2 setup.

```rust,no_run
# fn main() -> Result<(), Box<dyn std::error::Error>> {
use std::{net::UdpSocket, sync::Arc};
use web_transport_quinn::{crypto, quinn};

let mut tls = rustls::ClientConfig::builder_with_provider(crypto::default_provider()?)
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .with_root_certificates(rustls::RootCertStore::empty())
    .with_no_client_auth();
tls.alpn_protocols = vec![web_transport_quinn::ALPN.as_bytes().to_vec()];
let config = quinn::ClientConfig::new(Arc::new(crypto::QuicClientConfig::try_from(tls)?));
let endpoint = quinn::Endpoint::new(
    crypto::endpoint_config()?, None, UdpSocket::bind("0.0.0.0:0")?,
    quinn::default_runtime().ok_or("no async runtime")?,
)?;
let client = web_transport_quinn::Client::new(endpoint, config);
# Ok(())
# }
```

The adapter accepts any rustls provider exposing the QUIC AES-128-GCM-SHA256
initial suite. Configure TLS 1.3, and set server `max_early_data_size` to zero or
`u32::MAX`. `crypto::ResetKey` and `crypto::TokenKey` accept persistent secrets
when endpoints need keys that survive restarts. `HandshakeData` is a distinct
Rust type from Quinn's, so raw-session ALPN discovery requires this adapter.

See the [advanced examples](examples) for custom TLS and WebTransport setup.

## Async runtimes
Each WebTransport session runs a background task that reads capsules from the CONNECT stream, and `Session::close` runs the graceful close in another.
Both are spawned on the runtime that [quinn::default_runtime](https://docs.rs/quinn/latest/quinn/fn.default_runtime.html) detects, the same way `quinn::Endpoint::client` and `quinn::Endpoint::server` pick theirs.
Tokio works out of the box. To run on async-std or smol instead, enable quinn's `runtime-async-std` or `runtime-smol` feature in your own `Cargo.toml`.
Resolving a hostname in `Client::connect` still uses tokio, so outside a tokio runtime connect to an IP address or resolve the name yourself.

## API
The `web-transport-quinn` API is almost identical to the Quinn API, except that [Connection](https://docs.rs/quinn/latest/quinn/struct.Connection.html) is called [Session](https://docs.rs/web-transport-quinn/latest/web_transport_quinn/struct.Session.html).

When possible, `Deref` is used to expose the underlying Quinn API.
However some of the API is wrapped or unavailable due to WebTransport limitations.
- Stream IDs are not avaialble.
- Error codes are not full VarInts (62-bits) and significantly smaller.
