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

Use `ClientBuilder::new().with_system_roots()?` to build a reusable client,
then call `client.connect(url).await?` to establish a session. Clones share the
client's endpoints. The first connection to each address family binds an
IPv4 `0.0.0.0:0` or IPv6-only `[::]:0` socket. IPv4-mapped IPv6 destinations
use IPv4. Socket setup failures are returned by `connect`, so building a client
does not require IPv6 support or a running async runtime. Both sockets use the
runtime present at construction, or at the first connection if constructed
outside a runtime. Keep that runtime alive while using the client.

For an existing `quinn::ClientConfig`, `Client::from(config)` uses the same
lazy sockets. Set its ALPN to `h3`. `Client::new(endpoint, config)` instead keeps
the caller's endpoint and address handling.

`client.close(code, reason)` closes all bound endpoints and prevents new
connections through any clone. Await `client.wait_idle()` after closing to
wait for every bound endpoint to drain.

See the [examples](examples) for certificate setup and server construction.

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
The `web-transport-quinn` API is almost identical to the Quinn API, except that [Connection](https://docs.rs/quinn/latest/quinn/struct.Connection.html) is called [Session](https://docs.rs/web-transport-quinn/latest/web_transport_quinn/struct.Session.html).

When possible, `Deref` is used to expose the underlying Quinn API.
However some of the API is wrapped or unavailable due to WebTransport limitations.
- Stream IDs are not avaialble.
- Error codes are not full VarInts (62-bits) and significantly smaller.
