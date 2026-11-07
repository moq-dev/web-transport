//! A generic WebTransport interface.
//!
//! The underlying implementation switches based on the platform:
//!  - native: [web-transport-quinn](https://docs.rs/web-transport-quinn/latest)
//!  - web: [web-transport-wasm](https://docs.rs/web-transport-wasm/latest)
//!
//! WASM lacks server support, so for native you first establish a [web_transport_quinn::Session] and then use [Session::from()] to cast to this generic interface.
//!
//! # Crypto providers
//!
//! On native targets, the `aws-lc-rs` feature (enabled by default) and `ring`
//! forward to `web-transport-quinn`. To use ring without aws-lc-rs, disable
//! default features and enable only `ring`.
//!
//! An installed process-wide rustls crypto provider takes precedence. With both
//! features enabled, install a provider with `CryptoProvider::install_default()`
//! before constructing a `ClientBuilder`; otherwise construction panics.
//! With neither feature enabled, the native `ClientBuilder` is unavailable.
//! `Session` and `Server` conversions remain available for callers that configure
//! `web-transport-quinn` themselves, for example through its `Client::new`.
//!
//! These features have no effect on WASM, where the browser handles TLS and
//! `ClientBuilder` is always available.

#[cfg(any(not(target_arch = "wasm32"), target_os = "wasi"))]
#[path = "quinn.rs"]
mod quic;

#[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
#[path = "wasm.rs"]
mod quic;

pub use quic::*;
