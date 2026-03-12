//! # Deprecated
//!
//! This crate has been superseded by the [`qmux`] crate, which supports
//! TCP, TLS, and WebSocket transports with both the legacy `webtransport`
//! wire format and the new QMux (draft-ietf-quic-qmux-00) protocol.
//!
//! All Rust types in this crate are thin wrappers or re-exports from `qmux`.
//! Please migrate to `qmux` directly.

mod client;
mod server;

#[deprecated(note = "use qmux::Session instead")]
pub type Session = qmux::Session;

#[deprecated(note = "use qmux::SendStream instead")]
pub type SendStream = qmux::SendStream;

#[deprecated(note = "use qmux::RecvStream instead")]
pub type RecvStream = qmux::RecvStream;

#[deprecated(note = "use qmux::Error instead")]
pub type Error = qmux::Error;

#[allow(deprecated)]
pub use client::Client;
#[allow(deprecated)]
pub use server::Server;
pub use tokio_tungstenite;
pub use tokio_tungstenite::tungstenite;

/// Legacy ALPN identifier.
#[deprecated(note = "use qmux::ALPNS instead")]
pub const ALPN: &str = "webtransport";
