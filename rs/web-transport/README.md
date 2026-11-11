[![crates.io](https://img.shields.io/crates/v/web-transport)](https://crates.io/crates/web-transport)
[![docs.rs](https://img.shields.io/docsrs/web-transport)](https://docs.rs/web-transport)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport

[WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) is a new browser API powered by [QUIC](https://www.rfc-editor.org/rfc/rfc9000.html) intended as a replacement for WebSockets.
Most importantly, QUIC supports multiple independent data streams.

This crate provides a generic WebTransport implementation depending on the platform:

-   Native: [web-transport-quinn](../web-transport-quinn)
-   Browser WASM (`wasm32`, excluding WASI): [web-transport-wasm](../web-transport-wasm)

WASM builds additionally require `--cfg=web_sys_unstable_apis`; see
[web-transport-wasm](../web-transport-wasm/README.md#requirements).

## Platform API and traits

This crate exposes concrete types selected at compile time for the target platform.
The backend crates also implement [web-transport-trait](../web-transport-trait)'s
async and poll-based session and stream traits. Its `MaybeSend` and `MaybeSync`
bounds require `Send` and `Sync` when `target_family` is not `wasm` and impose no
such requirements on the `wasm` target family, including browser and WASI targets.

Use the backend types through this crate's `web_transport::quinn` or
`web_transport::wasm` re-export when your code needs these trait implementations.
The wrapper types exported by this crate do not implement them.

## Configured native clients

On native targets, `Client::from(web_transport::quinn::Client)` wraps a configured
client, preserving its endpoints, TLS settings and transport configuration. This
matches the existing native `Server` and `Session` conversions. The converted
client initially offers no application subprotocols; use its native-only
`with_protocols` method to replace the offers. An empty iterator clears them.
Clones share the native client and keep independent protocol offers.

Split setup by target, then share `Client::connect` and application code:

```rust
fn client() -> Result<web_transport::Client, web_transport::Error> {
    #[cfg(any(not(target_arch = "wasm32"), target_os = "wasi"))]
    {
        // For trusted development networks only: this disables server authentication.
        let native = web_transport::quinn::ClientBuilder::new()
            .dangerous()
            .with_no_certificate_verification()?;
        Ok(web_transport::Client::from(native).with_protocols(["example"]))
    }
    #[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
    {
        web_transport::ClientBuilder::new()
            .with_protocols(["example"])
            .with_system_roots()
    }
}

async fn connect(url: url::Url) -> Result<web_transport::Session, web_transport::Error> {
    client()?.connect(url).await
}
```

Configure a native `quinn::Client` with its builder, use `quinn::Client::from` with
a `quinn::ClientConfig` to bind default per-family endpoints on first use, or use
`quinn::Client::new` with your own Quinn endpoint. Build custom TLS with the
`web_transport::quinn::crypto` adapters; it must offer `web_transport::quinn::ALPN`
(`h3`). Offered application subprotocols are
separate from ALPN; read the negotiated selection with `Session::protocol`.

Browsers always authenticate the server. `ClientBuilder::with_system_roots`
uses browser-managed certificate trust. `with_server_certificate_hashes` accepts
SHA-256 certificate pins, subject to the browser's requirements for certificate
lifetime and key type; see
[serverCertificateHashes](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport/WebTransport#servercertificatehashes).
Browsers provide no certificate-verification bypass.

## Native transport configuration

The native `ClientBuilder::with_transport_config` accepts Quinn's owned `TransportConfig`, available through `web_transport::quinn::quinn::TransportConfig`. Explicit congestion control overrides only its controller, regardless of setter order; see the [Quinn configuration documentation](../web-transport-quinn/README.md#transport-configuration) for defaults and examples.

The native `ClientBuilder` does not implement `Clone`. Construct a builder for each configuration, then clone the finished `Client` when sharing its endpoint and settings.

## Native session lifetime

Dropping the final `Session` clone closes the Quinn connection, even if streams or native handles remain. Keep a Session clone while using its streams. An explicit `Session::close` already in progress finishes its bounded graceful close after the final handle drops. Keep the runtime alive and await `Session::closed` to allow capsule delivery; a timeout or cancellation can prevent delivery.

## Native CONNECT diagnostics

On native builds, `Session::response()` exposes the CONNECT response and
ordinary headers. `Error::response()` exposes the complete rejection response:

```rust,ignore
match client.connect(url).await {
    Ok(session) => {
        if let Some(response) = session.response() {
            for value in response.headers.get_all("x-request-id") {
                eprintln!("request id: {value:?}");
            }
        }
    }
    Err(error) => {
        if let Some(response) = error.response() {
            eprintln!("CONNECT rejected: {}", response.status);
            for value in response.headers.get_all("www-authenticate") {
                eprintln!("challenge: {value:?}");
            }
        }
        return Err(error);
    }
}
```

These accessors are native-only. The WASM wrapper exposes what its browser
bindings provide and does not synthesize HTTP response or rejection headers.
The [W3C API](https://www.w3.org/TR/webtransport/) is evolving independently;
its draft response-header features do not imply support in this wrapper or in
every browser. Raw QUIC sessions have no CONNECT response.

## Native crypto

The native Quinn backend uses rind through rind-rustls, with an installed process-wide
rustls provider taking precedence. Endpoint keys and Retry integrity always use rind.
Native builds support x86_64, aarch64 and 32-bit ARM; constructing a client or server
returns an error if the CPU lacks a required feature. This also applies with another
TLS provider installed. The native dependency graph no longer builds ring or aws-lc-rs.

WASM uses the browser's TLS and does not depend on rind. Advanced native callers use
[`web_transport_quinn::crypto`](../web-transport-quinn/README.md#crypto-and-migration).
