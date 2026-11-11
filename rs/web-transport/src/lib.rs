//! A generic WebTransport interface.
//!
//! The underlying implementation switches based on the platform:
//!  - native: [web-transport-quinn](https://docs.rs/web-transport-quinn/latest)
//!  - web: [web-transport-wasm](https://docs.rs/web-transport-wasm/latest)
//!
//! WASM lacks server support, so for native you first establish a [web_transport_quinn::Session] and then use [Session::from()] to cast to this generic interface.
//!
//! # Platform-specific client setup
//!
//! Configure native-only options before converting a
//! [native client](https://docs.rs/web-transport-quinn/latest/web_transport_quinn/struct.Client.html)
//! into [`Client`].
//! The connection and application code can then use the platform router on both targets:
//!
//! ```no_run
//! fn client() -> Result<web_transport::Client, web_transport::Error> {
//!     #[cfg(any(not(target_arch = "wasm32"), target_os = "wasi"))]
//!     {
//!         // For trusted development networks only: this disables server authentication.
//!         let native = web_transport::quinn::ClientBuilder::new()
//!             .dangerous()
//!             .with_no_certificate_verification()?;
//!         Ok(web_transport::Client::from(native).with_protocols(["example"]))
//!     }
//!     #[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
//!     {
//!         web_transport::ClientBuilder::new()
//!             .with_protocols(["example"])
//!             .with_system_roots()
//!     }
//! }
//!
//! async fn connect(url: url::Url) -> Result<web_transport::Session, web_transport::Error> {
//!     client()?.connect(url).await
//! }
//! ```
//!
//! Browsers always authenticate the server. Use [`ClientBuilder::with_system_roots`]
//! for browser-managed trust or [`ClientBuilder::with_server_certificate_hashes`]
//! for certificate pinning subject to the browser's certificate restrictions.
//! On native targets, `with_system_roots` verifies with the operating system's
//! trust store.

#[cfg(any(not(target_arch = "wasm32"), target_os = "wasi"))]
#[path = "quinn.rs"]
mod quic;

#[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
#[path = "wasm.rs"]
mod quic;

pub use quic::*;
