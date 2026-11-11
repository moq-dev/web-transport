# Choosing a package and backend

Start with the API boundary your application needs, then choose a transport.
The `web-transport` crate selects Quinn on native targets and the browser API on
non-WASI `wasm32` targets. Other native backends and QMux are separate packages; selecting them means
using their APIs directly or writing against `web-transport-trait`.

This guide describes the code in this checkout. The external comparison was
checked on 2026-11-07 against [wtransport 0.7.2][wt-manifest] and its public
reference documentation. Recheck the linked sources when changing versions.
The browser WASM paths below exclude WASI: the [router source][router-code] also
selects Quinn for WASI, whose build support is unverified here.
**Unsupported** means the cited API excludes a capability. **Unverified** means
these sources do not establish support; it does not mean the capability is
impossible or absent from every related project.

## Choose the Rust API boundary

| Package | Use it when | Boundary |
| --- | --- | --- |
| [`web-transport`](rs/web-transport) | You want one concrete client/session API for native Rust and browser WASM. | The [platform router][router-code] fixes the implementation at compile time. Configured native clients and servers come from Quinn, with conversions into the router's types. There is no browser server API. |
| [`web-transport-trait`](rs/web-transport-trait) | Your application should accept different session implementations. | The [async session/stream traits][traits-code] cover streams, datagrams, closure and metadata. They do not create endpoints or choose a backend. The [conditional bounds][trait-bounds] require `Send`/`Sync` outside `target_family = "wasm"` and relax them within that family, including WASI. |
| A native backend below | You need backend configuration or access to its connection and stream types. | Endpoint, TLS and runtime integration remain backend-specific. A shared trait does not make their builders interchangeable. |
| [`qmux`](rs/qmux) | You need the session abstraction over an ordered byte transport such as TCP, TLS or WebSocket. | Implements the async traits using QMux framing, with its own setup and wire protocol. Being retired; see [When to use QMux](#when-to-use-qmux). |
| [`web-transport-proto`](rs/web-transport-proto) | You are implementing protocol integration inside a backend. | Shared HTTP/3/WebTransport framing; it is not an application client or server. |

The async traits are implemented by Quinn, Noq, Quiche, iroh and QMux, and by
[the WASM adapter on WASM targets][wasm-traits]. The separate
[`poll` traits][poll-traits] serve callers that drive operations explicitly;
check each backend's implementations before depending on that surface.

## Choose a native backend

All four native backends expose client/server setup, bidirectional and
unidirectional streams, and datagram send/receive APIs. Choose based on the
networking stack you need to integrate, rather than assuming one is faster.

| Package | Integration surface | Where to start |
| --- | --- | --- |
| [`web-transport-quinn`](rs/web-transport-quinn) | Quinn endpoints, connections and streams. `Client::new`/`Server::new` accept configured Quinn objects; `Session::connect` and `Request::accept` work with an established connection. | [Client](rs/web-transport-quinn/src/client.rs), [server](rs/web-transport-quinn/src/server.rs), [session](rs/web-transport-quinn/src/session.rs), [examples](rs/web-transport-quinn/examples). The package README describes runtime selection and the hostname-resolution caveat outside Tokio. |
| [`web-transport-noq`](rs/web-transport-noq) | The corresponding Noq endpoint/connection API, using the Noq fork of Quinn. Choose it for an application already integrating Noq. | [Client](rs/web-transport-noq/src/client.rs), [server](rs/web-transport-noq/src/server.rs), [session](rs/web-transport-noq/src/session.rs), [examples](rs/web-transport-noq/examples). |
| [`web-transport-quiche`](rs/web-transport-quiche) | Quiche through `tokio-quiche` and the crate's `ez` layer. Builders accept sockets/settings; server integration also accepts a `tokio_quiche` listener and certificate resolver. | [Client](rs/web-transport-quiche/src/client.rs), [server](rs/web-transport-quiche/src/server.rs), [connection](rs/web-transport-quiche/src/connection.rs), [examples](rs/web-transport-quiche/examples). This is a Tokio integration, not a direct sans-I/O Quiche adapter. |
| [`web-transport-iroh`](rs/web-transport-iroh) | iroh connections in two modes: WebTransport over HTTP/3 (`Client::connect_h3`, `H3Request::accept`), or raw QUIC (`Client::connect_quic`, `QuicRequest::accept`, `Session::raw`) with the same session API but no HTTP/3 handshake. Raw sessions are not WebTransport-over-HTTP/3, so pick the mode your peer speaks. | [Client](rs/web-transport-iroh/src/client.rs), [server](rs/web-transport-iroh/src/server.rs), [session](rs/web-transport-iroh/src/session.rs). Verify browser reachability separately for an iroh deployment. |

The native HTTP/3 implementations use one WebTransport session per QUIC
connection. General HTTP/3 serving and multiple WebTransport sessions pooled on
one connection are unsupported by this design; see the
[Quinn](rs/web-transport-quinn/src/lib.rs),
[Noq](rs/web-transport-noq/src/lib.rs),
[Quiche](rs/web-transport-quiche/src/lib.rs) and
[iroh](rs/web-transport-iroh/src/lib.rs) limitations. Use a stack with HTTP/3
request routing if your application needs to share that connection with other
HTTP traffic.

### When to use QMux

[QMux](rs/qmux/README.md) provides multiplexed streams and datagrams over TCP,
Unix domain sockets, TLS and WebSockets, with feature-specific entry points.
It also accepts an application-provided transport. Use it when the peer speaks
QMux, including a WebSocket path for networks that block UDP. Your application
chooses that path; the Rust router does not automatically fall back to QMux.

QMux is a separate wire protocol from WebTransport over HTTP/3. Both ends must
agree on the QMux version and application protocol. Datagrams carried over a
reliable ordered connection still share its retransmission and head-of-line
blocking behavior; they do not acquire QUIC's independent packet delivery.
See the [QMux draft][qmux-draft] and the package's negotiation documentation.

The QMux packages here, [`qmux`](rs/qmux) and [`@moq/qmux`](js/qmux), receive
bug fixes only and will be retired in favour of the copy in
[moq-dev/moq][moq-qmux]. That copy implements moq-net's traits rather than
`web-transport-trait`; read its [migration guide][moq-qmux-migrate] before
starting a new QMux integration.

## Browser and language packages

| Environment | Package | Integration surface |
| --- | --- | --- |
| Rust in a browser | [`web-transport-wasm`](rs/web-transport-wasm), or the router | Wraps the browser's WebTransport API through `web-sys`; client only. The browser owns networking and TLS policy. Build with the [required unstable web-sys flags](rs/web-transport-wasm/README.md#requirements). |
| JavaScript in a browser | The browser's [WebTransport API][browser-api] | A browser client can connect to a native HTTP/3 WebTransport server. Browser interoperability does not require compiling the native server library to WASM. Check the target browser's support for the APIs you use. |
| Node.js | [`@moq/web-transport`](js/web-transport) | Custom client/server setup, followed by a WebTransport-shaped Web Streams/datagram/session API. Uses the [Quinn-backed NAPI-RS bridge](rs/web-transport-node/Cargo.toml), not the browser's network stack. |
| JavaScript over WebSocket | [`@moq/qmux`](js/qmux) | A browser polyfill plus server-side sessions over an accepted WebSocket. The host performs the HTTP upgrade and negotiates the subprotocol. Requires a QMux-compatible peer. Being retired; see [When to use QMux](#when-to-use-qmux). |
| Python | [`web-transport-rs`](py/web-transport) | Typed `asyncio` wrapper over UniFFI, with native client/server, streams and datagrams. |
| Kotlin / Android | [`dev.moq:web-transport`](kt) | UniFFI bindings packaged for JVM and Android. See its README for artifact and target requirements. |
| Swift | [`WebTransport`](swift) | Swift wrapper and generated UniFFI bindings, distributed through release archives. See its README for the supported installation path. |

Python, Kotlin and Swift share the [Quinn-backed `web-transport-ffi` core](rs/web-transport-ffi).
These packages do not select among the Rust backends or use QMux. Follow each
language's owning README for its available wrapper API, installation and examples;
a Rust method is not automatically available through every wrapper.

[`@moq/web-socket-stream`](js/web-socket-stream) is a lower-level WebSocket-to-Web
Streams adapter used by the JavaScript QMux package. It does not add multiplexed
streams or implement WebTransport by itself.

## Compared with wtransport

[wtransport][wt-readme] is another Rust WebTransport implementation. Its native
client and server use Quinn with Tokio, as shown by its [manifest][wt-manifest].
For a native Quinn application, both projects offer streams and datagrams;
compare the setup and ownership APIs your application will actually use.

| Capability | This workspace | wtransport 0.7.2 |
| --- | --- | --- |
| Native backend choice | Quinn, Noq, Quiche/Tokio and iroh packages, as linked above. The router uses Quinn. | Quinn/Tokio is verified. An alternative backend is unverified in the cited manifest and docs. |
| Browser interoperability | Native HTTP/3 servers plus the browser API; a dedicated Rust/WASM adapter is also provided. | The [browser integration guide][wt-browser] demonstrates JavaScript's native WebTransport connecting to a Rust server. A Rust adapter wrapping the browser API is unverified. |
| Streams | Native session APIs and the shared traits open/accept uni- and bidirectional streams, with read/write, finish, reset/stop and priority operations. The router offers a smaller concrete surface. | [`Connection`][wt-connection] opens/accepts both directions. [`SendStream`][wt-send] and [`RecvStream`][wt-recv] provide read/write, finish, reset/stop and Tokio I/O traits; send priority is exposed. |
| Datagrams | The native backends, router, WASM adapter and language wrappers expose send/receive operations; payload limits and async signatures depend on the surface. | [`Connection`][wt-connection] exposes `send_datagram`, `receive_datagram` and `max_datagram_size`. |
| Native configuration and underlying QUIC access | Use a backend directly for its endpoint/configuration and connection access; the router and bindings expose narrower APIs. | [Configuration builders][wt-config] expose TLS and transport settings. The [`quinn` feature][wt-manifest] exposes underlying Quinn configuration, connection and stream access. |
| Language bindings | Node.js through NAPI-RS; Python, Kotlin and Swift through UniFFI. | The [README][wt-readme] links external Elixir bindings. Support for the four languages listed here is unverified from those sources. |
| QMux / WebSocket transport | Separate Rust and JavaScript QMux packages, with explicit peer negotiation. | QMux integration is unverified in the cited README and manifest. |

The APIs are not drop-in replacements. For example, wtransport's stream-opening
operations return an opening future after the first await; its
[connection reference][wt-connection] explains both stages. Use each package's
own reference and examples for cancellation, ownership, errors and configuration.
No performance, reliability or production-readiness ranking is established here.

[router-code]: rs/web-transport/src/lib.rs
[traits-code]: rs/web-transport-trait/src/lib.rs
[trait-bounds]: rs/web-transport-trait/src/util.rs
[poll-traits]: rs/web-transport-trait/src/poll.rs
[wasm-traits]: rs/web-transport-wasm/src/traits.rs
[qmux-draft]: https://www.ietf.org/archive/id/draft-ietf-quic-qmux-02.html
[moq-qmux]: https://github.com/moq-dev/moq/tree/main/rs/qmux
[moq-qmux-migrate]: https://github.com/moq-dev/moq/blob/main/rs/qmux/MIGRATE.md
[browser-api]: https://www.w3.org/TR/webtransport/
[wt-manifest]: https://github.com/BiagioFesta/wtransport/blob/0.7.2/wtransport/Cargo.toml
[wt-readme]: https://github.com/BiagioFesta/wtransport/blob/0.7.2/README.md
[wt-browser]: https://github.com/BiagioFesta/wtransport/blob/0.7.2/README.md#browser-integration
[wt-connection]: https://docs.rs/wtransport/0.7.2/wtransport/connection/struct.Connection.html
[wt-config]: https://docs.rs/wtransport/0.7.2/wtransport/config/index.html
[wt-send]: https://docs.rs/wtransport/0.7.2/wtransport/stream/struct.SendStream.html
[wt-recv]: https://docs.rs/wtransport/0.7.2/wtransport/stream/struct.RecvStream.html
