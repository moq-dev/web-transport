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

## Configured native clients

On native targets, `Client::from(web_transport::quinn::Client)` wraps a configured
client, preserving its endpoint, TLS settings and transport configuration. This
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

Configure a native `quinn::Client` with its builder, or use `quinn::Client::new`
with your own Quinn endpoint and configuration. Custom TLS configurations must
offer `web_transport::quinn::ALPN` (`h3`). Offered application subprotocols are
separate from ALPN; read the negotiated selection with `Session::protocol`.

Browsers always authenticate the server. `ClientBuilder::with_system_roots`
uses browser-managed certificate trust. `with_server_certificate_hashes` accepts
SHA-256 certificate pins, subject to the browser's requirements for certificate
lifetime and key type; see
[serverCertificateHashes](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport/WebTransport#servercertificatehashes).
Browsers provide no certificate-verification bypass.


## Why no trait?

See [web-transport-trait](https://docs.rs/web-transport-trait). 

The biggest problem with async traits in Rust is `Send`.
WASM is `!Send` and as far as I can tell, it's not possible to implement a trait that both can support.
`web-transport-trait` requires `Send` which rules out WASM.

This crate skirts the issue by switching the underlying implementation based on the platform.
The compiler can then automatically apply `Send` bounds instead of explicitly requiring them.
Unfortunate, I know.
