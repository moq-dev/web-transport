[![crates.io](https://img.shields.io/crates/v/web-transport-noq)](https://crates.io/crates/web-transport-noq)
[![docs.rs](https://img.shields.io/docsrs/web-transport-noq)](https://docs.rs/web-transport-noq)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport-noq
A wrapper around the Noq API (a Quinn fork), abstracting away the annoying HTTP/3 internals.
Provides a QUIC-like API but with web support!

## WebTransport
[WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) is a new web API that allows for low-level, bidirectional communication between a client and a server.
It's [available in the browser](https://caniuse.com/webtransport) as an alternative to HTTP and WebSockets.

WebTransport is layered on top of HTTP/3 which itself is layered on top of QUIC.
This library hides that detail and exposes only the QUIC API, delegating as much as possible to the underlying QUIC implementation (Noq, a Quinn fork).

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
Use `ClientBuilder` and `ServerBuilder` for WebTransport endpoints. Set the server's
certificate with `ServerBuilder::with_certificate`, and choose system roots or
certificate fingerprints when finishing the client builder.

```rust
# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = web_transport_noq::ClientBuilder::new().with_system_roots()?;
let session = client.connect(url::Url::parse("https://localhost:4433/")?).await?;
let (mut send, mut recv) = session.open_bi().await?;
send.write_all(b"hello").await?;
# Ok(())
# }
```

See the [examples](examples) for full client and server setup.

## Crypto and CPU support

TLS uses `rind-rustls` unless the process has already installed a rustls
`CryptoProvider`, which takes precedence. QUIC initial packets and endpoint reset
and handshake-token keys always use rind. The `aws-lc-rs` and `ring` features are
removed; `qlog` remains available. Normal and build dependencies need no C crypto.

Rind supports the release architectures x86_64, aarch64 and armv7. It requires a
minimum CPU instruction set described in [rind's platforms](https://github.com/moq-dev/rind/blob/main/PLATFORMS.md).
Builders remain infallible to create and return `ClientError::UnsupportedCpu` or
`ServerError::UnsupportedCpu` from their finishing methods when that CPU check
fails, including with an installed provider. `Client::default()` unwraps setup and
can panic; use `ClientBuilder` to handle errors.

Advanced callers still use Noq's `crypto::rustls` types and `HandshakeData`.
Pass `crypto::initial_suite()?` to `QuicClientConfig::with_initial` or
`QuicServerConfig::with_initial`, rather than `try_from`, which depends on Noq's
built-in providers. Set ALPN to `web_transport_noq::ALPN` (`h3`) for WebTransport.
Use `crypto::endpoint_config()` for fresh reset keys and `crypto::server_config()`
for fresh Retry/validation-token keys. These helpers are fallible, as are
`crypto::default_provider()` and `crypto::sha256()`.

```rust
# fn example(tls: rustls::ClientConfig) -> Result<noq::Endpoint, Box<dyn std::error::Error>> {
use std::sync::Arc;
use web_transport_noq::{crypto, noq};

let tls = noq::crypto::rustls::QuicClientConfig::with_initial(
    Arc::new(tls), crypto::initial_suite()?,
)?;
let client_config = noq::ClientConfig::new(Arc::new(tls));
let endpoint = noq::Endpoint::new(
    crypto::endpoint_config()?,
    None,
    std::net::UdpSocket::bind("0.0.0.0:0")?,
    Arc::new(noq::TokioRuntime),
)?;
endpoint.set_default_client_config(client_config);
# Ok(endpoint)
# }
```

For servers, wrap `QuicServerConfig::with_initial` in `crypto::server_config`,
then pass `Some(server_config)` to `Endpoint::new`. Bind sockets to match the
address families your application supports. Noq's `Endpoint::client`,
`Endpoint::server`, `EndpointConfig::default` and `ServerConfig::with_crypto`
require Noq's ring/aws-lc-rs features and are unavailable in this crate's normal
provider configuration. For persistent reset or token secrets, supply
`crypto::ResetKey` to `EndpointConfig::new` and `crypto::TokenKey` to
`ServerConfig::new`; each secret needs at least 32 bytes of entropy.

## API
The `web-transport-noq` API is almost identical to the Noq API, except that [Connection](https://docs.rs/noq/latest/noq/struct.Connection.html) is called [Session](https://docs.rs/web-transport-noq/latest/web_transport_noq/struct.Session.html).

When possible, `Deref` is used to expose the underlying Noq API.
However some of the API is wrapped or unavailable due to WebTransport limitations.
- Stream IDs are not available.
- Error codes are not full VarInts (62-bits) and significantly smaller.

Session clones share ownership of the connection. Dropping the final `Session`
closes it immediately, including raw QUIC sessions, even if streams or native
`noq::Connection` handles remain. Keep a `Session` clone while using those handles.
Call `close` explicitly to send a best-effort close capsule; its entire graceful
close is bounded by three round trips (at least 100 ms), including flow-controlled
writes, and continues if the final Session is dropped. The first close wins.

Cancelling the graceful-close task, including before its first poll or during
runtime shutdown, closes the native connection and requests waiter wakeups
synchronously, then cancels the capsule reader. The capsule carries the full
UTF-8 reason; the native fallback carries a prefix of at most 16 bytes at a character
boundary. Already observed native errors are preserved. Peer delivery, endpoint
draining and execution of awakened tasks require a live runtime. Forgotten
futures, non-yielding tasks and process abort are outside this guarantee.
