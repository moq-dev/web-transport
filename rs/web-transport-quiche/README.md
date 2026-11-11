[![crates.io](https://img.shields.io/crates/v/web-transport-quiche)](https://crates.io/crates/web-transport-quiche)
[![docs.rs](https://img.shields.io/docsrs/web-transport-quiche)](https://docs.rs/web-transport-quiche)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport-quiche
A wrapper around the Quiche, abstracting away the annoying API and HTTP/3 internals.
Provides a QUIC-like API but with web support!

See the [package and backend selection guide](../../PACKAGE-SELECTION.md) for alternatives and integration boundaries.

## Limitations
This library builds on top of [tokio-quiche](https://docs.rs/tokio-quiche/latest/tokio_quiche/); the "official" Tokio runtime for [quiche](https://github.com/cloudflare/quiche).

The [`ez` module](src/ez) wraps `tokio-quiche` with async and poll-based QUIC APIs.
The [`h3` module](src/h3) handles the HTTP/3 setup for WebTransport.
Certificates are configured through a BoringSSL hook because `tokio-quiche`'s
file-based configuration only accepts a single TLS certificate.

## WebTransport
[WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) is a new web API that allows for low-level, bidirectional communication between a client and a server.
It's [available in the browser](https://caniuse.com/webtransport) as an alternative to HTTP and WebSockets.

WebTransport is layered on top of HTTP/3 which itself is layered on top of QUIC.
This library hides that detail and exposes only the QUIC API, delegating as much as possible to the underlying QUIC implementation (quiche).

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
The default client resolves the remote before binding an ephemeral socket of
the same address family. IPv6 sockets are IPv6-only; IPv4-mapped IPv6 remotes
use IPv4. Explicit `with_bind` and `with_socket` inputs retain their address
handling.

## Session lifetime

`Connection` clones share ownership of a session. Dropping the final clone
requests immediate QUIC closure and cancels its capsule reader.
Retained streams or native `ez::Connection` handles do not keep the session alive.
Raw QUIC sessions use the same final-clone ownership boundary.

Call `Connection::close` before dropping to choose the close code and reason;
final drop preserves an explicit close already in progress. Await
`Connection::closed` when the application needs the driver to complete teardown.

## Send-rate statistics

`Connection::stats()` returns `ez::ConnectionStats`; its `send_rate` field and
`web_transport_trait::Stats::estimated_send_rate()` expose the same estimate in
bits per second. The driver snapshots Quiche's public `PathStats::delivery_rate`
and converts bytes per second with saturating multiplication by eight.
`None` means no path statistics have been sampled yet; `Some(0)` is possible
before a usable sample. The estimate is not a guarantee of link capacity or application throughput,
and it can remain stale while the application sends too little to measure capacity.

For the audited tokio-quiche 0.20.1 and Quiche 0.30.0 releases, both client and
server use the [default `cubic` controller][cc-default]. Pass `Settings` to either builder's
`with_settings` method to select another `cc_algorithm`:

| `Settings::cc_algorithm` | Meaning of the reported estimate |
| --- | --- |
| `cubic` (default), `reno` | Latest accepted delivery-rate sample. A sample marked app-limited may raise the rate but cannot lower it. There is no windowed maximum or expiry of the retained estimate. |
| `bbr`, `bbr2`, `bbr2_gcongestion` | All select BBR2. Its maximum-bandwidth filter rejects lower app-limited samples; the reported estimate is also capped by BBR2's congestion-derived `bandwidth_lo` when present. |

The client builder's `with_settings` replaces its defaults, and
`Settings::default()` leaves `verify_peer` false. When starting a client from
`Settings::default()`, set `verify_peer = true` or configure
`with_root_certificates` or `with_server_certificate_hashes`; otherwise the
client skips certificate verification.

Quiche's [controller selector][cc-selector] accepts those five names and rejects
others. The [CUBIC/Reno sampler][legacy-rate] already checks the app-limited flag
before replacing its stored rate, so this value is not an unfiltered raw sample.
Its per-sample flag is internal, and `PathStats::max_bandwidth` is `None` for
these controllers. The public statistics therefore cannot support a separate
application-side windowed filter that distinguishes app-limited samples.
Deriving `cwnd / rtt` would give a send-window heuristic rather than a measured
delivery rate, and would not recover that missing flag.

For BBR2, [the network model][bbr-model] reports
`min(max_bandwidth, bandwidth_lo)` when the congestion bound exists, and
`max_bandwidth` otherwise. Using the public `PathStats::max_bandwidth` instead
would discard that bound. Keeping `delivery_rate` preserves both upstream sample
filtering and the controller's response to congestion. Delivery-rate accuracy
also depends on respecting Quiche's pacing hints: disabling pacing or ignoring
those hints in the transport or platform can distort the estimate.

[cc-default]: https://github.com/cloudflare/quiche/blob/925b225825e2188f8e58e359114150500557bf61/tokio-quiche/src/settings/quic.rs
[cc-selector]: https://github.com/cloudflare/quiche/blob/be47c5011215b9f13bad06bd7627d3ae49888a19/quiche/src/recovery/mod.rs
[legacy-rate]: https://github.com/cloudflare/quiche/blob/be47c5011215b9f13bad06bd7627d3ae49888a19/quiche/src/recovery/congestion/delivery_rate.rs
[bbr-model]: https://github.com/cloudflare/quiche/blob/be47c5011215b9f13bad06bd7627d3ae49888a19/quiche/src/recovery/gcongestion/bbr2/network_model.rs

## Testing

Run `cargo test -p web-transport-quiche` for the unit tests and socket integration
coverage. Driver lifecycle tests in `src/ez/driver/tests.rs` use an in-memory QUIC
pair: packet delivery is explicit and separate from driver callbacks, so a test
can deliver `STOP_SENDING` before accepting a stream without a socket race or
sleep. Certificates are generated in memory; no fixture files are needed.

For a focused run, use `cargo test -p web-transport-quiche --lib ez::driver::tests`.
The pair pumps queued packets only; tests that need QUIC timer expiry require a
separate clock-driving seam.
