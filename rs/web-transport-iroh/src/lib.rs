//! WebTransport is a protocol for client-server communication over QUIC.
//!
//! It's [available in the browser](https://caniuse.com/webtransport) as an
//! alternative to HTTP and WebSockets.
//!
//! WebTransport is layered on top of HTTP/3 which is then layered on top of QUIC.
//!
//! This crate implements the [web-transport-trait] session and stream traits for iroh [connections] and streams.
//! A web-transport-iroh session can either use an iroh connection directly, or run HTTP/3 over the iroh connection.
//! The latter includes the WebTransport request-response handshake, through which a request target and headers can
//! be set.
//!
//! # Limitations
//!
//! WebTransport is able to be pooled with HTTP/3 and multiple WebTransport sessions.
//! This crate avoids that complexity, doing the bare minimum to support a single
//! WebTransport session that owns the entire QUIC connection.
//! If you want to support multiple WebTransport sessions over the same QUIC connection...
//! you should just dial a new QUIC connection instead.
//!
//! [web-transport-trait]: https://docs.rs/web-transport-trait/latest/web_transport_trait/
//! [iroh documentation]: https://docs.rs/iroh/latest/iroh/
//! [connections]: https://docs.rs/iroh/latest/iroh/endpoint/struct.Connection.html

mod client;
mod connect;
mod error;
mod recv;
mod send;
mod server;
mod session;
mod settings;
#[cfg(test)]
mod tests;

pub use client::*;
pub use connect::*;
pub use error::*;
pub use recv::*;
pub use send::*;
pub use server::*;
pub use session::*;
pub use settings::*;

/// The HTTP/3 ALPN is required when negotiating a QUIC connection.
pub const ALPN_H3: &str = "h3";

/// Re-export the http crate because it's in the public API.
pub use http;
/// Re-export iroh.
pub use iroh;
/// Re-export the WebTransport protocol implementation.
pub use web_transport_proto as proto;
/// Re-export the generic WebTransport implementation.
pub use web_transport_trait as generic;
