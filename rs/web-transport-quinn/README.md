[![crates.io](https://img.shields.io/crates/v/web-transport-quinn)](https://crates.io/crates/web-transport-quinn)
[![docs.rs](https://img.shields.io/docsrs/web-transport-quinn)](https://docs.rs/web-transport-quinn)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport-quinn
A wrapper around the Quinn API, abstracting away the annoying HTTP/3 internals.
Provides a QUIC-like API but with web support!

See the [package and backend selection guide](../../PACKAGE-SELECTION.md) for alternatives and integration boundaries.

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

Use `ClientBuilder::new().with_system_roots()?` to build a reusable client,
then call `client.connect(url).await?` to establish a session. Clones share the
client's endpoints. The first connection to each address family binds an
IPv4 `0.0.0.0:0` or IPv6-only `[::]:0` socket. IPv4-mapped IPv6 destinations
use IPv4. Socket setup failures are returned by `connect`, so building a client
does not require IPv6 support or a running async runtime. Both sockets use the
runtime present at construction, or at the first connection if constructed
outside a runtime. Keep that runtime alive while using the client.

`with_system_roots()` verifies server certificates with
[rustls-platform-verifier](https://github.com/rustls/rustls-platform-verifier),
so the operating system's trust store decides, including enterprise-installed
roots and revocation where the platform supports it. On Android, the application
must [initialize the verifier](https://github.com/rustls/rustls-platform-verifier#android)
before the first handshake.

For an existing `quinn::ClientConfig`, `Client::from(config)` uses the same
lazy sockets. Set its ALPN to `web_transport_quinn::ALPN` (`h3`).
`Client::new(endpoint, config)` instead keeps the caller's endpoint and address
handling, and `Server::new(endpoint)` accepts a caller's server endpoint.

`client.close(code, reason)` closes all bound endpoints and prevents new
connections through any clone. Await `client.wait_idle()` after closing to
wait for every bound endpoint to drain.

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
Those Quinn convenience methods require its ring or aws-lc-rs feature.

```rust,no_run
# fn main() -> Result<(), Box<dyn std::error::Error>> {
use std::sync::Arc;
use web_transport_quinn::{crypto, quinn};

let provider = crypto::default_provider()?;
let verifier = rustls_platform_verifier::Verifier::new(provider.clone())?;

let mut tls = rustls::ClientConfig::builder_with_provider(provider)
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(verifier))
    .with_no_client_auth();
tls.alpn_protocols = vec![web_transport_quinn::ALPN.as_bytes().to_vec()];
let config = quinn::ClientConfig::new(Arc::new(crypto::QuicClientConfig::try_from(tls)?));
let client = web_transport_quinn::Client::from(config);
# Ok(())
# }
```

The adapter accepts any rustls provider exposing the QUIC AES-128-GCM-SHA256
initial suite. Configure TLS 1.3, and set server `max_early_data_size` to zero or
`u32::MAX`. `crypto::ResetKey` and `crypto::TokenKey` accept persistent secrets
when endpoints need keys that survive restarts. `HandshakeData` is a distinct
Rust type from Quinn's, so raw-session ALPN discovery requires this adapter.

See the [advanced examples](examples) for custom TLS and WebTransport setup.

## Session lifetime
Only `Session` clones keep the session alive. Dropping the final clone immediately closes the QUIC connection and cancels the CONNECT capsule reader, even when the peer keeps CONNECT open. Streams and native `quinn::Connection` handles do not extend this lifetime, including for `Session::raw`. Keep a Session clone while using its streams.

Calling `Session::close` first transfers teardown to the bounded graceful-close task, so dropping the final Session does not interrupt capsule delivery or replace the first close reason. Cancellation, including before the task's first poll, synchronously closes the native connection and requests reader cancellation without spawning another task. Runtime shutdown releases resources as futures are dropped; peer delivery, driver draining and execution of awakened futures still require a live runtime. Keep the runtime alive and await `Session::closed` to allow graceful close to finish; its timeout can prevent capsule delivery.

## Async runtimes
Each WebTransport session runs a background task that reads capsules from the CONNECT stream, and `Session::close` runs the graceful close in another.
Both are spawned on the runtime that [quinn::default_runtime](https://docs.rs/quinn/latest/quinn/fn.default_runtime.html) detects, the same way `quinn::Endpoint::client` and `quinn::Endpoint::server` pick theirs.
Tokio works out of the box. To run on async-std or smol instead, enable quinn's `runtime-async-std` or `runtime-smol` feature in your own `Cargo.toml`.
Resolving a hostname in `Client::connect` still uses tokio, so outside a tokio runtime connect to an IP address or resolve the name yourself.

## Transport configuration

`ClientBuilder::with_transport_config` and `ServerBuilder::with_transport_config` take an owned `quinn::TransportConfig`. When omitted, the builders use Quinn's defaults: a 30-second maximum idle timeout, no keepalive, and CUBIC congestion control. The transport configuration applies to every client certificate-verification path and to server connections.

```rust,no_run
use std::time::Duration;
use web_transport_quinn::{quinn, ClientBuilder, CongestionControl};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let mut transport = quinn::TransportConfig::default();
transport.max_idle_timeout(Some(Duration::from_secs(60).try_into()?));
transport.keep_alive_interval(Some(Duration::from_secs(10)));

let client = ClientBuilder::new()
    .with_transport_config(transport)
    .with_congestion_control(CongestionControl::LowLatency)
    .with_system_roots()?;
# Ok(())
# }
```

An explicit `LowLatency` or `Throughput` congestion choice overrides only the configuration's congestion controller, regardless of builder call order. `Default` removes that override and preserves the supplied controller. Calling `with_transport_config` again replaces the supplied configuration; other settings survive the congestion override.

Quinn's `TransportConfig` is not cloneable, so `ClientBuilder` owns it and does not implement `Clone`. Construct a builder for each configuration, then clone the finished `Client` to share its endpoints and settings across connections.

## API
`Session::closed` preserves the first close reason. A clean CONNECT-stream FIN without
a close capsule reports `WebTransportError::Closed(0, "")`; malformed or interrupted
capsules report `WebTransportError::CapsuleError` unless the underlying connection has
already reported its own error. An explicit local close reports `ConnectionError::LocallyClosed`.

`WebTransportError::CapsuleError(String)` adds a diagnostic variant to the public
enum; exhaustive matches must handle it. Dropping a receive stream sends
application code 0 in the session's code space, preserving raw QUIC code 0 for raw sessions.

The `web-transport-quinn` API is almost identical to the Quinn API, except that [Connection](https://docs.rs/quinn/latest/quinn/struct.Connection.html) is called [Session](https://docs.rs/web-transport-quinn/latest/web_transport_quinn/struct.Session.html).

When possible, `Deref` is used to expose the underlying Quinn API.
However some of the API is wrapped or unavailable due to WebTransport limitations.
- Stream IDs are not avaialble.
- Error codes are not full VarInts (62-bits) and significantly smaller.


## CONNECT response metadata

`Request::respond` and `Request::reject` accept a
[`proto::ConnectResponse`](../web-transport-proto/README.md#connect-response-headers)
with ordinary headers, including repeated values.
Inspect successful headers with `Session::response()`. A rejected CONNECT
is a structured `ConnectError::ErrorStatus(Box<ConnectResponse>)` error; inspect
`response.status` and `response.headers.get_all(name)`. The response stays inside
the backend's `ClientError`, with no string conversion needed. Raw QUIC has no
CONNECT response. See the protocol crate's migration notes for the breaking
error payload change and replacement of `ConnectResponse::OK` with
`ConnectResponse::default()`.
